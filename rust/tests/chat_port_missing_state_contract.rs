//! A deployment with no ChatPort and no built-in connection must stay alive but silent.
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const BIN: &str = env!("CARGO_BIN_EXE_qq-inner-core");

struct Root(PathBuf);
impl Root {
    fn new(ws_url: &str, model_url: &str, disabled_manifest: bool) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("chat-port-missing-{}", rand::random::<u64>()));
        fs::create_dir_all(&path)?;
        if disabled_manifest {
            fs::create_dir_all(path.join("plugins/onebot"))?;
            fs::write(path.join("plugins/onebot/manifest.json"),
                include_str!("../../plugins/onebot/manifest.json"))?;
            fs::write(path.join("plugins/plugins.json"), serde_json::to_vec_pretty(&json!({
                "schema_version":1,"mode":"manual","plugins":[{
                    "id":"onebot","directory":"onebot","enabled":false,
                    "params":{},"bindings":{},"secret_refs":{}
                }]
            }))?)?;
        }
        fs::write(path.join("config.json"), serde_json::to_vec_pretty(&json!({
            "storage":{"directory":"state"},
            "provider":{"kind":"openai","baseUrl":model_url,"model":"fixture"},
            "onebot":{"url":ws_url,"selfId":"99","reconnectMaxSeconds":1},
            "agent":{"allowedGroups":["10"],"observation":{"enabled":false},
                "identity":{"enabled":false},"backfill":{"enabled":false},
                "schedule":{"enabled":false},"rhythm":{"enabled":false},
                "learning":{"enabled":false},"threeLayerDecision":false,
                "sending":{"enabled":false},"debounceSeconds":0,
                "minThinkIntervalSeconds":1,"activeWindowSeconds":600,
                "maxMessagesPerHour":100,"quietHours":null}
        }))?)?;
        fs::write(path.join("secrets.json"), br#"{"apiKey":"fixture-key"}"#)?;
        Ok(Self(path))
    }
    fn state(&self) -> PathBuf { self.0.join("state") }
    fn status(&self) -> PathBuf { self.state().join("status.json") }
    fn log(&self) -> PathBuf { self.state().join("agent.log") }
    fn db(&self) -> PathBuf { self.state().join("agent.sqlite") }
}
impl Drop for Root { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

struct Agent(Child);
impl Agent {
    fn start(root: &Root) -> Result<Self> {
        Ok(Self(Command::new(BIN).arg("--root").arg(&root.0).arg("start")
            .stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?))
    }
    fn check_running(&mut self) -> Result<()> {
        if let Some(status) = self.0.try_wait()? {
            let mut stderr = String::new();
            if let Some(pipe) = self.0.stderr.as_mut() { pipe.read_to_string(&mut stderr)?; }
            anyhow::bail!("agent exited before observable state: {status}; {stderr}");
        }
        Ok(())
    }
    fn control_state(&mut self, root: &Root) -> Result<Value> {
        self.check_running()?;
        let mut socket = UnixStream::connect(root.state().join("control.sock"))?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        writeln!(socket, "{}", json!({"id":"missing-io-contract","method":"state.get","params":{}}))?;
        let mut reader = BufReader::new(socket);
        loop {
            let mut line = String::new();
            anyhow::ensure!(reader.read_line(&mut line)? > 0, "control socket closed");
            let reply: Value = serde_json::from_str(&line)?;
            if reply["id"] == "missing-io-contract" {
                anyhow::ensure!(reply["ok"] == true, "state.get failed: {reply}");
                return Ok(reply["result"].clone());
            }
        }
    }
}
impl Drop for Agent { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }

struct Wire {
    url: String,
    peers: Arc<Mutex<Vec<mpsc::UnboundedSender<Message>>>>,
    sends: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Wire {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/", listener.local_addr()?);
        let peers = Arc::new(Mutex::new(Vec::new()));
        let sends = Arc::new(AtomicUsize::new(0));
        let saved_peers = peers.clone();
        let saved_sends = sends.clone();
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let peers = saved_peers.clone();
                let sends = saved_sends.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = accept_async(socket).await else { return; };
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    peers.lock().unwrap().push(tx);
                    loop {
                        tokio::select! {
                            outgoing = rx.recv() => {
                                let Some(outgoing) = outgoing else { break; };
                                if ws.send(outgoing).await.is_err() { break; }
                            }
                            incoming = ws.next() => {
                                let Some(Ok(Message::Text(frame))) = incoming else { break; };
                                let Ok(request) = serde_json::from_str::<Value>(&frame) else { continue; };
                                let data = match request["action"].as_str().unwrap_or("") {
                                    "get_login_info" => json!({"user_id":99}),
                                    "get_status" => json!({"online":true}),
                                    "send_group_msg" => {
                                        sends.fetch_add(1, Ordering::SeqCst);
                                        json!({"message_id":501})
                                    }
                                    _ => json!({}),
                                };
                                let reply = json!({"echo":request["echo"],"status":"ok","retcode":0,"data":data});
                                if ws.send(Message::Text(reply.to_string())).await.is_err() { break; }
                            }
                        }
                    }
                });
            }
        });
        Ok(Self { url, peers, sends, task })
    }
    fn broadcast(&self, frame: Value) -> Result<()> {
        for peer in self.peers.lock().unwrap().iter() { peer.send(Message::Text(frame.to_string()))?; }
        Ok(())
    }
    fn has_connection(&self) -> bool { !self.peers.lock().unwrap().is_empty() }
}
impl Drop for Wire { fn drop(&mut self) { self.task.abort(); } }

