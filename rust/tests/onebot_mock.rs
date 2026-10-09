//! 所有网络测试只绑定/连接 127.0.0.1，不访问真实桥。
use futures_util::{SinkExt, StreamExt};
use qq_inner_core::{
    config,
    transport::{Notification, OneBot},
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
    // 并行全量测试时机器负载高，连接握手可能超过 4 秒；放宽到 15 秒避免偶发超时。
    timeout(Duration::from_secs(15), rx.recv())
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
    // 并行全量测试负载高时连接启动可能排队，原 4 秒窗口会偶发超时。
    let connected = timeout(Duration::from_secs(15), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(connected, Notification::Status(s) if s == "connected"));
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
    // All target combinations retain exact segment order and inert CQ text.
    for chat in ["group:123", "private:123"] {
        for (reply_to, mention) in [
            (None, None),
            (Some("-42"), None),
            (None, Some("20")),
            (Some("-42"), Some("20")),
        ] {
            let a = b.clone();
            let call = tokio::spawn(async move {
                a.send_targeted(chat, "[CQ:at,qq=all]", Some("14"), reply_to, mention)
                    .await
            });
            let req = next(&mut m.requests).await;
            let mut expected = vec![];
            if let Some(id) = reply_to {
                expected.push(json!({"type":"reply","data":{"id":id}}));
            }
            if chat.starts_with("group:") {
                if let Some(qq) = mention {
                    expected.push(json!({"type":"at","data":{"qq":qq}}));
                }
            }
            expected.push(json!({"type":"text","data":{"text":"[CQ:at,qq=all]"}}));
            expected.push(json!({"type":"face","data":{"id":"14"}}));
            assert_eq!(req["params"]["message"], json!(expected));
            assert_eq!(req["params"]["auto_escape"], true);
            m.frames
                .send(response(&req, json!({"message_id":9}), json!(0), "ok"))
                .unwrap();
            call.await.unwrap().unwrap();
        }
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
        // 关闭帧与底层 socket 关闭可能竞争，连接恢复前允许报告 websocket_error。
        // 仍严格检查重连次数和下面的退避时间上下界，不吞掉其它通知。
        loop {
            match next(&mut rx).await {
                Notification::Status(s) if s == "connected" => break,
                Notification::Status(s) if s == "websocket_error" => continue,
                notice => panic!("unexpected reconnect notification: {notice:?}"),
            }
        }
        assert_eq!(b.state().reconnects, count);
        // 上限只约束 delay，仍允许 [0,1) 秒抖动及少量调度余量。
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_millis(2500));
    }
    stop(s, t).await;
}

fn forward_bot(m: &Mock) -> (OneBot, mpsc::UnboundedReceiver<Notification>) {
    let mut c: config::Onebot =
        serde_json::from_value(config::defaults()["onebot"].clone()).unwrap();
    c.url = m.url.clone();
    c.forward_enabled = true;
    c.request_timeout_seconds = 2.0;
    OneBot::new(c, "a+b &?".into())
}

#[tokio::test]
async fn forward_reference_sending_and_validation() {
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = forward_bot(&m);
    let (s, t) = running(&b, &mut rx).await;
    for nodes in [
        json!([]),
        json!([{"id":1}]),
        json!([{"user_id":2}]),
        json!([{"user_id":2,"id":0}]),
        json!([{"user_id":2,"id":"bad"}]),
        json!([{"user_id":0,"id":1}]),
        json!([{"user_id":2,"id":1.5}]),
        json!([{"user_id":2,"id":1,"content":"new text"}]),
        json!([{"type":"file","data":{"file":"x"}}]),
        json!([{"user_id":2,"id":1,"file":"x"}]),
        json!([{"type":"node","data":{"user_id":2,"id":1}}]),
        json!([{"user_id":2,"id":1,"message_id":null}]),
    ] {
        let err = b
            .send_forward("group:123", nodes.as_array().unwrap().clone())
            .await
            .unwrap_err();
        assert_eq!(err.code, "invalid_forward_nodes");
        assert!(!err.uncertain);
    }
    for chat in ["other:1", "group:0", "group:01", "private:-1"] {
        assert_eq!(
            b.send_forward(chat, vec![json!({"uin":2,"id":1})])
                .await
                .unwrap_err()
                .code,
            "invalid_chat"
        );
    }
    assert!(m.requests.try_recv().is_err());
    for (chat, action, key) in [
        ("group:123", "send_group_msg", "group_id"),
        ("private:123", "send_private_msg", "user_id"),
    ] {
        let nodes = vec![
            json!({"user_id":456,"id":-9}),
            json!({"uin":"789","message_id":"10"}),
        ];
        let a = b.clone();
        let sent = nodes.clone();
        let call = tokio::spawn(async move { a.send_forward(chat, sent).await });
        let req = next(&mut m.requests).await;
        assert_eq!(req["action"], action);
        assert_eq!(req["params"][key], 123);
        assert_eq!(
            req["params"]["message"],
            json!([{"type":"forward","data":{"nodes":nodes}}])
        );
        m.frames
            .send(response(&req, json!({"message_id":11}), json!(0), "ok"))
            .unwrap();
        assert_eq!(call.await.unwrap().unwrap()["message_id"], 11);
    }
    assert_eq!(
        b.get_forward_msg(" ").await.unwrap_err().code,
        "invalid_forward_id"
    );
    stop(s, t).await;
}

