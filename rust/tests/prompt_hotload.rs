//! Goal basis: docs/DEVELOPMENT.md:54 — tuning prompts must need no build/restart.
//! Real process, watcher, OneBot input, HTTP provider output and control state.get.
use futures_util::{SinkExt, StreamExt};
use qq_inner_core::{prompt_overlay::FILE, prompts, settings};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixStream},
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

struct Service {
    child: Child,
    root: PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for name in ["status.json", "agent.log"] {
                eprintln!(
                    "{name}: {}",
                    fs::read_to_string(self.root.join("data").join(name)).unwrap_or_default()
                );
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}
async fn state(service: &Service) -> Option<Value> {
    let mut socket = UnixStream::connect(service.root.join("data/control.sock"))
        .await
        .ok()?;
    socket
        .write_all(b"{\"id\":\"test\",\"method\":\"state.get\",\"params\":{}}\n")
        .await
        .ok()?;
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).await.ok()?;
    let value: Value = serde_json::from_str(&line).ok()?;
    (value["ok"] == true).then(|| value["result"].clone())
}
async fn wait_state(service: &Service, predicate: impl Fn(&Value) -> bool) -> Value {
    timeout(Duration::from_secs(20), async {
        loop {
            if let Some(value) = state(service).await {
                if predicate(&value) {
                    return value;
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("service state timeout")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_edit_reaches_next_form_and_articulate_in_same_running_binary() {
    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http.local_addr().unwrap();
    let systems = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = systems.clone();
    let provider = tokio::spawn(async move {
        loop {
            let (socket, _) = http.accept().await.unwrap();
            let captured = captured.clone();
            tokio::spawn(async move {
                let mut socket = BufReader::new(socket);
                let mut size = 0;
                loop {
                    let mut line = String::new();
                    socket.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            size = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; size];
                socket.read_exact(&mut body).await.unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let system = request["messages"][0]["content"].as_str().unwrap();
                let payload: Value =
                    serde_json::from_str(request["messages"][1]["content"].as_str().unwrap())
                        .unwrap();
                captured.lock().unwrap().push(system.into());
                let result = if system.contains("TASK: FORM\n") {
                    json!({"allocation":"self","candidates":[{"kind":"system2","text":"看看盆土的湿度"}]})
                } else if system.contains("TASK: EVALUATE\n") {
                    json!({"ratings":payload["candidates"].as_array().unwrap().iter().map(|c| json!({"id":c["id"],"motivation":5,"relevance":5,"originality":5,"for":["relevance"],"against":[]})).collect::<Vec<_>>()})
                } else if system.contains("TASK: FORECAST\n") {
                    json!({"shouldSend":true,"outcomes":{"reply":0.6,"silence":0.4,"negative":0},"responseMode":"answer","plan":"回答问题"})
                } else {
                    assert!(system.contains("TASK: ARTICULATE\n"));
                    json!({"text":"可以先看看盆土是不是已经干透了。"})
                };
                let body = json!({"choices":[{"message":{"content":result.to_string()},"finish_reason":"stop"}]}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
        }
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = listener.local_addr().unwrap();
    let (send, mut events) = mpsc::unbounded_channel::<Value>();
    let bridge = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        loop {
            tokio::select! {
                Some(event) = events.recv() => ws.send(Message::Text(event.to_string())).await.unwrap(),
                message = ws.next() => {
                    let Some(Ok(Message::Text(text))) = message else { break; };
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let data = match request["action"].as_str().unwrap() {
                        "get_login_info" => json!({"user_id":99,"nickname":"Bot"}),
                        "get_status" => json!({"online":true,"good":true}),
                        "get_group_member_list" => json!([]),
                        _ => json!({}),
                    };
                    ws.send(Message::Text(json!({"status":"ok","retcode":0,"data":data,"echo":request["echo"]}).to_string())).await.unwrap();
                }
            }
        }
    });
    let root = PathBuf::from(format!("/tmp/qp-{:x}", rand::random::<u64>()));
    fs::create_dir_all(root.join(".runtime")).unwrap();
    settings::atomic_json(&root.join("config.json"), &json!({
        "apiKey":"local-test", "onebot":{"url":format!("ws://{ws_addr}"),"selfId":"99"},
        "provider":{"baseUrl":format!("http://{http_addr}"),"model":"mock"},
        "agent":{"allowedGroups":["10"],"dryRun":true,"schedule":{"enabled":false},"rhythm":{"enabled":false},"observation":{"enabled":false},"learning":{"enabled":false},"backfill":{"enabled":false},"quietHours":null,"debounceSeconds":0,"minThinkIntervalSeconds":1,"proactiveCooldownSeconds":0,"sending":{"enabled":true,"addressedProbability":1}}
    })).unwrap();
    settings::atomic_json(&root.join("secrets.json"), &json!({"apiKey":"local-test"})).unwrap();
    let write = |version: &str| {
        settings::atomic_json(
            &root.join(FILE),
            &json!({
                "FORMATION":format!("{}\nFORM-{version}", prompts::FORMATION),
                "ARTICULATION":format!("{}\nARTICULATE-{version}", prompts::ARTICULATION)
            }),
        )
        .unwrap()
    };
    write("before");
    let binary = env!("CARGO_BIN_EXE_qq-inner-core");
    let original_binary = fs::read(binary).unwrap();
    let child = Command::new(binary)
        .args(["start", "--root", root.to_str().unwrap()])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let service = Service {
        child,
        root: root.clone(),
    };
    let first = wait_state(&service, |s| {
        s["qqOnline"] == true && s["prompts"]["source"] == "overlay"
    })
    .await;
    let event = |id: &str| json!({"post_type":"message","message_type":"group","group_id":10,"user_id":20,"self_id":99,"message_id":id,"time":chrono::Utc::now().timestamp(),"sender":{"nickname":"Human"},"message":"[CQ:at,qq=99]怎么浇水？"});
    send.send(event("first")).unwrap();
    timeout(Duration::from_secs(15), async {
        loop {
            if systems
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.contains("ARTICULATE-before"))
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(systems
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.contains("FORM-before")));
    fs::write(root.join(".settings-write"), "test barrier").unwrap();
    write("after");
    sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        state(&service).await.unwrap()["prompts"]["revision"],
        first["prompts"]["revision"]
    );
    fs::remove_file(root.join(".settings-write")).unwrap();
    let updated = wait_state(&service, |s| {
        s["prompts"]["revision"] != first["prompts"]["revision"]
    })
    .await;
    assert_eq!(updated["pid"], first["pid"]);
    assert_eq!(updated["pid"], service.child.id());
    assert_eq!(updated["appliedRevision"], first["appliedRevision"]);
    systems.lock().unwrap().clear();
    send.send(event("second")).unwrap();
    timeout(Duration::from_secs(15), async {
        loop {
            if systems
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.contains("ARTICULATE-after"))
            {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(systems
        .lock()
        .unwrap()
        .iter()
        .any(|s| s.contains("FORM-after")));
    assert!(systems
        .lock()
        .unwrap()
        .iter()
        .all(|s| !s.contains("-before")));
    assert_eq!(
        fs::read(binary).unwrap(),
        original_binary,
        "no rebuild or binary replacement"
    );
    // Availability and bus wiring: malformed file, per-key fallback, deletion, recovery.
    assert!(!updated["prompts"]["errors"].as_array().unwrap().is_empty());
    fs::write(root.join(FILE), "{").unwrap();
    let bad = wait_state(&service, |s| s["prompts"]["source"] == "builtin").await;
    assert_eq!(bad["pid"], first["pid"]);
    assert!(!bad["prompts"]["errors"].as_array().unwrap().is_empty());
    fs::remove_file(root.join(FILE)).unwrap();
    wait_state(&service, |s| s["prompts"]["revision"].is_null()).await;
    write("recovered");
    let recovered = wait_state(&service, |s| s["prompts"]["source"] == "overlay").await;
    assert_eq!(recovered["pid"], first["pid"]);
    bridge.abort();
    provider.abort();
}