struct Model {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Model {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/v1", listener.local_addr()?);
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move { let _ = serve_model(socket).await; });
            }
        });
        Ok(Self { url, task })
    }
}
impl Drop for Model { fn drop(&mut self) { self.task.abort(); } }
async fn serve_model(mut socket: TcpStream) -> Result<()> {
    let mut bytes = [0u8; 4096];
    let _ = socket.read(&mut bytes).await?;
    let body = r#"{"choices":[{"message":{"content":"{}"},"finish_reason":"stop"}]}"#;
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    Ok(())
}

fn status(root: &Root) -> Result<Value> { Ok(serde_json::from_slice(&fs::read(root.status())?)?) }
fn chat(status: &Value) -> Result<&Value> {
    let all = &status["interfaceAvailability"];
    let entry = if let Some(entries) = all.as_array() {
        entries.iter().find(|entry| entry["kind"] == "chat")
    } else { all.get("chat") };
    entry.context("chat interface availability must be queryable")
}
fn logs(root: &Root, event: &str) -> Result<Vec<Value>> {
    Ok(fs::read_to_string(root.log()).unwrap_or_default().lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["event"] == event).collect())
}
fn count(root: &Root, table: &str) -> Result<i64> {
    let sql = match table {
        "deliveries" => "SELECT count(*) FROM deliveries",
        "outbound_message_facts" => "SELECT count(*) FROM outbound_message_facts",
        _ => anyhow::bail!("unexpected table"),
    };
    Ok(Connection::open(root.db())?.query_row(sql, [], |row| row.get(0))?)
}
async fn wait_for(mut check: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if check()? { return Ok(()); }
        anyhow::ensure!(Instant::now() < deadline, "runtime condition timed out");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
async fn start(disabled_manifest: bool) -> Result<(Root, Agent, Wire, Model)> {
    let wire = Wire::start().await?;
    let model = Model::start().await?;
    let root = Root::new(&wire.url, &model.url, disabled_manifest)?;
    let mut agent = Agent::start(&root)?;
    wait_for(|| {
        agent.check_running()?;
        Ok(status(&root).is_ok() && root.state().join("control.sock").exists())
    }).await?;
    Ok((root, agent, wire, model))
}
fn addressed_message() -> Value {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    json!({"post_type":"message","message_type":"group","self_id":99,"group_id":10,
        "user_id":20,"message_id":9104,"time":now,"sender":{"nickname":"Human"},
        "message":"[CQ:at,qq=99]你好","raw_message":"[CQ:at,qq=99]你好"})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "§3.13: awaits removal of the built-in connection"]
async fn missing_chat_io_is_reported_as_missing() -> Result<()> {
    let (root, mut agent, _wire, _model) = start(false).await?;
    assert!(!root.0.join("plugins/plugins.json").exists());
    let snapshot = status(&root)?;
    let entry = chat(&snapshot)?;
    assert_eq!(entry["route"], "none");
    assert_eq!(entry["health"], "missing");
    agent.check_running()?;
    assert!(agent.control_state(&root)?.is_object(), "control socket must still respond");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "§3.13: awaits removal of the built-in connection"]
async fn missing_chat_io_is_announced_once() -> Result<()> {
    let (root, mut agent, _wire, _model) = start(false).await?;
    wait_for(|| Ok(!logs(&root, "no_available_io")?.is_empty())).await?;
    let first = logs(&root, "no_available_io")?;
    assert_eq!(first.len(), 1, "one missing state must be announced once");
    assert_eq!(first[0]["kind"], "chat");
    assert!(first[0]["reason"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(["source", "ingressSource", "route"].iter().any(|key|
        first[0][key].as_str().is_some_and(|s| !s.is_empty())),
        "the alarm must identify the missing I/O source");
    let modified = fs::metadata(root.status())?.modified()?;
    wait_for(|| {
        agent.check_running()?;
        Ok(fs::metadata(root.status())?.modified()? > modified)
    }).await?;
    assert_eq!(logs(&root, "no_available_io")?.len(), 1,
        "periodic status writes must not repeat the same alarm");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "§3.13: awaits removal of the built-in connection"]
async fn missing_chat_io_stops_participation_and_sending() -> Result<()> {
    let (root, mut agent, wire, _model) = start(false).await?;
    wait_for(|| Ok(chat(&status(&root)?)?["route"] == "none" || wire.has_connection())).await?;
    wire.broadcast(addressed_message())?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snapshot = status(&root)?;
    let effective = snapshot["participation"]["effective"].as_str()
        .context("participation.effective must be queryable")?;
    let deliveries = count(&root, "deliveries")?;
    let outbound = count(&root, "outbound_message_facts")?;
    let sends = wire.sends.load(Ordering::SeqCst);
    assert!(effective != "normal" && deliveries == 0 && outbound == 0 && sends == 0,
        "missing Chat I/O must stop participation and sending: effective={effective}, deliveries={deliveries}, outbound={outbound}, sends={sends}");
    agent.check_running()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_plugin_with_builtin_is_not_missing() -> Result<()> {
    let (root, _agent, _wire, _model) = start(true).await?;
    wait_for(|| Ok(status(&root)?["onebotConnected"] == true)).await?;
    let snapshot = status(&root)?;
    let entry = chat(&snapshot)?;
    assert_eq!(entry["route"], "built_in");
    assert_ne!(entry["health"], "missing");
    assert!(logs(&root, "no_available_io")?.is_empty(), "live built-in fallback is not missing");
    Ok(())
}
