//! A real process must expose each active interface route and its observed health.
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const BIN: &str = env!("CARGO_BIN_EXE_qq-inner-core");

struct Root(PathBuf);
impl Root {
    fn new(ws_url: &str, model_url: &str) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("interface-availability-{}", rand::random::<u64>()));
        fs::create_dir_all(path.join("plugins/onebot"))?;
        fs::write(path.join("plugins/onebot/manifest.json"),
            include_str!("../../plugins/onebot/manifest.json"))?;
        fs::write(path.join("plugins/plugins.json"), serde_json::to_vec_pretty(&json!({
            "schema_version":1,"mode":"manual","plugins":[{
                "id":"onebot","directory":"onebot","enabled":false,
                "params":{},"bindings":{},"secret_refs":{}
            }]
        }))?)?;
        fs::write(path.join("config.json"), serde_json::to_vec_pretty(&json!({
            "storage":{"directory":"state"},
            "provider":{"kind":"openai","baseUrl":model_url,"model":"fixture"},
            "onebot":{"url":ws_url,"selfId":"99","reconnectMaxSeconds":1},
            "agent":{"allowedGroups":["10"],"observation":{"enabled":false},
                "identity":{"enabled":false},"backfill":{"enabled":false},
                "schedule":{"enabled":false},"rhythm":{"enabled":false}}
        }))?)?;
        fs::write(path.join("secrets.json"), br#"{"apiKey":"fixture-key"}"#)?;
        Ok(Self(path))
    }
    fn state(&self) -> PathBuf { self.0.join("state") }
    fn status(&self) -> PathBuf { self.state().join("status.json") }
    fn log(&self) -> PathBuf { self.state().join("agent.log") }
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
            anyhow::bail!("agent exited: {status}; {stderr}");
        }
        Ok(())
    }
}
impl Drop for Agent {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

struct Wire {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Wire {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("ws://{}/", listener.local_addr()?);
        let task = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(mut ws) = accept_async(socket).await else { return; };
                    while let Some(Ok(Message::Text(frame))) = ws.next().await {
                        let Ok(request) = serde_json::from_str::<Value>(&frame) else { continue; };
                        let data = match request["action"].as_str().unwrap_or("") {
                            "get_login_info" => json!({"user_id":99}),
                            "get_status" => json!({"online":true}),
                            _ => json!({}),
                        };
                        let reply = json!({"echo":request["echo"],"status":"ok","retcode":0,"data":data});
                        if ws.send(Message::Text(reply.to_string())).await.is_err() { break; }
                    }
                });
            }
        });
        Ok(Self { url, task })
    }
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

