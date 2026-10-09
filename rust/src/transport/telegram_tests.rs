//! Telegram 传输的离线测试：本地 TcpListener mock HTTP，不发真实网络请求。
use super::*;
use crate::config;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
};

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

/// mock 请求处理器：`(method, body) -> (status, response body)`。
type Handler = Arc<dyn Fn(&str, &Value) -> (u16, String) + Send + Sync>;

impl Mock {
    fn new(handler: impl Fn(&str, &Value) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let handler: Handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|value| value.parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut bytes = vec![0; length];
                let _ = reader.read_exact(&mut bytes);
                let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                let method = request_line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|path| path.rsplit('/').next())
                    .unwrap_or("")
                    .to_string();
                captured
                    .lock()
                    .unwrap()
                    .push((method.clone(), body.clone()));
                let (status, response) = handler(&method, &body);
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} Mock\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn last(&self) -> (String, Value) {
        self.requests.lock().unwrap().last().cloned().unwrap()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn ok(result: Value) -> (u16, String) {
    (200, json!({"ok": true, "result": result}).to_string())
}

fn fail(status: u16, code: u64, description: &str) -> (u16, String) {
    (
        status,
        json!({"ok": false, "error_code": code, "description": description}).to_string(),
    )
}

fn telegram_config() -> config::Telegram {
    serde_json::from_value(json!({"enabled": true})).unwrap()
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "qq-inner-telegram-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn telegram(
    mock: &Mock,
    settings: config::Telegram,
) -> (Telegram, mpsc::UnboundedReceiver<Notification>) {
    let (bot, rx) = Telegram::new(settings, "TEST:TOKEN".into(), temp_dir());
    (bot.with_base_url(mock.url.clone()), rx)
}

fn identity() -> (u16, String) {
    ok(json!({"id":42,"is_bot":true,"first_name":"Test","username":"test_bot"}))
}

// ---------------------------------------------------------------------------
// 纯函数翻译
// ---------------------------------------------------------------------------

#[test]
fn translates_private_text_message() {
    let update = json!({"update_id":1,"message":{
        "message_id":7,
        "from":{"id":99,"is_bot":false,"first_name":"Alice","last_name":"Smith","username":"alice"},
        "chat":{"id":99,"type":"private","first_name":"Alice"},
        "date":1700000000,"text":"hello"}});
    let event = update_to_event(&update, "42", "test_bot").unwrap();
    assert_eq!(event["post_type"], "message");
    assert_eq!(event["message_type"], "private");
    assert_eq!(event["user_id"], 99);
    assert_eq!(event["self_id"], "42");
    assert_eq!(event["time"], 1700000000);
    assert_eq!(event["message_id"], 7);
    assert!(event.get("group_id").is_none());
    assert_eq!(event["sender"]["nickname"], "Alice Smith");
    assert_eq!(event["sender"]["card"], "alice");
    assert_eq!(
        event["message"],
        json!([{"type":"text","data":{"text":"hello"}}])
    );
}

#[test]
fn translates_group_negative_id_and_bot_mention() {
    let update = json!({"update_id":2,"message":{
        "message_id":8,"from":{"id":99,"is_bot":false,"first_name":"Bob"},
        "chat":{"id":-1001234567890_i64,"type":"supergroup","title":"Test Group"},
        "date":1700000001,"text":"@test_bot hi",
        "entities":[{"type":"mention","offset":0,"length":9}]}});
    let event = update_to_event(&update, "42", "test_bot").unwrap();
    assert_eq!(event["message_type"], "group");
    assert_eq!(event["group_id"], -1001234567890_i64);
    assert_eq!(event["sender"]["nickname"], "Bob");
    assert!(event["sender"].get("card").is_none());
    assert_eq!(
        event["message"],
        json!([
            {"type":"at","data":{"qq":"42"}},
            {"type":"text","data":{"text":" hi"}}
        ])
    );
}

#[test]
fn utf16_entity_offsets_are_not_utf8() {
    // "😀" 占 2 个 UTF-16 码元；实体从第 3 个码元开始，而不是第 4 个字节。
    let update = json!({"update_id":3,"message":{
        "message_id":9,"from":{"id":99,"first_name":"Emoji"},
        "chat":{"id":-500,"type":"group","title":"G"},
        "date":1700000002,"text":"😀 @alice",
        "entities":[{"type":"mention","offset":3,"length":6}]}});
    let event = update_to_event(&update, "42", "test_bot").unwrap();
    assert_eq!(
        event["message"],
        json!([
            {"type":"text","data":{"text":"😀 "}},
            {"type":"text","data":{"text":"@alice"}}
        ])
    );
}

#[test]
fn text_mention_becomes_at_segment() {
    let update = json!({"update_id":4,"message":{
        "message_id":10,"from":{"id":99,"first_name":"A"},
        "chat":{"id":-500,"type":"group","title":"G"},
        "date":1700000003,"text":"hey @carol",
        "entities":[{"type":"text_mention","offset":4,"length":6,"user":{"id":777,"first_name":"Carol"}}]}});
    let event = update_to_event(&update, "42", "test_bot").unwrap();
    assert_eq!(
        event["message"],
        json!([
            {"type":"text","data":{"text":"hey "}},
            {"type":"at","data":{"qq":"777"}}
        ])
    );
}

#[test]
fn reply_to_bot_is_reply_plus_at_self() {
    let update = json!({"update_id":5,"message":{
        "message_id":11,"from":{"id":99,"first_name":"A"},
        "chat":{"id":99,"type":"private","first_name":"A"},
        "date":1700000004,"text":"thanks",
        "reply_to_message":{"message_id":5,"from":{"id":42,"is_bot":true,"first_name":"Test"},"date":1700000000,"text":"hi"}}});
    let event = update_to_event(&update, "42", "test_bot").unwrap();
    assert_eq!(
        event["message"],
        json!([
            {"type":"reply","data":{}},
            {"type":"at","data":{"qq":"42"}},
            {"type":"text","data":{"text":"thanks"}}
        ])
    );
}

#[test]
fn media_caption_location_and_forward_markers() {
    let photo = json!({"update_id":6,"message":{
        "message_id":12,"from":{"id":99,"first_name":"A"},
        "chat":{"id":99,"type":"private"},"date":1,"photo":[{"file_id":"x"}]}});
    assert_eq!(
        update_to_event(&photo, "42", "test_bot").unwrap()["message"],
        json!([{"type":"image"}])
    );

    let caption = json!({"update_id":7,"message":{
        "message_id":13,"from":{"id":99,"first_name":"A"},
        "chat":{"id":99,"type":"private"},"date":1,"photo":[{"file_id":"x"}],"caption":"look"}});
    assert_eq!(
        update_to_event(&caption, "42", "test_bot").unwrap()["message"],
        json!([
            {"type":"image"},
            {"type":"text","data":{"text":"look"}}
        ])
    );

    let location = json!({"update_id":8,"message":{
        "message_id":14,"from":{"id":99,"first_name":"A"},
        "chat":{"id":99,"type":"private"},"date":1,"location":{"latitude":1.0,"longitude":2.0}}});
    assert_eq!(
        update_to_event(&location, "42", "test_bot").unwrap()["message"],
        json!([{"type":"text","data":{"text":"[location]"}}])
    );

    let forwarded = json!({"update_id":9,"message":{
        "message_id":15,"from":{"id":99,"first_name":"A"},
        "chat":{"id":99,"type":"private"},"date":1,"text":"hi",
        "forward_origin":{"type":"hidden_user","sender_user_name":"Ghost"}}});
    assert_eq!(
        update_to_event(&forwarded, "42", "test_bot").unwrap()["message"],
        json!([
            {"type":"text","data":{"text":"[forwarded from Ghost]"}},
            {"type":"text","data":{"text":"hi"}}
        ])
    );
}

#[test]
fn service_and_unknown_updates_are_ignored() {
    let service = json!({"update_id":10,"message":{
        "message_id":16,"from":{"id":99,"first_name":"A"},
        "chat":{"id":-1,"type":"group","title":"G"},"date":1,
        "new_chat_members":[{"id":5,"first_name":"New"}]}});
    assert!(update_to_event(&service, "42", "test_bot").is_none());
    assert!(update_to_event(
        &json!({"update_id":11,"edited_message":{"message_id":1}}),
        "42",
        "test_bot"
    )
    .is_none());
    assert!(update_to_event(
        &json!({"update_id":12,"callback_query":{"id":"x"}}),
        "42",
        "test_bot"
    )
    .is_none());
    // 非数字 bot id 与缺失文本都不会 panic。
    assert!(update_to_event(
        &json!({"update_id":13,"message":{"from":{"id":1},"chat":{"id":1,"type":"private"}}}),
        "42",
        "test_bot"
    )
    .is_none());
}

#[test]
fn my_chat_member_join_is_group_increase() {
    let update = json!({"update_id":14,"my_chat_member":{
        "chat":{"id":-100500,"type":"supergroup","title":"G"},
        "from":{"id":99,"first_name":"A"},
        "date":1700000005,
        "old_chat_member":{"status":"left","user":{"id":42,"is_bot":true}},
        "new_chat_member":{"status":"member","user":{"id":42,"is_bot":true}}}});
    let event = update_to_event(&update, "42", "test_bot").unwrap();
    assert_eq!(event["post_type"], "notice");
    assert_eq!(event["notice_type"], "group_increase");
    assert_eq!(event["group_id"], -100500);
    assert_eq!(event["user_id"], "42");
    assert_eq!(event["self_id"], "42");
    assert_eq!(event["time"], 1700000005);

    let unchanged = json!({"update_id":15,"my_chat_member":{
        "chat":{"id":-100500,"type":"supergroup"},
        "old_chat_member":{"status":"member"},"new_chat_member":{"status":"member"}}});
    assert!(update_to_event(&unchanged, "42", "test_bot").is_none());
}

#[test]
fn entity_offsets_do_not_overflow() {
    let text = "hello @alice";
    // offset/length 为 i64::MAX 时 checked_add 必须跳过实体而不是 panic 或回绕。
    let entities = [
        json!({"type":"mention","offset":i64::MAX,"length":i64::MAX}),
        json!({"type":"text_mention","offset":6,"length":i64::MAX,"user":{"id":777}}),
    ];
    let segments = entity_segments(text, &entities, "42", "test_bot");
    // 两个实体都越界跳过，整段原文保留。
    assert_eq!(
        json!(segments),
        json!([{"type":"text","data":{"text":"hello @alice"}}])
    );
}

#[test]
fn classify_conflict_variants_and_transient_codes() {
    let webhook = classify(
        409,
        &json!({"ok":false,"error_code":409,"description":"Conflict: can't use getUpdates method while webhook is active"}),
    );
    assert_eq!(webhook.code, "webhook_conflict");
    assert!(!webhook.uncertain);
    assert_eq!(webhook.retry_after, None);

    let conflict = classify(
        409,
        &json!({"ok":false,"error_code":409,"description":"Conflict"}),
    );
    assert_eq!(conflict.code, "telegram_conflict");
    assert!(!conflict.uncertain);

    let limited = classify(
        429,
        &json!({"ok":false,"error_code":429,"parameters":{"retry_after":7}}),
    );
    assert_eq!(limited.code, "rate_limited");
    assert_eq!(limited.retry_after, Some(7));

    let server = classify(500, &json!({"ok":false,"error_code":500}));
    assert_eq!(server.code, "telegram_server_error");
    assert!(server.uncertain);
}

#[tokio::test]
async fn rate_limit_backoff_waits_retry_after() {
    let mock = Mock::new(|method, _| {
        match method {
        "getMe" => identity(),
        "getUpdates" => (
            429,
            json!({"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":7}})
                .to_string(),
        ),
        _ => ok(json!(true)),
    }
    });
    let (mut bot, _rx) = telegram(&mock, telegram_config());
    bot.random = Arc::new(|| 0.0);
    let delays = Arc::new(Mutex::new(Vec::new()));
    let captured = delays.clone();
    let (stop, stop_rx) = watch::channel(false);
    let stopper = stop.clone();
    let count = Arc::new(AtomicU64::new(0));
    let counter = count.clone();
    bot.sleep = Some(Arc::new(move |duration| {
        captured.lock().unwrap().push(duration.as_secs_f64());
        if counter.fetch_add(1, Ordering::SeqCst) >= 1 {
            let _ = stopper.send(true);
        }
    }));
    let runner = bot.clone();
    let task = tokio::spawn(async move { runner.run(stop_rx).await });
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    let delays = delays.lock().unwrap().clone();
    assert!(delays[0] >= 7.0, "{delays:?}");
}

