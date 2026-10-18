//! Runtime acceptance: the owned ChatPort and the diagnostic OneBot socket see
//! the same wire frames, but only one source may enter the engine.
use anyhow::{Context, Result};
use futures_util::{future::BoxFuture, SinkExt, StreamExt};
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        io::{Capabilities, ChatPort, Command, CommandResult, IoError, PortEvent, PortKind,
            PortManifest, PortReply, PortState, TransportKind, VersionReq},
        orientation::{OrientationProvider, OrientationTransport}, Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command as Process, Stdio},
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const BIN: &str = env!("CARGO_BIN_EXE_qq-inner-core");
const CHAT: &str = "group:10";

struct Root(PathBuf);
impl Root {
    fn new(ws_url: &str, model_url: &str, enabled: bool) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("chat-port-runtime-{}", rand::random::<u64>()));
        fs::create_dir_all(path.join("plugins/onebot"))?;
        fs::write(path.join("plugins/onebot/manifest.json"),
            include_str!("../../plugins/onebot/manifest.json"))?;
        fs::write(path.join("plugins/plugins.json"), serde_json::to_vec_pretty(&json!({
            "schema_version":1,"mode":"manual","plugins":[{
                "id":"onebot","directory":"onebot","enabled":enabled,
                "params":{},"bindings":{},"secret_refs":{}
            }]
        }))?)?;
        fs::write(path.join("config.json"), serde_json::to_vec_pretty(&json!({
            "storage":{"directory":"state"},
            "provider":{"kind":"openai","baseUrl":model_url,"model":"fixture"},
            "onebot":{"url":ws_url,"selfId":"99","reconnectMaxSeconds":1},
            "agent":{"allowedGroups":["10"],"observation":{"enabled":false},
                "identity":{"enabled":false},"schedule":{"enabled":false},
                "rhythm":{"enabled":false},"learning":{"enabled":false},
                "threeLayerDecision":false,"sending":{"enabled":false},
                "debounceSeconds":0,"minThinkIntervalSeconds":0,
                "addressedProbability":1,"maxMessagesPerHour":100,
                "activeWindowSeconds":600,"quietHours":null}
        }))?)?;
        fs::write(path.join("secrets.json"), br#"{"apiKey":"fixture-key"}"#)?;
        Ok(Self(path))
    }
    fn state(&self) -> PathBuf { self.0.join("state") }
    fn db(&self) -> PathBuf { self.state().join("agent.sqlite") }
    fn log(&self) -> PathBuf { self.state().join("agent.log") }
}
impl Drop for Root {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

struct Agent(Child);
impl Agent {
    fn start(root: &Root) -> Result<Self> {
        Ok(Self(Process::new(BIN).arg("--root").arg(&root.0).arg("start")
            .stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?))
    }
    fn state(&mut self, root: &Root) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(8);
        let socket = root.state().join("control.sock");
        let mut stream = loop {
            if let Some(status) = self.0.try_wait()? {
                let mut stderr = String::new();
                if let Some(pipe) = self.0.stderr.as_mut() { pipe.read_to_string(&mut stderr)?; }
                anyhow::bail!("agent exited before control socket: {status}; {stderr}");
            }
            if let Ok(stream) = UnixStream::connect(&socket) { break stream; }
            anyhow::ensure!(Instant::now() < deadline, "control socket did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        };
        writeln!(stream, "{}", json!({"id":"runtime-contract","method":"state.get","params":{}}))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            anyhow::ensure!(reader.read_line(&mut line)? > 0, "control socket closed");
            let reply: Value = serde_json::from_str(&line)?;
            if reply["id"] == "runtime-contract" {
                anyhow::ensure!(reply["ok"] == true, "state.get failed: {reply}");
                return Ok(reply["result"].clone());
            }
        }
    }
}
impl Drop for Agent {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

struct Wire {
    url: String,
    peers: Arc<Mutex<Vec<Option<mpsc::UnboundedSender<Message>>>>>,
    actions: Arc<Mutex<Vec<(usize, Value)>>>,
    reject_new: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}
impl Wire {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/", listener.local_addr()?);
        let peers = Arc::new(Mutex::new(Vec::new()));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let reject_new = Arc::new(AtomicBool::new(false));
        let saved_peers = peers.clone();
        let saved_actions = actions.clone();
        let rejecting = reject_new.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if rejecting.load(Ordering::SeqCst) { drop(stream); continue; }
                let peers = saved_peers.clone();
                let actions = saved_actions.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = accept_async(stream).await else { return; };
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    let index = {
                        let mut peers = peers.lock().unwrap();
                        let index = peers.len();
                        peers.push(Some(tx));
                        index
                    };
                    loop {
                        tokio::select! {
                            outgoing = rx.recv() => {
                                let Some(outgoing) = outgoing else { break; };
                                let close = matches!(outgoing, Message::Close(_));
                                if ws.send(outgoing).await.is_err() || close { break; }
                            }
                            incoming = ws.next() => {
                                let Some(Ok(Message::Text(frame))) = incoming else { break; };
                                let Ok(request) = serde_json::from_str::<Value>(&frame) else { continue; };
                                let action = request["action"].as_str().unwrap_or("");
                                let data = match action {
                                    "get_login_info" => json!({"user_id":99}),
                                    "get_status" => json!({"online":true}),
                                    "send_group_msg" => {
                                        actions.lock().unwrap().push((index, request.clone()));
                                        json!({"message_id":501})
                                    }
                                    _ => json!({}),
                                };
                                let ack = json!({"echo":request["echo"],"status":"ok","retcode":0,"data":data});
                                if ws.send(Message::Text(ack.to_string())).await.is_err() { break; }
                            }
                        }
                    }
                    peers.lock().unwrap()[index] = None;
                });
            }
        });
        Ok(Self { url, peers, actions, reject_new, task })
    }
    fn count(&self) -> usize { self.peers.lock().unwrap().iter().filter(|peer| peer.is_some()).count() }
    fn send(&self, index: usize, frame: Value) -> Result<()> {
        let peers = self.peers.lock().unwrap();
        let peer = peers.get(index).and_then(Option::as_ref).context("WS connection missing")?;
        peer.send(Message::Text(frame.to_string()))?;
        Ok(())
    }
    fn broadcast(&self, frame: Value) -> Result<()> {
        let peers = self.peers.lock().unwrap();
        for peer in peers.iter().flatten() { peer.send(Message::Text(frame.to_string()))?; }
        Ok(())
    }
    fn close_first_and_reject_reconnects(&self) -> Result<()> {
        self.reject_new.store(true, Ordering::SeqCst);
        let peers = self.peers.lock().unwrap();
        peers.first().and_then(Option::as_ref).context("first WS connection missing")?
            .send(Message::Close(None))?;
        Ok(())
    }
}
impl Drop for Wire {
    fn drop(&mut self) { self.task.abort(); }
}