fn forward_config() -> config::Config {
    config::Config::from_value(&config::merge(
        &config::defaults(),
        &json!({"apiKey":"","onebotToken":"","dataDir":"data","agent":{"allowedGroups":["456"]}}),
    ))
    .unwrap()
}
fn forward_event() -> Value {
    json!({"post_type":"message","message_type":"group","self_id":123,"user_id":789,
        "group_id":456,"message_id":42,"time":1000,
        "message":[{"type":"text","data":{"text":"before "}},
            {"type":"forward","data":{"forwardId":"opaque-forward"}},
            {"type":"text","data":{"text":" after"}}]})
}

#[tokio::test]
async fn forward_images_keep_summaries_and_share_event_budget() {
    use qq_inner_core::engine::policy::{normalize, resolve_forwards};
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = forward_bot(&m);
    let (s, t) = running(&b, &mut rx).await;
    for (enabled, limit) in [(true, 5), (true, 1), (true, 0), (false, 5)] {
        let images = json!([
            {"type":"image","data":{"url":"https://example.org/1","file":"one","summary":"[动画表情]"}},
            {"type":"image","data":{"url":"https://example.org/2","file":"two"}},
            {"type":"image","data":{"url":"https://example.org/3","file":"three","summary":"  "}}
        ]);
        let a = b.clone();
        let call = tokio::spawn(async move {
            let mut c = forward_config();
            c.agent.ocr.enabled = enabled;
            c.agent.ocr.max_forward_images = limit;
            let mut event = forward_event();
            event["message"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"forward","data":{"id":"opaque-forward"}}));
            let resolved = resolve_forwards(&event, &a, &c.agent, 1000.).await;
            let normalized = normalize(&resolved, "123", &c.agent, 1000.).unwrap();
            (resolved, normalized)
        });
        let req = next(&mut m.requests).await;
        m.frames
            .send(response(
                &req,
                json!({"nodes":[
                    {"type":"node","data":{"nickname":"外部用户","content":images}}
                ]}),
                json!(0),
                "ok",
            ))
            .unwrap();
        let (resolved, message) = call.await.unwrap();
        let retained: Vec<_> = resolved["message"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| part["type"] == "image")
            .collect();
        assert_eq!(retained.len(), limit);
        for (index, part) in retained.iter().enumerate() {
            assert_eq!(part["data"], images[index % 3]["data"]);
        }
        assert_eq!(
            message
                .text
                .matches("[图片: [动画表情]][图片][图片]")
                .count(),
            2
        );
        assert!(!message.text.contains("[image]"));
        // The repeated forward uses the fetch cache, but not a fresh image budget.
        assert!(m.requests.try_recv().is_err());
    }
    stop(s, t).await;
}