#[tokio::test]
async fn transport_error_is_uncertain_network_error() {
    // 先占端口再释放，保证连接被拒绝（transport error）而不是 HTTP 响应。
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let (bot, _rx) = Telegram::new(telegram_config(), "T:TOKEN".into(), temp_dir());
    let bot = bot.with_base_url(format!("http://{addr}"));
    bot.set_identity_for_test("42", "test_bot");
    let error = bot.send("private:5", "hi", None).await.unwrap_err();
    assert_eq!(error.code, "telegram_network_error");
    assert!(error.uncertain);
}

// ---------------------------------------------------------------------------
// HTTP 传输
// ---------------------------------------------------------------------------

#[tokio::test]
async fn check_fills_identity_and_clears_offline() {
    let mock = Mock::new(|method, _| match method {
        "getMe" => ok(
            json!({"id":123456789_i64,"is_bot":true,"first_name":"Tang","username":"example_bot"}),
        ),
        _ => ok(json!(true)),
    });
    let (bot, _rx) = telegram(&mock, telegram_config());
    let state = bot.check().await.unwrap();
    assert_eq!(state.self_id, "123456789");
    assert_eq!(state.username, "example_bot");
    assert!(state.connected && state.online);
    assert_eq!(mock.count(), 1);
    assert_eq!(mock.last().0, "getMe");
}

