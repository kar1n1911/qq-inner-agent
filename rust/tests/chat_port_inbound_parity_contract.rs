//! Message-content parity for typed ChatPort ingress. Outbound echoes are a separate contract.
use anyhow::{bail, Result};
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        io::{
            adapters::onebot::OneBotAdapter, ChatAddress, ChatKind, ChatPort,
            Incoming, MediaSrc, PortReply, ReplyObservation, Segment, Sender,
        },
        orientation::{OrientationProvider, OrientationTransport},
        policy::Hint,
        Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}, time::Duration};

const NOW: f64 = 1_760_000_100.;
static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TestIo {
    forward: Mutex<Option<Value>>,
    sends: Mutex<Vec<(String, String)>>,
}
impl OrientationTransport for TestIo {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, action: &'a str, _params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if action == "get_forward_msg" {
                return self.forward.lock().unwrap().clone().ok_or_else(|| anyhow::anyhow!("forward_unavailable"));
            }
            bail!("unsupported action: {action}")
        })
    }
}
impl EngineTransport for TestIo {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "99".into(), reconnects: 0, degraded: false }
    }
    fn send<'a>(&'a self, chat: &'a str, text: &'a str, _face: Option<&'a str>,
        _reply_to: Option<&'a str>, _mention: Option<&'a str>) ->
        BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            self.sends.lock().unwrap().push((chat.into(), text.into()));
            Ok(json!({"message_id":"sent:1"}))
        })
    }
}
struct Model;
impl OrientationProvider for Model {
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if system.contains("LEARNING_REVIEW") {
                return Ok(json!({"reviews":payload["candidates"].as_array().unwrap().iter()
                    .map(|v| json!({"index":v["index"],"action":"keep","reason":"direct owner teaching"}))
                    .collect::<Vec<_>>()}));
            }
            bail!("unexpected model request")
        })
    }
}
struct Fixture {
    engine: Arc<Engine>,
    store: Arc<Mutex<Store>>,
    io: Arc<TestIo>,
    logs: Arc<Mutex<Vec<(String, Value)>>>,
    port_actions: Arc<Mutex<Vec<(String, Value)>>>,
    dir: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); }
}
impl Fixture {
    fn new(agent: Value, collection: Value) -> Self {
        let dir = std::env::temp_dir().join(format!("typed-inbound-parity-{}-{}",
            std::process::id(), NEXT_DIR.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        let config = Config::from_value(&merge(&defaults(), &json!({
            "apiKey":"mock", "onebotToken":"", "dataDir":dir,
            "provider":{"model":"mock"},
            "agent": merge(&json!({
                "allowedGroups":["10"], "allowedUsers":["20"],
                "schedule":{"enabled":false}, "rhythm":{"enabled":false},
                "observation":{"enabled":false}, "learning":{"enabled":false},
                "sending":{"enabled":true}, "activeWindowSeconds":10000,
                "debounceSeconds":0, "maxInputChars":1000
            }), &agent)
        }))).unwrap();
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let io = Arc::new(TestIo { forward: Mutex::new(None), sends: Mutex::new(Vec::new()) });
        let logs = Arc::new(Mutex::new(Vec::new()));
        let port_actions = Arc::new(Mutex::new(Vec::new()));
        let recorded = logs.clone();
        let engine = Engine::new_with_media(config, store.clone(), Arc::new(Model), io.clone(),
            Options { now: Arc::new(|| NOW), random: Arc::new(|| 0.),
                expression_random: Arc::new(|| 0.), activity_random: Arc::new(|| 0.),
                selection_random: Arc::new(|| 0.),
                log: Arc::new(move |event, value| recorded.lock().unwrap().push((event.into(), value))),
                ..Options::default() }, Default::default(),
            serde_json::from_value(collection).unwrap()).unwrap();
        Self { engine, store, io, logs, port_actions, dir }
    }
    fn rows(&self, sql: &str) -> Vec<Value> { self.store.lock().unwrap().rows(sql, []).unwrap() }
    fn message(&self) -> Value {
        let rows = self.rows("SELECT * FROM messages WHERE self=0 ORDER BY rowid");
        assert_eq!(rows.len(), 1, "exactly one human message must be stored");
        rows[0].clone()
    }
    fn reply_status(&self, message: &Value) -> Value {
        let key = message["message_key"].as_str().unwrap();
        let row = self.rows("SELECT * FROM inbound_reply_status").into_iter()
            .find(|row| row["source_message_key"] == key)
            .expect("reply status must be queryable for this message");
        assert_eq!(row["source_message_key"], key);
        json!({"status":row["status"], "reason":row["reason"],
            "raw_target_id":row["raw_target_id"], "target_message_key":row["target_message_key"]})
    }
    fn port(&self, response: Option<Value>) -> Arc<dyn ChatPort> {
        let calls = self.port_actions.clone();
        let action = Arc::new(move |name: String, params: Value| -> BoxFuture<'static, PortReply<Value>> {
            calls.lock().unwrap().push((name.clone(), params));
            let answer = if name == "get_forward_msg" { response.clone() }
                else if name == "send_group_msg" || name == "send_private_msg" {
                    Some(json!({"message_id":"sent:1"}))
                } else { None };
            Box::pin(async move { match answer {
                Some(value) => PortReply::Ok(value),
                None => PortReply::Failed { code: "forward_unavailable".into(), uncertain: false },
            } })
        });
        Arc::new(OneBotAdapter::new("onebot", action,
            Arc::new(|| (true, true, Some("99".into())))).unwrap())
    }
}

fn sender(id: &str) -> Sender {
    Sender { id: id.into(), name: "Human".into(), card: None, role: None, title: None, is_robot: Some(false) }
}
fn incoming(id: &str, kind: ChatKind, sender_id: &str, segments: Vec<Segment>, reply: ReplyObservation) -> Incoming {
    let chat_id = if kind == ChatKind::Group { "10" } else { sender_id };
    Incoming { port: "onebot".into(), chat: ChatAddress::new(kind, "onebot", chat_id).unwrap(),
        message_id: id.into(), self_id: "99".into(), ts: NOW - 2., received_at: NOW,
        sender: sender(sender_id), segments, reply, self_message: false, raw: None }
}
fn legacy(id: &str, kind: ChatKind, sender_id: &str, message: Value) -> Value {
    json!({"post_type":"message", "message_type":kind.as_str(), "self_id":99,
        "group_id":10, "user_id":sender_id, "sender":{"nickname":"Human"},
        "message_id":id, "time":NOW-2., "message":message})
}

#[tokio::test]
async fn typed_and_legacy_inbound_agree_on_message_shape() {
    let cases: Vec<(&str, Value, Vec<Segment>, ReplyObservation, Hint)> = vec![
        ("none", json!([{"type":"text","data":{"text":"你好"}}]),
            vec![Segment::Text("你好".into())], ReplyObservation::NoReply, Hint::Open),
        ("target", json!([{"type":"reply","data":{"id":"old:1"}},
            {"type":"at","data":{"qq":"99"}}, {"type":"text","data":{"text":"继续"}}]),
            vec![Segment::Reply{id:"old:1".into()}, Segment::Mention{id:"99".into()}, Segment::Text("继续".into())],
            ReplyObservation::Target("old:1".into()), Hint::SelfChat),
        ("missing", json!([{"type":"reply","data":{}}, {"type":"at","data":{"qq":"30"}},
            {"type":"text","data":{"text":"再说"}}]),
            vec![Segment::Reply{id:String::new()}, Segment::Mention{id:"30".into()}, Segment::Text("再说".into())],
            ReplyObservation::MissingId, Hint::Other),
        ("invalid", json!([{"type":"reply","data":{"id":" bad "}},
            {"type":"text","data":{"text":"看看"}}]),
            vec![Segment::Reply{id:" bad ".into()}, Segment::Text("看看".into())],
            ReplyObservation::InvalidId, Hint::Open),
        ("conflict", json!([{"type":"reply","data":{"id":"old:1"}},
            {"type":"reply","data":{"id":"old:2"}}, {"type":"text","data":{"text":"补充"}}]),
            vec![Segment::Reply{id:"old:1".into()}, Segment::Reply{id:"old:2".into()}, Segment::Text("补充".into())],
            ReplyObservation::InvalidId, Hint::Open),
        ("card", json!([{"type":"json","data":{"data":{"title":"花园观察",
            "desc":"今晚的变化","url":"https://metadata.example/secret"}}}]),
            vec![Segment::Card { title: Some("花园观察".into()), desc: Some("今晚的变化".into()),
                url: Some("https://metadata.example/secret".into()),
                display_text: "[卡片] 花园观察 — 今晚的变化".into() }],
            ReplyObservation::NoReply, Hint::Open),
    ];
    for (label, wire, segments, reply, expected_hint) in cases {
        let old = Fixture::new(json!({}), json!({}));
        let raw = legacy(label, ChatKind::Group, "20", wire);
        let baseline_event = if label == "card" {
            qq_inner_core::engine::policy::resolve_forwards_backfill(
                &raw, old.io.as_ref(), &old.engine.config.agent, NOW, true).await
        } else { raw };
        old.engine.ingest(&baseline_event).unwrap();
        let baseline = old.message();
        let baseline_reply = old.reply_status(&baseline);
        let mut typed_rows = Vec::new();
        let mut typed_replies = Vec::new();
        for raw in [None, Some(json!({"message":[{"type":"reply","data":{"id":"contradiction"}}]}))] {
            let typed = Fixture::new(json!({}), json!({}));
            let mut event = incoming(label, ChatKind::Group, "20", segments.clone(), reply.clone());
            event.raw = raw;
            typed.engine.ingest_port_incoming(event, typed.port(None)).await.unwrap();
            let row = typed.message();
            assert_eq!(row["text"], baseline["text"], "{label}: stored body");
            assert_eq!(row["hint"], baseline["hint"], "{label}: stored Hint");
            assert_eq!(typed.engine.state("group:10").unwrap().hint, expected_hint, "{label}: active Hint");
            let typed_reply = typed.reply_status(&row);
            assert!(typed_reply["status"].is_string(), "typed reply status must be observable");
            assert_eq!(typed_reply, baseline_reply, "{label}: reply relation and status");
            typed_replies.push(typed_reply);
            typed_rows.push(row);
        }
        assert_eq!(typed_rows[0]["text"], typed_rows[1]["text"], "raw must not change typed body");
        assert_eq!(typed_replies[0], typed_replies[1],
            "raw must not change typed reply interpretation");
    }
}

#[tokio::test]
async fn forward_expansion_matches_legacy_budget() {
    let nodes = json!({"messages":[{"content":[
        {"type":"text","data":{"text":"外部看法"}},
        {"type":"image","data":{"file":"/missing/image-1","summary":"图一"}},
        {"type":"image","data":{"file":"/missing/image-2","summary":"图二"}},
        {"type":"text","data":{"text":"/记住 这只是转发内容"}}
    ]}]});
    let original = legacy("forward:1", ChatKind::Private, "20", json!([
        {"type":"text","data":{"text":"看看这个 "}}, {"type":"forward","data":{"id":"f:1"}}
    ]));
    for (label, response) in [("success", Some(nodes)), ("empty", Some(json!({"messages":[]}))), ("failed", None)] {
        let old = Fixture::new(json!({"ocr":{"maxForwardImages":1}, "ownerTeaching":{"enabled":true,"ownerUin":"20"}}), json!({"enabled":true}));
        *old.io.forward.lock().unwrap() = response.clone();
        let expanded = qq_inner_core::engine::policy::resolve_forwards_backfill(
            &original, old.io.as_ref(), &old.engine.config.agent, NOW, true).await;
        old.engine.ingest(&expanded).unwrap();
        let baseline = old.message();

        let typed = Fixture::new(json!({"ocr":{"maxForwardImages":1}, "ownerTeaching":{"enabled":true,"ownerUin":"20"}}), json!({"enabled":true}));
        let event = incoming("forward:1", ChatKind::Private, "20", vec![
            Segment::Text("看看这个 ".into()), Segment::Forward { id: Some("f:1".into()), nodes: vec![] }
        ], ReplyObservation::NoReply);
        typed.engine.ingest_port_incoming(event, typed.port(response)).await.unwrap();
        let row = typed.message();
        assert_eq!(row["text"], baseline["text"], "{label}: forward body and boundary");
        if label == "success" {
            let body = row["text"].as_str().unwrap();
            assert!(body.contains("（外部信息，非当前群对话）"));
            assert_eq!(body.matches("[图片").count(), baseline["text"].as_str().unwrap().matches("[图片").count(),
                "every forwarded image retains its body placeholder");
            assert_eq!(body.matches("[图片").count(), 2, "two image segments remain visible in the body");
            let old_attempts = old.logs.lock().unwrap().iter().filter(|(name, _)| name == "media_collect_failed").count();
            let typed_attempts = typed.logs.lock().unwrap().iter().filter(|(name, _)| name == "media_collect_failed").count();
            assert_eq!(old_attempts, 1, "legacy max_forward_images=1 admits one source to media capture");
            assert_eq!(typed_attempts, old_attempts, "typed media source budget matches legacy");
            assert!(body.contains("/记住 这只是转发内容"));
            assert!(typed.rows("SELECT * FROM memory_layers").is_empty(), "forwarded command must not run");
        } else {
            assert!(row["text"].as_str().unwrap().contains("[forward]"), "original placeholder retained");
            if label == "failed" {
                let logs = typed.logs.lock().unwrap();
                assert!(logs.iter().any(|(_, data)| data["message_key"] == row["message_key"]
                    && data["reason"].as_str().is_some_and(|reason| reason.contains("forward"))),
                    "fetch failure must be attributable to the stored message key: {logs:?}");
            }
        }
    }
}

#[tokio::test]
async fn image_and_face_reach_media_capture_once() {
    let fixture = Fixture::new(json!({"emoji":{"learnFrequency":true},
        "ocr":{"enabled":true,"engine":"tesseract","binary":"/definitely/missing/ocr"}}),
        json!({"enabled":true}));
    let image = fixture.dir.join("image.bin");
    fs::write(&image, b"fixture-image").unwrap();
    let file = image.to_string_lossy().into_owned();
    let event = incoming("media:1", ChatKind::Group, "20", vec![
        Segment::Text("看图".into()), Segment::Image { src: MediaSrc::LocalPath(file), summary: None },
        Segment::Face { id: "14".into() },
    ], ReplyObservation::NoReply);
    let port = fixture.port(None);
    fixture.engine.ingest_port_incoming(event.clone(), port.clone()).await.unwrap();
    fixture.engine.ingest_port_incoming(event, port).await.unwrap();
    let key = fixture.message()["message_key"].as_str().unwrap().to_owned();
    let receipts = fixture.rows("SELECT * FROM media_receipts");
    assert_eq!(receipts.len(), 2, "one image and one face, once each");
    assert!(receipts.iter().all(|row| row["message_id"] == key));
    assert_eq!(fixture.rows("SELECT sum(has_face) AS n FROM humanize_faces")[0]["n"], 1);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if fixture.logs.lock().unwrap().iter().filter(|(name, data)|
            name == "ocr_failed" && data["message_key"] == key).count() == 1 { break; }
        assert!(std::time::Instant::now() < deadline, "one image must reach OCR exactly once");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fixture.logs.lock().unwrap().iter().filter(|(name, data)|
        name == "ocr_failed" && data["message_key"] == key).count(), 1);