fn read_status(root: &Root) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(root.status())?)?)
}
fn read_log(root: &Root) -> Result<Vec<Value>> {
    Ok(fs::read_to_string(root.log()).unwrap_or_default().lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok()).collect())
}
async fn wait_for(mut check: impl FnMut() -> Result<bool>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if check()? { return Ok(()); }
        anyhow::ensure!(Instant::now() < deadline, "runtime condition timed out");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
async fn start() -> Result<(Root, Agent, Wire, Model)> {
    let wire = Wire::start().await?;
    let model = Model::start().await?;
    let root = Root::new(&wire.url, &model.url)?;
    let mut agent = Agent::start(&root)?;
    wait_for(|| {
        agent.check_running()?;
        match read_status(&root) {
            Ok(status) => Ok(status["onebotConnected"] == true && status["qqOnline"] == true),
            Err(_) => Ok(false),
        }
    }).await?;
    Ok((root, agent, wire, model))
}

fn interface<'a>(status: &'a Value, kind: &str) -> Result<&'a Value> {
    let all = &status["interfaceAvailability"];
    let entry = if let Some(rows) = all.as_array() {
        rows.iter().find(|row| row["kind"] == kind)
    } else {
        all.get(kind)
    };
    entry.with_context(|| format!("status must expose {kind} availability: {status}"))
}
fn route(entry: &Value) -> Result<&str> {
    entry["route"].as_str().context("route must be readable")
}
fn health(entry: &Value) -> Result<&str> {
    entry["health"].as_str().context("health must be readable")
}
fn availability_events(root: &Root, kind: &str) -> Result<Vec<Value>> {
    Ok(read_log(root)?.into_iter().filter(|row| {
        row["kind"] == kind && event_field(row, "route").is_string()
    }).collect())
}
fn event_field<'a>(row: &'a Value, field: &str) -> &'a Value {
    for state in ["current", "new", "to"] {
        if row[state][field].is_string() { return &row[state][field]; }
    }
    &row[field]
}
fn active_plugin(status: &Value, kind: &str) -> bool {
    let plugins = &status["plugins"];
    let entries: Vec<&Value> = match plugins {
        Value::Array(rows) => rows.iter().collect(),
        Value::Object(rows) => rows.values().collect(),
        _ => vec![],
    };
    entries.into_iter().any(|entry| {
        entry["kind"].as_str().is_some_and(|name| name.eq_ignore_ascii_case(kind))
            && entry["effective_state"] == "active"
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_all_three_interfaces() -> Result<()> {
    let (root, _agent, _wire, _model) = start().await?;
    let status = read_status(&root)?;
    for kind in ["chat", "model", "db"] {
        let entry = interface(&status, kind)?;
        if entry.get("kind").is_some() { assert_eq!(entry["kind"], kind); }
        assert!(matches!(route(entry)?, "plugin" | "built_in" | "none"));
        assert!(matches!(health(entry)?, "ready" | "degraded" | "unknown" | "missing"));
        assert!(matches!(entry["scope"].as_str(), Some("full" | "diagnostic_only")));
        assert!(entry["observedAt"].as_f64().is_some_and(|n| n.is_finite() && n > 0.));
        assert!(entry["generation"].as_str().is_some_and(|s| !s.is_empty()));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_falls_back_to_built_in_with_a_visible_warning() -> Result<()> {
    let (root, _agent, _wire, _model) = start().await?;
    let status = read_status(&root)?;
    let chat = interface(&status, "chat")?;
    assert_eq!(route(chat)?, "built_in");
    assert_eq!(health(chat)?, "degraded", "an available fallback is not missing or fully ready");
    assert!(chat["fallbackFrom"].is_null());
    wait_for(|| Ok(availability_events(&root, "chat")?.iter().any(|row|
        event_field(row, "route") == "built_in"))).await?;
    let warnings = availability_events(&root, "chat")?;
    assert!(warnings.iter().any(|row| event_field(row, "route") == "built_in"),
        "fallback must leave a structured warning with its actual route");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_and_db_report_their_actual_route() -> Result<()> {
    let (root, _agent, _wire, _model) = start().await?;
    let status = read_status(&root)?;
    for kind in ["model", "db"] {
        let entry = interface(&status, kind)?;
        let selected = route(entry)?;
        assert_ne!(selected, "none", "the fixture has a live {kind} implementation");
        assert_ne!(health(entry)?, "missing", "a selected implementation cannot be missing");
        assert_eq!(selected == "plugin", active_plugin(&status, kind),
            "route must agree with the actual published plugin, not its manifest declaration");
        assert_eq!(entry["scope"], "full", "the live primary {kind} interface has full scope");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn availability_changes_are_logged_once_per_transition() -> Result<()> {
    let (root, mut agent, _wire, _model) = start().await?;
    wait_for(|| Ok(!availability_events(&root, "chat")?.is_empty())).await?;
    let first_modified = fs::metadata(root.status())?.modified()?;
    wait_for(|| {
        agent.check_running()?;
        Ok(fs::metadata(root.status())?.modified()? > first_modified)
    }).await?;
    let events = availability_events(&root, "chat")?;
    let snapshot = interface(&read_status(&root)?, "chat")?.clone();
    let route = route(&snapshot)?;
    let health = health(&snapshot)?;
    let same: Vec<_> = events.iter().filter(|row|
        event_field(row, "route") == route && event_field(row, "health") == health).collect();
    assert_eq!(same.len(), 1, "unchanged availability must not warn on each status write");
    let change = same[0];
    assert!(change.get("previous").or_else(|| change.get("old"))
        .or_else(|| change.get("from")).is_some(),
        "transition log must contain the previous state");
    assert!(change.get("current").or_else(|| change.get("new"))
        .or_else(|| change.get("to")).is_some(),
        "transition log must contain the new state");
    Ok(())
}