#[tokio::test]
async fn check_reports_unauthorized() {
    let mock = Mock::new(|_, _| fail(401, 401, "Unauthorized"));
    let (bot, _rx) = telegram(&mock, telegram_config());
    let error = bot.check().await.unwrap_err();
    assert_eq!(error.code, "telegram_unauthorized");
    assert!(!error.uncertain);
    assert_eq!(error.retry_after, None);
    assert!(!bot.state().connected);
}

#[tokio::test]
async fn send_success_offline_and_payload() {
    let mock = Mock::new(|method, _| match method {
        "getMe" => identity(),
        "sendMessage" => ok(json!({"message_id":77})),
        _ => ok(json!(true)),
    });
    let (bot, _rx) = telegram(&mock, telegram_config());
    // 未认证时明确 telegram_offline，不发请求。
    let error = bot.send("private:5", "hi", None).await.unwrap_err();
    assert_eq!(error.code, "telegram_offline");
    assert_eq!(mock.count(), 0);

    bot.check().await.unwrap();
    let result = bot.send("group:-100123", "hello", Some("1")).await.unwrap();
    assert_eq!(result, json!({"message_id":77}));
    let (method, params) = mock.last();
    assert_eq!(method, "sendMessage");
    assert_eq!(params["chat_id"], -100123);
    assert_eq!(params["text"], "hello");
    assert_eq!(params["disable_web_page_preview"], true);

    for chat in ["", "room:1", "group:", "private:-5", "group:abc", "group:0"] {
        assert_eq!(
            bot.send(chat, "hi", None).await.unwrap_err().code,
            "invalid_chat"
        );
    }
    assert_eq!(
        bot.send("private:5", "", None).await.unwrap_err().code,
        "invalid_text"
    );
}

