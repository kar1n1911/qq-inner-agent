//! 所有网络测试只绑定/连接 127.0.0.1，不访问真实桥。
use futures_util::{SinkExt, StreamExt};
use qq_inner_core::{
    config,
    onebot::{Notification, OneBot},
};
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{timeout, Duration},
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

struct Mock {
    url: String,
    requests: mpsc::UnboundedReceiver<Value>,
    frames: mpsc::UnboundedSender<Message>,
    task: JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
// tungstenite 回调签名固定使用未装箱的 HTTP 错误响应。
#[allow(clippy::result_large_err)]
async fn mock(login: Value, online: Value, early: usize) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let (request_tx, requests) = mpsc::unbounded_channel();
    let (frames, mut frame_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = accept_hdr_async(stream, |req: &Request, response: Response| {
                if req.uri().query() == Some("access_token=a%2Bb+%26%3F") {
                    Ok(response)
                } else {
                    Err(tokio_tungstenite::tungstenite::http::Response::builder()
                        .status(401)
                        .body(Some("unauthorized".into()))
                        .unwrap())
                }
            })
            .await;
            let Ok(mut ws) = socket else {
                continue;
            };
            for i in 0..early {
                ws.send(Message::Text(
                    json!({"post_type":"custom","i":i}).to_string(),
                ))
                .await
                .unwrap();
            }
            let mut handshaking = true;
            loop {
                tokio::select! {
                    Some(frame) = frame_rx.recv() => { if ws.send(frame).await.is_err() { break; } },
                    incoming = ws.next() => {
                        let Some(Ok(Message::Text(text))) = incoming else { break; };
                        let req: Value = serde_json::from_str(&text).unwrap();
                        let data = match req["action"].as_str().unwrap() {
                            "get_login_info" => Some(login.clone()),
                            "get_status" if handshaking => { handshaking = false; Some(json!({"online":online})) },
                            _ => None,
                        };
                        if let Some(data) = data { if ws.send(response(&req, data, json!(0), "ok")).await.is_err() { break; } }
                        else { request_tx.send(req).unwrap(); }
                    }
                }
            }
        }
    });
    Mock {
        url,
        requests,
        frames,
        task,
    }
}
fn response(req: &Value, data: Value, retcode: Value, status: &str) -> Message {
    Message::Text(
        json!({"echo":req["echo"],"status":status,"retcode":retcode,"data":data}).to_string(),
    )
}
fn bot(
    m: &Mock,
    token: &str,
    id: &str,
    heartbeat: f64,
) -> (OneBot, mpsc::UnboundedReceiver<Notification>) {
    let mut c: config::Onebot =
        serde_json::from_value(config::defaults()["onebot"].clone()).unwrap();
    c.url = m.url.clone();
    c.self_id.text = id.into();
    c.self_id.is_truthy = !id.is_empty();
    c.request_timeout_seconds = 0.15;
    c.heartbeat_seconds = heartbeat;
    c.reconnect_max_seconds = 1.0;
    OneBot::new(c, token.into())
}
async fn next<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
    timeout(Duration::from_secs(4), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn running(
    bot: &OneBot,
    rx: &mut mpsc::UnboundedReceiver<Notification>,
) -> (watch::Sender<bool>, JoinHandle<()>) {
    let (stop, shutdown) = watch::channel(false);
    let b = bot.clone();
    let task = tokio::spawn(async move { b.run(shutdown).await });
    assert!(matches!(next(rx).await, Notification::Status(s) if s == "connected"));
    (stop, task)
}
async fn stop(stop: watch::Sender<bool>, task: JoinHandle<()>) {
    stop.send(true).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn authentication_and_handshake_errors() {
    for (login, id, token, expected) in [
        (json!({"user_id":123}), "123", "wrong", "connect_closed"),
        (json!({}), "", "a+b &?", "missing_account"),
        (json!({"user_id":0}), "", "a+b &?", "missing_account"),
        (json!({"user_id":123}), "456", "a+b &?", "wrong_qq_account"),
    ] {
        let m = mock(login, json!(true), 0).await;
        let (b, mut rx) = bot(&m, token, id, 10.0);
        let err = b.check().await.unwrap_err();
        assert_eq!(err.code, expected);
        assert!(!err.uncertain);
        if token == "wrong" {
            assert!(
                matches!(next(&mut rx).await, Notification::Status(s) if s == "websocket_error")
            );
        }
        assert!(!b.state().connected);
    }
    for online in [json!(true), json!(false), json!("true")] {
        let m = mock(json!({"user_id":123}), online.clone(), 0).await;
        let (b, mut rx) = bot(&m, "a+b &?", "123", 10.0);
        let state = b.check().await.unwrap();
        assert_eq!(state.self_id, "123");
        assert!(state.connected);
        assert_eq!(state.online, online == true);
        assert!(
            matches!(next(&mut rx).await, Notification::Status(s) if s == if online == true {"connected"} else {"qq_offline"})
        );
    }
}
#[tokio::test]
async fn early_buffer_limit_order_and_frame_filter() {
    let m = mock(json!({"user_id":123}), json!(true), 205).await;
    let (b, mut rx) = bot(&m, "a+b &?", "", 10.0);
    let (s, t) = running(&b, &mut rx).await;
    for i in 0..200 {
        assert!(matches!(next(&mut rx).await, Notification::Event(e) if e["i"] == i));
    }
    assert!(rx.try_recv().is_err());
    for frame in [
        Message::Text("{".into()),
        Message::Binary(br#"{"post_type":"binary"}"#.to_vec()),
        Message::Text(json!({"post_type":"large","text":"😀".repeat(500_001)}).to_string()),
    ] {
        m.frames.send(frame).unwrap();
    }
    // 未知 post_type 与未匹配 echo 仍应透传。
    m.frames
        .send(Message::Text(
            json!({"post_type":"unknown","echo":"unmatched"}).to_string(),
        ))
        .unwrap();
    assert!(matches!(next(&mut rx).await, Notification::Event(e) if e["post_type"] == "unknown"));
    stop(s, t).await;
}
#[tokio::test]
async fn calls_echo_failures_timeout_and_disconnect() {
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = bot(&m, "a+b &?", "", 10.0);
    let (s, t) = running(&b, &mut rx).await;
    let a = b.clone();
    let first = tokio::spawn(async move { a.call("first", json!({})).await });
    let a = b.clone();
    let second = tokio::spawn(async move { a.call("second", json!({})).await });
    let r1 = next(&mut m.requests).await;
    let r2 = next(&mut m.requests).await;
    assert_ne!(r1["echo"], r2["echo"]);
    let echo = r1["echo"].as_str().unwrap();
    assert_eq!(echo.len(), 36);
    assert_eq!(&echo[14..15], "4");
    for r in [&r2, &r1] {
        m.frames
            .send(response(r, r["action"].clone(), json!("0"), "ok"))
            .unwrap();
    }
    assert_eq!(first.await.unwrap().unwrap(), "first");
    assert_eq!(second.await.unwrap().unwrap(), "second");
    for (retcode, status, expected) in [
        (json!(42), "failed", "42"),
        (json!(0), "failed", "unknown"),
        (json!(null), "ok", ""),
    ] {
        let a = b.clone();
        let call = tokio::spawn(async move { a.call("test", json!({})).await });
        let req = next(&mut m.requests).await;
        m.frames
            .send(response(&req, json!({}), retcode, status))
            .unwrap();
        let result = call.await.unwrap();
        if expected.is_empty() {
            assert!(result.is_ok())
        } else {
            let e = result.unwrap_err();
            assert_eq!(e.code, format!("onebot_action_failed_{expected}"));
            assert!(!e.uncertain);
        }
    }
    let e = b.call("timeout", json!({})).await.unwrap_err();
    assert_eq!(e.code, "action_timeout");
    assert!(e.uncertain);
    let late = next(&mut m.requests).await;
    m.frames
        .send(response(&late, json!("late"), json!(0), "ok"))
        .unwrap();
    let a = b.clone();
    let call = tokio::spawn(async move { a.call("lost", json!({})).await });
    next(&mut m.requests).await;
    m.frames.send(Message::Close(None)).unwrap();
    let e = call.await.unwrap().unwrap_err();
    assert_eq!(e.code, "connection_lost");
    assert!(e.uncertain);
    stop(s, t).await;
}
#[tokio::test]
async fn sending_format_and_validation() {
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = bot(&m, "a+b &?", "", 10.0);
    assert_eq!(
        b.send("group:1", "hi", None).await.unwrap_err().code,
        "qq_offline"
    );
    assert_eq!(
        b.call("x", json!({})).await.unwrap_err().code,
        "not_connected"
    );
    let (s, t) = running(&b, &mut rx).await;
    for chat in ["group:0", "group:01", "other:1", "private:1:2", "group:١"] {
        assert_eq!(
            b.send(chat, "hi", None).await.unwrap_err().code,
            "invalid_chat"
        );
    }
    for face in ["", "123456", "-1", "１"] {
        assert_eq!(
            b.send("group:1", "hi", Some(face)).await.unwrap_err().code,
            "invalid_face"
        );
    }
    for (chat, action, key, face) in [
        ("group:123", "send_group_msg", "group_id", Some("012")),
        ("private:123", "send_private_msg", "user_id", None),
    ] {
        let a = b.clone();
        let call = tokio::spawn(async move { a.send(chat, "[CQ:at,qq=all]", face).await });
        let req = next(&mut m.requests).await;
        assert_eq!(req["action"], action);
        assert_eq!(req["params"][key].as_f64(), Some(123.0));
        assert_eq!(req["params"]["auto_escape"], true);
        let mut segments = vec![json!({"type":"text","data":{"text":"[CQ:at,qq=all]"}})];
        if let Some(id) = face {
            segments.push(json!({"type":"face","data":{"id":id}}));
        }
        assert_eq!(req["params"]["message"], json!(segments));
        m.frames
            .send(response(&req, json!({"message_id":9}), json!(0), "ok"))
            .unwrap();
        assert_eq!(call.await.unwrap().unwrap()["message_id"], 9);
    }
    // 单 face 的传输不允许夹带空 text 段。
    let a = b.clone();
    let call = tokio::spawn(async move { a.send("group:123", "", Some("14")).await });
    let req = next(&mut m.requests).await;
    assert_eq!(
        req["params"]["message"],
        json!([{"type":"face","data":{"id":"14"}}])
    );
    m.frames
        .send(response(&req, json!({"message_id":10}), json!(0), "ok"))
        .unwrap();
    assert_eq!(call.await.unwrap().unwrap()["message_id"], 10);
    stop(s, t).await;
}
#[tokio::test]
async fn heartbeat_no_overlap_offline_failure_and_reconnect() {
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = bot(&m, "a+b &?", "", 0.02);
    let (s, t) = running(&b, &mut rx).await;
    let req = next(&mut m.requests).await;
    assert_eq!(req["action"], "get_status");
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(m.requests.try_recv().is_err());
    m.frames
        .send(response(&req, json!({"online":false}), json!(0), "ok"))
        .unwrap();
    let req = next(&mut m.requests).await;
    assert!(!b.state().online);
    assert_eq!(
        b.send("group:1", "x", None).await.unwrap_err().code,
        "qq_offline"
    );
    m.frames
        .send(response(&req, json!({"online":true}), json!(0), "ok"))
        .unwrap();
    let _req = next(&mut m.requests).await;
    assert!(b.state().online);
    // 不回心跳，触发超时并断开；下一次连接应完成握手。
    assert!(matches!(next(&mut rx).await,Notification::Status(s) if s=="connected"));
    assert!(b.state().reconnects >= 1);
    stop(s, t).await;
    assert!(!b.state().connected);
}
#[tokio::test]
async fn connect_timeout_and_cancel() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut c: config::Onebot =
        serde_json::from_value(config::defaults()["onebot"].clone()).unwrap();
    c.url = format!("ws://{}/", listener.local_addr().unwrap());
    c.request_timeout_seconds = 0.05;
    let (b, _) = OneBot::new(c, String::new());
    assert_eq!(b.check().await.unwrap_err().code, "connect_timeout");
    let (s, shutdown) = watch::channel(false);
    let a = b.clone();
    let task = tokio::spawn(async move { a.run(shutdown).await });
    s.send(true).unwrap();
    task.await.unwrap();
    assert_eq!(b.state().reconnects, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_check_root_success_offline_and_auth_failure() {
    for (online, token, success, marker) in [
        (true, "a+b &?", true, "authentication = ok"),
        (false, "a+b &?", false, "qq_offline"),
        (true, "wrong-secret", false, "connect_closed"),
    ] {
        let m = mock(json!({"user_id":123}), json!(online), 0).await;
        // 临时 fixture 也留在 rust/ 内，遵守写入范围。
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("onebot-check-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.json"),
            json!({"onebot":{"url":m.url,"selfId":"123"}}).to_string(),
        )
        .unwrap();
        std::fs::write(
            root.join("secrets.json"),
            json!({"onebotToken":token}).to_string(),
        )
        .unwrap();
        let path = root.clone();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_qq-inner-core"))
                .arg("--root")
                .arg(path)
                .arg("check")
                .env_remove("ONEBOT_TOKEN")
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(output.status.success(), success);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(text.contains(marker), "{text}");
        assert!(!text.contains(token));
        assert!(!text.contains(&m.url));
    }
}

#[tokio::test]
async fn repeated_disconnects_respect_backoff_cap() {
    let m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = bot(&m, "a+b &?", "", 10.0);
    let (s, t) = running(&b, &mut rx).await;
    for count in 1..=3 {
        let started = tokio::time::Instant::now();
        m.frames.send(Message::Close(None)).unwrap();
        assert!(matches!(next(&mut rx).await,Notification::Status(s) if s=="connected"));
        assert_eq!(b.state().reconnects, count);
        // 上限只约束 delay，仍允许 [0,1) 秒抖动及少量调度余量。
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_millis(2500));
    }
    stop(s, t).await;
}