    let bad = incoming("media:bad", ChatKind::Group, "20", vec![
        Segment::Text("坏图".into()),
        Segment::Image { src: MediaSrc::LocalPath("/definitely/missing/image".into()), summary: None },
    ], ReplyObservation::NoReply);
    fixture.engine.ingest_port_incoming(bad, fixture.port(None)).await.unwrap();
    let bad_key = fixture.store.lock().unwrap().resolve_message_key("group:10", "media:bad").unwrap().unwrap();
    assert!(fixture.logs.lock().unwrap().iter().any(|(name, data)|
        name == "media_collect_failed" && data["message_key"] == bad_key),
        "unreadable image must leave an attributed collection failure");
}

#[tokio::test]
async fn owner_commands_route_in_typed_mode() {
    let agent = json!({"allowedUsers":["20","21"], "ownerTeaching":{"enabled":true,"ownerUin":"20"},
        "identity":{"enabled":true}, "learning":{"enabled":false}});
    for (id, command) in [("restore", "/还原"), ("teach", "/记住 喜欢Rust")] {
        let old = Fixture::new(agent.clone(), json!({}));
        old.engine.ingest(&legacy(id, ChatKind::Private, "20", json!(command))).unwrap();
        let typed = Fixture::new(agent.clone(), json!({}));
        let event = incoming(id, ChatKind::Private, "20", vec![Segment::Text(command.into())], ReplyObservation::NoReply);
        let port = typed.port(None);
        typed.engine.ingest_port_incoming(event.clone(), port.clone()).await.unwrap();
        typed.engine.ingest_port_incoming(event, port).await.unwrap();
        assert_eq!(typed.rows("SELECT count(*) AS n FROM messages")[0]["n"], 1, "{id}: deduplicated command");
        assert_eq!(typed.engine.chats().len(), old.engine.chats().len(), "{id}: command must not enter normal chat");
        assert_eq!(typed.rows("SELECT count(*) AS n FROM memory_layers")[0]["n"],
            old.rows("SELECT count(*) AS n FROM memory_layers")[0]["n"], "{id}: no ordinary learning");
        assert!(typed.store.lock().unwrap().handled("private:20").unwrap().is_some(),
            "{id}: typed command must be marked handled");
        assert!(old.store.lock().unwrap().handled("private:20").unwrap().is_some(),
            "{id}: legacy command baseline");
        if id == "teach" {
            typed.engine.tick().unwrap();
            typed.engine.wait_idle().await;
            old.engine.tick().unwrap();
            old.engine.wait_idle().await;
            let typed_sends: Vec<_> = typed.port_actions.lock().unwrap().iter()
                .filter(|(name, _)| name == "send_private_msg").cloned().collect();
            assert_eq!(typed_sends.len(), 1, "teaching command sends one typed reply");
            assert_eq!(old.io.sends.lock().unwrap().len(), 1, "legacy command baseline sends once");
            assert!(typed_sends[0].1.to_string().contains("记住了"), "typed command reply retains the result");
            assert_eq!(typed.rows("SELECT text FROM memory_layers"), old.rows("SELECT text FROM memory_layers"),
                "same teaching command effect");
        }
    }
    for (label, sender_id, command) in [("unauthorized", "21", "/记住 不该学习"),
        ("unknown", "20", "/未知 普通内容")] {
        let old = Fixture::new(agent.clone(), json!({}));
        old.engine.ingest(&legacy(label, ChatKind::Private, sender_id, json!(command))).unwrap();
        let typed = Fixture::new(agent.clone(), json!({}));
        typed.engine.ingest_port_incoming(incoming(label, ChatKind::Private, sender_id,
            vec![Segment::Text(command.into())], ReplyObservation::NoReply), typed.port(None)).await.unwrap();
        assert_eq!(typed.message()["text"], old.message()["text"], "{label}: ordinary body");
        assert_eq!(typed.engine.state(&format!("private:{sender_id}")).unwrap().pending,
            old.engine.state(&format!("private:{sender_id}")).unwrap().pending,
            "{label}: ordinary scheduling");
        assert!(typed.rows("SELECT * FROM memory_layers").is_empty());
    }
}