#[tokio::test]
async fn send_classifies_rate_limit_and_unauthorized() {
    let limited = Mock::new(|method, _| {
        match method {
        "getMe" => identity(),
        "sendMessage" => (
            429,
            json!({"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":7}}).to_string(),
        ),
        _ => ok(json!(true)),
    }
    });
    let (bot, _rx) = telegram(&limited, telegram_config());
    bot.check().await.unwrap();
    let error = bot.send("private:5", "hi", None).await.unwrap_err();
    assert_eq!(error.code, "rate_limited");
    assert!(!error.uncertain);

    let unauthorized = Mock::new(|method, _| match method {
        "getMe" => identity(),
        _ => fail(401, 401, "Unauthorized"),
    });
    let (bot, _rx) = telegram(&unauthorized, telegram_config());
    bot.check().await.unwrap();
    let error = bot.send("private:5", "hi", None).await.unwrap_err();
    assert_eq!(error.code, "telegram_unauthorized");
    assert!(!error.uncertain);

    let broken = Mock::new(|method, _| match method {
        "getMe" => identity(),
        _ => fail(500, 500, "Internal Server Error"),
    });
    let (bot, _rx) = telegram(&broken, telegram_config());
    bot.check().await.unwrap();
    let error = bot.send("private:5", "hi", None).await.unwrap_err();
    assert_eq!(error.code, "telegram_server_error");
    // 5xx 可能已经执行；调用方不得自动重发。
    assert!(error.uncertain);
}