struct Model {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Model {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/v1", listener.local_addr()?);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move { let _ = serve_model(stream).await; });
            }
        });
        Ok(Self { url, task })
    }
}
impl Drop for Model { fn drop(&mut self) { self.task.abort(); } }

async fn serve_model(mut stream: TcpStream) -> Result<()> {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "HTTP request ended before headers");
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(pos) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
        anyhow::ensure!(bytes.len() < 64_000, "HTTP request headers too large");
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let size = headers.lines().find_map(|line| line.to_ascii_lowercase()
        .strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok()))
        .unwrap_or(0);
    anyhow::ensure!(size <= 1_000_000, "HTTP request body too large");
    while bytes.len() - header_end < size {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "HTTP request ended before body");
        bytes.extend_from_slice(&chunk[..n]);
    }
    let request: Value = serde_json::from_slice(&bytes[header_end..header_end + size])?;
    let body = request.to_string();
    let answer = if body.contains("TASK: FORM") {
        json!({"candidates":[{"kind":"system2","text":"回应你好"}],"allocation":"self"})
    } else if body.contains("TASK: EVALUATE") || body.contains("TASK: EVALUATION") {
        let id = request["messages"].as_array().and_then(|messages| messages.last())
            .and_then(|message| message["content"].as_str())
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
            .and_then(|payload| payload["candidates"].as_array().and_then(|rows| rows.first()).cloned())
            .map(|row| row["id"].clone()).unwrap_or(Value::Null);
        json!({"ratings":[{"id":id,"motivation":5,"relevance":5,"originality":5}]})
    } else if body.contains("TASK: ARTICULATE") {
        json!({"text":"收到","emoji":null})
    } else { json!({}) };
    let response = json!({"choices":[{"message":{"content":answer.to_string()},"finish_reason":"stop"}]});
    let response = response.to_string();
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await?;
    Ok(())
}