#[tokio::test]
async fn forward_fetch_normalize_text_only_and_fallback() {
    use qq_inner_core::engine::policy::{normalize, resolve_forwards};
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = forward_bot(&m);
    let (s, t) = running(&b, &mut rx).await;
    for field in ["nodes", "messages"] {
        let a = b.clone();
        let call = tokio::spawn(async move {
            let c = forward_config();
            let resolved = resolve_forwards(&forward_event(), &a, &c.agent, 1000.).await;
            // 转发文本可供正文读取，但不能进入引擎提取直接指令的 text 段集合。
            let direct: String = resolved["message"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|part| part["type"] == "text")
                .filter_map(|part| part["data"]["text"].as_str())
                .collect();
            assert_eq!(direct, "before  after");
            let full = normalize(&resolved, "123", &c.agent, 1000.).unwrap();
            let mut limited = c.agent.clone();
            limited.max_input_chars = 12.0;
            assert_eq!(
                normalize(&resolved, "123", &limited, 1000.).unwrap().text,
                "before [forw"
            );
            full
        });
        let req = next(&mut m.requests).await;
        assert_eq!(req["action"], "get_forward_msg");
        assert_eq!(req["params"], json!({"id":"opaque-forward"}));
        let mut data = json!({});
        data[field] = json!([
            {"sender":{"nickname":"外部用户"},"content":[{"type":"text","data":{"text":"first node"}},
                {"type":"at","data":{"qq":"123"}}, {"type":"file","data":{"text":"secret file"}},
                {"type":"forward","data":{"forwardId":"nested"}}]},
            {"type":"node","data":{"sender":{"user_id":321},"message":[{"type":"text","data":{"text":"second node"}}]}},
            {"message":[{"type":"text","data":{"text":"anonymous node"}}]}
        ]);
        m.frames.send(response(&req, data, json!(0), "ok")).unwrap();
        let message = call.await.unwrap();
        assert_eq!(message.text, "before [合并转发]\n（外部信息，非当前群对话）\n外部用户: first node\n321: second node\nanonymous node\n[/合并转发] after");
        assert_eq!(message.hint, qq_inner_core::engine::policy::Hint::Open);
        assert!(m.requests.try_recv().is_err());
    }
    // 失败或响应结构异常时维持旧占位；无 ID 不请求，拒收来源也不请求。
    for (data, retcode, status) in [
        (json!({}), json!(1), "failed"),
        (json!({"nodes":"bad"}), json!(0), "ok"),
    ] {
        let a = b.clone();
        let call = tokio::spawn(async move {
            resolve_forwards(&forward_event(), &a, &forward_config().agent, 1000.).await
        });
        let req = next(&mut m.requests).await;
        m.frames
            .send(response(&req, data, retcode, status))
            .unwrap();
        assert_eq!(call.await.unwrap(), forward_event());
    }
    let mut missing = forward_event();
    missing["message"][1]["data"] = json!({});
    assert_eq!(
        resolve_forwards(&missing, &b, &forward_config().agent, 1000.).await,
        missing
    );
    let mut ignored = forward_event();
    ignored["group_id"] = json!(999);
    assert_eq!(
        resolve_forwards(&ignored, &b, &forward_config().agent, 1000.).await,
        ignored
    );
    assert!(m.requests.try_recv().is_err());
    stop(s, t).await;
}

#[tokio::test]
async fn forward_disabled_preserves_existing_behavior() {
    use qq_inner_core::engine::policy::{normalize, resolve_forwards};
    let m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = bot(&m, "a+b &?", "123", 10.0);
    let (s, t) = running(&b, &mut rx).await;
    let c = forward_config();
    assert!(!c.onebot.forward_enabled);
    assert!(!b.forward_enabled());
    let event = forward_event();
    let resolved = resolve_forwards(&event, &b, &c.agent, 1000.).await;
    assert_eq!(resolved, event);
    assert_eq!(
        normalize(&resolved, "123", &c.agent, 1000.).unwrap().text,
        "before  [forward]  after"
    );
    // 桥已转成中文占位文本的消息也逐字保留，不触发拉取。
    let mut placeholder = event.clone();
    placeholder["message"] = json!([{"type":"text","data":{"text":"[聊天记录]"}}]);
    let unchanged = resolve_forwards(&placeholder, &b, &c.agent, 1000.).await;
    assert_eq!(unchanged, placeholder);
    assert_eq!(
        normalize(&unchanged, "123", &c.agent, 1000.).unwrap().text,
        "[聊天记录]"
    );
    assert_eq!(
        normalize(&resolved, "123", &c.agent, 1000.).unwrap().text,
        normalize(&event, "123", &c.agent, 1000.).unwrap().text
    );
    assert_eq!(
        b.send_forward("group:456", vec![json!({"user_id":789,"id":42})])
            .await
            .unwrap_err()
            .code,
        "forward_disabled"
    );
    assert_eq!(
        b.get_forward_msg("opaque-forward").await.unwrap_err().code,
        "forward_disabled"
    );
    assert!(m.requests.is_empty());
    stop(s, t).await;
}

#[tokio::test]
async fn identity_actions_use_logged_in_account_and_exact_parameters() {
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = bot(&m, "a+b &?", "", 30.);
    let (shutdown, task) = running(&b, &mut rx).await;
    for (action, expected) in [
        (
            "set_group_card",
            json!({"group_id":"10","user_id":"123","card":"群名片"}),
        ),
        ("set_qq_profile", json!({"nickname":"昵称"})),
        ("set_qq_avatar", json!({"file":"file:///avatar.png"})),
        ("set_self_longnick", json!({"longNick":"好奇共学"})),
    ] {
        let client = b.clone();
        let request = tokio::spawn(async move {
            match action {
                "set_group_card" => client.set_group_card("10", "群名片").await,
                "set_qq_profile" => client.set_qq_profile("昵称").await,
                "set_qq_avatar" => client.set_qq_avatar("file:///avatar.png").await,
                _ => client.set_signature("好奇共学").await,
            }
        });
        let packet = next(&mut m.requests).await;
        assert_eq!(packet["action"], action);
        assert_eq!(packet["params"], expected);
        m.frames
            .send(response(&packet, json!({}), json!(0), "ok"))
            .unwrap();
        request.await.unwrap().unwrap();
    }
    stop(shutdown, task).await;
}