#[tokio::test]
async fn call_maps_actions_and_rejects_unsupported() {
    let mock = Mock::new(|method, body| match method {
        "getMe" => identity(),
        "getChat" => ok(json!({
            "id": body["chat_id"],
            "title": "Test Group",
            "pinned_message": {"text": "pin"}
        })),
        "getChatMemberCount" => ok(json!(12)),
        "getChatMember" => ok(json!({
            "user": {"id":5,"first_name":"Alice","last_name":"Smith","username":"alice"}
        })),
        _ => ok(json!(true)),
    });
    let (bot, _rx) = telegram(&mock, telegram_config());
    let login = bot.call("get_login_info", json!({})).await.unwrap();
    assert_eq!(login["user_id"], 42);
    assert_eq!(login["nickname"], "Test");

    let info = bot
        .call("get_group_info", json!({"group_id": -100.0}))
        .await
        .unwrap();
    assert_eq!(info["group_id"], -100);
    assert_eq!(info["group_name"], "Test Group");
    assert_eq!(info["member_count"], 12);
    assert_eq!(info["max_member_count"], 0);

    // 引擎会把群 id 传成 f64；必须规整为整数再发，避免 `-100.0` 被 Telegram 拒绝。
    let (method, params) = mock.last();
    assert_eq!(method, "getChatMemberCount");
    assert_eq!(params["chat_id"], -100);

    let member = bot
        .call(
            "get_group_member_info",
            json!({"group_id": "-100", "user_id": 5.0}),
        )
        .await
        .unwrap();
    assert_eq!(member["nickname"], "Alice Smith");
    assert_eq!(member["card"], "alice");
    let (method, params) = mock.last();
    assert_eq!(method, "getChatMember");
    assert_eq!(params["chat_id"], -100);
    assert_eq!(params["user_id"], 5);

    let notices = bot
        .call("_get_group_notice", json!({"group_id":-100}))
        .await
        .unwrap();
    assert_eq!(notices, json!([{"message":{"text":"pin"}}]));

    for action in [
        "get_group_member_list",
        "get_group_msg_history",
        "get_forward_msg",
        "send_group_msg",
    ] {
        assert_eq!(
            bot.call(action, json!({})).await.unwrap_err().to_string(),
            "unsupported_action"
        );
    }
}