fn now() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 }
fn message(id: i64, content: &str) -> Value {
    json!({"post_type":"message","message_type":"group","self_id":99,"group_id":10,
        "user_id":20,"message_id":id,"time":now(),"sender":{"nickname":"Human"},
        "message":content,"raw_message":content})
}
fn unknown_notice(kind: &str) -> Value {
    json!({"post_type":"notice","notice_type":kind,"self_id":99,
        "group_id":10,"user_id":20,"time":now()})
}
fn rows(root: &Root, sql: &str) -> Result<i64> {
    Ok(Connection::open(root.db())?.query_row(sql, [], |row| row.get(0))?)
}
fn logs(root: &Root, event: &str) -> Result<Vec<Value>> {
    let content = fs::read_to_string(root.log()).unwrap_or_default();
    Ok(content.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|row| row["event"] == event).collect())
}
async fn until(mut check: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if check()? { return Ok(()); }
        anyhow::ensure!(Instant::now() < deadline, "condition did not become true before timeout");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn started(enabled: bool) -> Result<(Root, Agent, Wire, Model)> {
    let wire = Wire::start().await?;
    let model = Model::start().await?;
    let root = Root::new(&wire.url, &model.url, enabled)?;
    let mut agent = Agent::start(&root)?;
    agent.state(&root)?;
    until(|| {
        let state = agent.state(&root)?;
        Ok(wire.count() == if enabled { 2 } else { 1 }
            && state["onebotConnected"] == true && state["qqOnline"] == true)
    }).await?;
    Ok((root, agent, wire, model))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_plugin_keeps_legacy_ingress() -> Result<()> {
    let (root, mut agent, wire, _model) = started(false).await?;
    wire.broadcast(message(9101, "普通消息"))?;
    until(|| Ok(rows(&root, "SELECT count(*) FROM messages WHERE id='9101'")? == 1)).await?;
    assert_eq!(rows(&root, "SELECT count(*) FROM messages WHERE id='9101'")?, 1);
    assert_eq!(agent.state(&root)?["ingressSource"], "legacy");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabled_plugin_ingests_once_and_reports_chat_port() -> Result<()> {
    let (root, mut agent, wire, _model) = started(true).await?;
    wire.broadcast(message(9102, "普通消息"))?;
    wire.broadcast(unknown_notice("group_recall"))?;
    until(|| Ok(rows(&root, "SELECT count(*) FROM messages WHERE id='9102'")? == 1
        && logs(&root, "notice_unhandled")?.len() == 1)).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rows(&root, "SELECT count(*) FROM messages WHERE id='9102'")?, 1);
    assert_eq!(logs(&root, "notice_unhandled")?.len(), 1);
    // A broadcast alone could be handled by the legacy socket. Probe the two
    // sockets separately to establish which one actually feeds the engine.
    wire.send(0, unknown_notice("group_ban"))?;
    until(|| Ok(logs(&root, "notice_unhandled")?.iter()
        .any(|row| row["kind"] == "group_ban"))).await?;
    wire.send(1, unknown_notice("friend_add"))?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(logs(&root, "notice_unhandled")?.len(), 2,
        "the diagnostic legacy socket must not deliver its notice to the engine");
    assert_eq!(agent.state(&root)?["ingressSource"], "chat_port");
    assert!(logs(&root, "event_rejected")?.is_empty(), "valid frames must not be rejected");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_follows_the_plugin_connection() -> Result<()> {
    let (root, mut agent, wire, _model) = started(true).await?;
    until(|| Ok(agent.state(&root)?["onebotConnected"] == true
        && agent.state(&root)?["qqOnline"] == true)).await?;
    // The factory creates and starts the owned socket before Runtime::connect
    // starts the diagnostic legacy socket; peer zero is therefore the plugin.
    wire.close_first_and_reject_reconnects()?;
    until(|| Ok(wire.count() == 1 && agent.state(&root)?["onebotConnected"] == false)).await?;
    let state = agent.state(&root)?;
    assert_eq!(state["ingressSource"], "chat_port");
    assert_eq!(state["onebotConnected"], false);
    assert_eq!(state["qqOnline"], false);
    assert!(state["reconnects"].is_null(), "legacy reconnects must not describe the plugin");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_is_sent_once_across_both_connections() -> Result<()> {
    let (root, _agent, wire, _model) = started(true).await?;
    wire.broadcast(message(9103, "[CQ:at,qq=99]你好"))?;
    until(|| Ok(wire.actions.lock().unwrap().len() == 1)).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let actions = wire.actions.lock().unwrap().clone();
    assert_eq!(actions.len(), 1, "both WS connections together must send once");
    assert_eq!(actions[0].1["action"], "send_group_msg");
    until(|| Ok(rows(&root, "SELECT count(*) FROM deliveries WHERE status='sent_unconfirmed'")? == 1)).await?;
    let conn = Connection::open(root.db())?;
    let sent_text: String = conn.query_row(
        "SELECT text FROM messages WHERE chat='group:10' AND self=1 AND id='501'", [], |row| row.get(0))?;
    let evidence = |id: i64| json!({"post_type":"message_sent","self_id":99,"group_id":10,
        "user_id":99,"message_id":id,"time":now(),"raw_message":sent_text,
        "sender":{"user_id":99,"nickname":"Bot"}});
    wire.broadcast(evidence(999))?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rows(&root, "SELECT count(*) FROM deliveries WHERE status='confirmed'")?, 0);
    wire.broadcast(evidence(501))?;
    until(|| Ok(rows(&root, "SELECT count(*) FROM deliveries WHERE status='confirmed'")? == 1)).await?;
    wire.broadcast(evidence(501))?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rows(&root, "SELECT count(*) FROM deliveries WHERE status='confirmed'")?, 1);
    assert_eq!(wire.actions.lock().unwrap().len(), 1, "reconciliation must not resend");
    Ok(())
}

struct UnitTransport;
impl OrientationTransport for UnitTransport {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unused") })
    }
}
impl OrientationProvider for UnitTransport {
    fn json<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unused") })
    }
}
impl EngineTransport for UnitTransport {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "99".into(), reconnects: 0, degraded: false }
    }
    fn send<'a>(&'a self, _: &'a str, _: &'a str, _: Option<&'a str>, _: Option<&'a str>,
        _: Option<&'a str>) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async { panic!("unused") })
    }
}
struct RejectAttach(PortManifest);
impl ChatPort for RejectAttach {
    fn manifest(&self) -> &PortManifest { &self.0 }
    fn capabilities(&self) -> Capabilities { Capabilities::default() }
    fn state(&self) -> PortState { PortState::disconnected() }
    fn deliver<'a>(&'a self, _: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        Box::pin(async { panic!("unused") })
    }
    fn attach(&self, _: mpsc::Sender<PortEvent>) -> std::result::Result<(), IoError> {
        Err(IoError::new("sink_rejected"))
    }
}

#[test]
fn attach_failure_leaves_no_primary_port() -> Result<()> {
    let config = Config::from_value(&merge(&defaults(), &json!({
        "apiKey":"mock","onebotToken":"","dataDir":"unused",
        "provider":{"model":"fixture"},"agent":{"allowedGroups":["42"]}
    })))?;
    let transport = Arc::new(UnitTransport);
    let engine = Engine::new(config, Arc::new(Mutex::new(Store::in_memory()?)),
        transport.clone(), transport, Options::default())?;
    let port = Arc::new(RejectAttach(PortManifest {
        name: "onebot".into(), kind: PortKind::Chat, transport: TransportKind::InProcess,
        core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
        host_permissions: vec![], dependencies: vec![], config_fields: vec![],
    }));
    let error = engine.attach_primary_chat_port(port, 16).unwrap_err();
    assert!(error.to_string().contains("sink_rejected"));
    assert!(engine.chat_port_for("group:42").is_none());
    Ok(())
}