#[tokio::test]
async fn backfill_forward_skips_age_but_keeps_admission_checks() {
    use qq_inner_core::engine::policy::{
        normalize_backfill, resolve_forwards, resolve_forwards_backfill,
    };
    let mut m = mock(json!({"user_id":123}), json!(true), 0).await;
    let (b, mut rx) = forward_bot(&m);
    let (s, t) = running(&b, &mut rx).await;
    let mut c = forward_config();
    c.agent.active_window_seconds = 10.;
    c.agent.ignored_users = vec!["888".into()];
    let mut old = forward_event();
    old["time"] = json!(1);
    assert_eq!(resolve_forwards(&old, &b, &c.agent, 1000.).await, old);
    for (key, value) in [
        ("time", json!(1061)),
        ("time", json!("invalid")),
        ("message_id", json!(null)),
        ("message_id", json!("")),
        ("group_id", json!(999)),
        ("user_id", json!(123)),
        ("user_id", json!(888)),
        ("user_id", json!(null)),
        ("self_id", json!(999)),
        ("post_type", json!("notice")),
        ("message_type", json!("unknown")),
    ] {
        let mut invalid = old.clone();
        invalid[key] = value;
        assert_eq!(
            resolve_forwards_backfill(&invalid, &b, &c.agent, 1000., true).await,
            invalid,
            "{key}"
        );
    }
    assert!(m.requests.try_recv().is_err());
    let a = b.clone();
    let call = tokio::spawn(async move {
        let resolved = resolve_forwards_backfill(&old, &a, &c.agent, 1000., true).await;
        let full = normalize_backfill(&resolved, "123", &c.agent, 1000.).unwrap();
        c.agent.max_input_chars = 45.;
        let clipped = normalize_backfill(&resolved, "123", &c.agent, 1000.).unwrap();
        assert!(clipped.text.contains("[合并转发]"));
        assert!(clipped.text.ends_with("[/合并转发]"));
        assert!(clipped.text.chars().count() <= 45);
        full
    });
    let req = next(&mut m.requests).await;
    assert_eq!(req["action"], "get_forward_msg");
    m.frames.send(response(&req, json!({"messages":[{"sender":{"nickname":"历史作者"},"content":[{"type":"text","data":{"text":"很久以前的外部对话，足够长以验证截断保留边界"}}]}]}), json!(0), "ok")).unwrap();
    let message = call.await.unwrap();
    assert_eq!(message.ts, 1.);
    assert!(message.text.contains("历史作者: 很久以前的外部对话"));
    assert!(message.text.contains("[/合并转发]"));
    stop(s, t).await;
}

#[tokio::test]
async fn json_cards_parse_fields_and_fall_back_under_forward_switch() {
    use qq_inner_core::engine::policy::{normalize, resolve_forwards, resolve_forwards_backfill};
    for enabled in [true, false] {
        let m = mock(json!({"user_id":123}), json!(true), 0).await;
        let (b, mut rx) = if enabled {
            forward_bot(&m)
        } else {
            bot(&m, "a+b &?", "123", 10.0)
        };
        let (s, t) = running(&b, &mut rx).await;
        let c = forward_config();
        for (card, expected) in [
            (json!({"title":"标题","desc":"描述","summary":"摘要","prompt":"提示"}).to_string(), "[卡片] 标题 — 描述 — 摘要 — 提示"),
            (json!({"meta":{"news":{"title":"新闻","desc":"详情","summary":"详情","url":"https://example.com"}}}).to_string(), "[卡片] 新闻 — 详情"),
            ("invalid json".into(), "[json]"),
            (json!({"meta":{"title":123,"desc":" "}}).to_string(), "[json]"),
            ("null".into(), "[json]"),
        ] {
            let mut event = forward_event();
            event["message"] = json!([{"type":"json","data":{"data":card}}]);
            for backfill in [false, true] {
                let resolved = if backfill {
                    resolve_forwards_backfill(&event, &b, &c.agent, 1000., enabled).await
                } else {
                    resolve_forwards(&event, &b, &c.agent, 1000.).await
                };
                assert_eq!(normalize(&resolved, "123", &c.agent, 1000.).unwrap().text, if enabled { expected } else { "[json]" });
                assert_ne!(resolved["message"][0]["type"], "text");
            }
        }
        assert!(m.requests.is_empty());
        stop(s, t).await;
    }
}