#[tokio::test]
async fn offset_persists_across_restart() {
    let mock = Mock::new(|method, _| match method {
        "getUpdates" => ok(json!([
            {"update_id":100,"message":{"message_id":1,"from":{"id":9,"first_name":"A"},"chat":{"id":9,"type":"private"},"date":1,"text":"a"}},
            {"update_id":101,"message":{"message_id":2,"from":{"id":9,"first_name":"A"},"chat":{"id":9,"type":"private"},"date":1,"text":"b"}}
        ])),
        _ => ok(json!(true)),
    });
    let dir = temp_dir();
    let (bot, _rx) = Telegram::new(telegram_config(), "T".into(), dir.clone());
    let bot = bot.with_base_url(mock.url.clone());
    let updates = bot.poll_once().await.unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(*bot.offset.lock().unwrap(), Some(102));
    let stored = crate::settings::read_json(&dir.join("telegram-offset.json"))
        .unwrap()
        .unwrap();
    assert_eq!(stored["offset"], 102);

    let (restarted, _rx2) = Telegram::new(telegram_config(), "T".into(), dir.clone());
    let restarted = restarted.with_base_url(mock.url.clone());
    assert_eq!(*restarted.offset.lock().unwrap(), Some(102));
    let _ = restarted.poll_once().await.unwrap();
    let (method, params) = mock.last();
    assert_eq!(method, "getUpdates");
    assert_eq!(params["offset"], 102);
    assert_eq!(params["timeout"], 20);
    assert_eq!(
        params["allowed_updates"],
        json!(["message", "my_chat_member"])
    );
}

#[tokio::test]
async fn run_backs_off_and_stops_on_shutdown() {
    let mock = Mock::new(|method, _| match method {
        "getMe" => identity(),
        "getUpdates" => fail(500, 500, "boom"),
        _ => ok(json!(true)),
    });
    let (mut bot, mut rx) = telegram(&mock, telegram_config());
    bot.random = Arc::new(|| 0.0);
    let delays = Arc::new(Mutex::new(Vec::new()));
    let captured = delays.clone();
    let (stop, stop_rx) = watch::channel(false);
    let stopper = stop.clone();
    let count = Arc::new(AtomicU64::new(0));
    let counter = count.clone();
    bot.sleep = Some(Arc::new(move |duration| {
        captured.lock().unwrap().push(duration.as_secs_f64());
        if counter.fetch_add(1, Ordering::SeqCst) >= 2 {
            let _ = stopper.send(true);
        }
    }));
    let runner = bot.clone();
    let task = tokio::spawn(async move { runner.run(stop_rx).await });
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*delays.lock().unwrap(), vec![1.0, 2.0, 4.0]);
    assert!(bot.state().reconnects >= 2);
    let mut statuses = Vec::new();
    while let Ok(notification) = rx.try_recv() {
        if let Notification::Status(code) = notification {
            statuses.push(code);
        }
    }
    assert!(statuses.iter().any(|code| code == "connected"));
    assert!(statuses.iter().any(|code| code == "telegram_server_error"));
}

#[tokio::test]
async fn unauthorized_chat_is_recorded_and_logged() {
    let update = json!({"update_id":1,"message":{
        "message_id":1,"from":{"id":99,"first_name":"Eve","username":"eve"},
        "chat":{"id":-555,"type":"group","title":"Secret"},
        "date":1700000000,"text":"hello"}});
    let mock = Mock::new(|method, _| match method {
        "getMe" => identity(),
        _ => ok(json!(true)),
    });
    let (mut bot, _rx) = telegram(&mock, telegram_config());
    let logs = Arc::new(Mutex::new(Vec::new()));
    let captured = logs.clone();
    bot.log = Arc::new(move |event, data| {
        captured
            .lock()
            .unwrap()
            .push((event.to_string(), data.clone()))
    });
    bot.check().await.unwrap();
    bot.dispatch(&update);

    let seen = bot.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].chat, "group:-555");
    assert_eq!(seen[0].title, "Secret");
    assert_eq!(seen[0].user_id, "99");
    assert_eq!(seen[0].username, "eve");
    assert_eq!(seen[0].nickname, "Eve");
    assert!(logs
        .lock()
        .unwrap()
        .iter()
        .any(|(event, data)| { event == "telegram_chat_ignored" && data["chat"] == "group:-555" }));
    assert_eq!(bot.take_seen().len(), 1);
    assert!(bot.seen().is_empty());

    // 白名单内的 chat 不进入 seen registry。
    let mut allowed = telegram_config();
    allowed.allowed_groups = vec!["-555".into()];
    let (allowed_bot, _rx2) = telegram(&mock, allowed);
    allowed_bot.check().await.unwrap();
    allowed_bot.dispatch(&update);
    assert!(allowed_bot.seen().is_empty());
}
