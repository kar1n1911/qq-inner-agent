//! Independent secondary evidence for an acknowledged outbound message.
use anyhow::{bail, Result};
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        io::{adapters::onebot::OneBotAdapter, ChatAddress, ChatKind, ChatPort, Command,
            CommandResult, Fetch, OutboundEvidence, OutboundEvidenceSource, PortEvent, PortReply},
        orientation::{OrientationProvider, OrientationTransport},
        Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

const NOW: f64 = 1_760_000_100.;
const ACK: &str = "ack:7";
const BODY: &str = "已经发出的正文";

#[derive(Default)]
struct Transport {
    sends: AtomicUsize,
    hang_readback: bool,
}
impl OrientationTransport for Transport {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, action: &'a str, _params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if action == "get_msg" && self.hang_readback {
                std::future::pending::<()>().await;
            }
            bail!("readback_unavailable")
        })
    }
}
impl EngineTransport for Transport {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "99".into(), reconnects: 0, degraded: false }
    }
    fn send<'a>(&'a self, _chat: &'a str, _text: &'a str, _face: Option<&'a str>,
        _reply_to: Option<&'a str>, _mention: Option<&'a str>) ->
        BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            self.sends.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"message_id":"unexpected-second-send"}))
        })
    }
}
struct Model;
impl OrientationProvider for Model {
    fn json<'a>(&'a self, _system: &'a str, _payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { bail!("unexpected_model_call") })
    }
}
struct Fixture {
    engine: Arc<Engine>,
    store: Arc<Mutex<Store>>,
    transport: Arc<Transport>,
    logs: Arc<Mutex<Vec<(String, Value)>>>,
}
impl Fixture {
    fn new(hang_readback: bool) -> Self {
        let config = Config::from_value(&merge(&defaults(), &json!({
            "apiKey":"mock", "onebotToken":"", "dataDir":"unused",
            "provider":{"model":"mock"},
            "agent":{"allowedGroups":["10"], "allowedUsers":["20"],
                "schedule":{"enabled":false}, "rhythm":{"enabled":false},
                "observation":{"enabled":false}, "learning":{"enabled":false},
                "sending":{"enabled":true}, "activeWindowSeconds":1000}
        }))).unwrap();
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let transport = Arc::new(Transport { hang_readback, ..Default::default() });
        let logs = Arc::new(Mutex::new(Vec::new()));
        let observed = logs.clone();
        let engine = Engine::new(config, store.clone(), Arc::new(Model), transport.clone(),
            Options { now: Arc::new(|| NOW), random: Arc::new(|| 0.),
                expression_random: Arc::new(|| 0.), activity_random: Arc::new(|| 0.),
                selection_random: Arc::new(|| 0.),
                log: Arc::new(move |event, data| observed.lock().unwrap().push((event.into(), data))),
                ..Options::default() }).unwrap();
        Self { engine, store, transport, logs }
    }
    fn seed_ack(&self) {
        let db = self.store.lock().unwrap();
        let delivery = db.delivery("group:10", false, NOW - 10.).unwrap();
        db.finish_delivery(&delivery, "sent_unconfirmed", Some(&json!(ACK))).unwrap();
        db.outbound_message(&json!({"chat":"group:10", "id":ACK, "sender":"99",
            "name":"Bot", "text":BODY, "ts":NOW-10., "received_at":NOW-10., "self":true}),
            &delivery).unwrap();
        drop(db);
        assert_eq!(self.fact()["status"], "sent_unconfirmed");
        assert_eq!(self.fact()["time_source"], "local_estimate");
    }
    fn fact(&self) -> Value {
        let rows = self.store.lock().unwrap().rows(
            "SELECT m.chat,m.id,m.sender,m.text,m.ts,m.received_at,m.message_key,d.status,d.message_id,f.time_source \
             FROM messages m JOIN outbound_message_facts f ON f.id=m.message_key \
             JOIN deliveries d ON d.id=f.delivery_id WHERE m.self=1", []).unwrap();
        assert_eq!(rows.len(), 1, "one acknowledged message fact");
        rows[0].clone()
    }
    fn evidence(&self) -> OutboundEvidence {
        OutboundEvidence { chat: group(), message_id: ACK.into(), sender_id: "99".into(),
            text: BODY.into(), server_ts: NOW+1., received_at: NOW+3.,
            source: OutboundEvidenceSource::MessageSent }
    }
    fn sends(&self) -> usize { self.transport.sends.load(Ordering::SeqCst) }
}
fn group() -> ChatAddress { ChatAddress::new(ChatKind::Group, "onebot", "10").unwrap() }
fn sent_frame() -> Value {
    json!({"post_type":"message_sent", "message_type":"group", "group_id":10,
        "self_id":99, "user_id":99, "sender":{"user_id":99,"nickname":"Bot"},
        "message_id":ACK, "time":NOW+1., "raw_message":BODY, "message":BODY})
}
fn adapter(answer: Value) -> (Arc<OneBotAdapter>, Arc<Mutex<Vec<(String, Value)>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let action = Arc::new(move |name: String, params: Value| -> BoxFuture<'static, PortReply<Value>> {
        recorded.lock().unwrap().push((name, params));
        let answer = answer.clone();
        Box::pin(async move { PortReply::Ok(answer) })
    });
    (Arc::new(OneBotAdapter::new("onebot", action,
        Arc::new(|| (true, true, Some("99".into())))).unwrap()), calls)
}

#[test]
fn message_sent_becomes_evidence_not_inbound() {
    let f = Fixture::new(false);
    let (port, _) = adapter(Value::Null);
    let event = port.map_event(&sent_frame(), NOW+3.).unwrap().expect("event must be retained");
    let PortEvent::OutboundEvidence(evidence) = event else {
        panic!("message_sent must produce only secondary evidence, never Incoming");
    };
    assert_eq!(evidence.source, OutboundEvidenceSource::MessageSent);
    assert_eq!(evidence.chat, group());
    assert_eq!(evidence.message_id, ACK);
    assert_eq!(evidence.sender_id, "99");
    assert_eq!(evidence.text, BODY);
    assert_eq!(evidence.server_ts, NOW+1.);
    assert_eq!(evidence.received_at, NOW+3.);
    assert!(f.store.lock().unwrap().rows("SELECT * FROM messages", []).unwrap().is_empty(),
        "mapping a self echo must not create an inbound message");
}

#[test]
fn matching_evidence_confirms_exactly_one_delivery() {
    let f = Fixture::new(false);
    f.seed_ack();
    let before = f.fact();
    assert!(f.engine.ingest_outbound_evidence(f.evidence()).unwrap());
    let after = f.fact();
    assert_eq!(after["message_key"], before["message_key"]);
    assert_eq!(after["status"], "confirmed");
    assert_eq!(after["time_source"], "server");
    assert_eq!(after["ts"], NOW+1.);
    assert_eq!(after["received_at"], NOW+3.);
    assert_eq!(f.store.lock().unwrap().rows("SELECT * FROM deliveries", []).unwrap().len(), 1);
    assert_eq!(f.sends(), 0, "confirmation must not send again");
}

#[test]
fn mismatched_evidence_confirms_nothing() {
    for label in ["chat", "message_id", "sender", "text", "no_ack"] {
        let f = Fixture::new(false);
        if label != "no_ack" { f.seed_ack(); }
        let before = (label != "no_ack").then(|| f.fact());
        let mut evidence = f.evidence();
        match label {
            "chat" => evidence.chat = ChatAddress::new(ChatKind::Group, "onebot", "11").unwrap(),
            "message_id" => evidence.message_id = "other-ack".into(),
            "sender" => evidence.sender_id = "100".into(),
            "text" => evidence.text = "different body".into(),
            "no_ack" => {},
            _ => unreachable!(),
        }
        assert!(!f.engine.ingest_outbound_evidence(evidence).unwrap(), "{label}");
        if let Some(before) = before { assert_eq!(f.fact(), before, "{label}: no fact or time rewrite"); }
        else {
            assert!(f.store.lock().unwrap().rows("SELECT * FROM deliveries", []).unwrap().is_empty());
        }
        let logs = f.logs.lock().unwrap();
        assert!(logs.iter().any(|(name, data)| name == "outbound_reconciliation_failed"
            && data["message_id"].is_string() && data["reason"].is_string()),
            "{label}: failed match must remain observable: {logs:?}");
        assert_eq!(f.sends(), 0);
    }
}

#[test]
fn duplicate_evidence_does_not_rewrite_history() {
    let f = Fixture::new(false);
    f.seed_ack();
    assert!(f.engine.ingest_outbound_evidence(f.evidence()).unwrap());
    let first = f.fact();
    let mut repeated = f.evidence();
    repeated.server_ts = NOW+100.;
    repeated.received_at = NOW+101.;
    let second = f.engine.ingest_outbound_evidence(repeated).unwrap();
    assert!(!second, "an already confirmed ACK must not confirm again");
    assert_eq!(f.fact(), first, "duplicate evidence cannot rewrite server or receive time");
    assert_eq!(f.sends(), 0);
}

#[tokio::test]
async fn typed_readback_requires_the_queried_target() {
    let mut readback = sent_frame();
    readback.as_object_mut().unwrap().remove("post_type");
    for label in ["matched", "wrong_chat", "wrong_id"] {
        let f = Fixture::new(false);
        f.seed_ack();
        let before = f.fact();
        let (port, calls) = adapter(readback.clone());
        let reply = port.deliver(Command::Fetch { port: "onebot".into(),
            request: Fetch::OutboundEvidence { port: "onebot".into(), message_id: ACK.into() }
        }).await;
        let PortReply::Ok(CommandResult::OutboundEvidence(evidence)) = reply else {
            panic!("get_msg must return typed outbound evidence, not raw JSON: {reply:?}");
        };
        assert_eq!(evidence.source, OutboundEvidenceSource::Readback);
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(calls.lock().unwrap()[0].0, "get_msg");
        assert_eq!(calls.lock().unwrap()[0].1["message_id"], ACK);
        let expected_chat = if label == "wrong_chat" {
            ChatAddress::new(ChatKind::Group, "onebot", "11").unwrap()
        } else { group() };
        let expected_id = if label == "wrong_id" { "other-ack" } else { ACK };
        let confirmed = f.engine.reconcile_port_readback(&expected_chat, expected_id, evidence).unwrap();
        assert_eq!(confirmed, label == "matched", "{label}");
        if label == "matched" {
            assert_eq!(f.fact()["status"], "confirmed");
            assert_eq!(f.fact()["time_source"], "server");
        } else { assert_eq!(f.fact(), before, "{label}: wrong query target cannot confirm"); }
        assert_eq!(f.sends(), 0);
    }
}

#[tokio::test]
async fn forged_or_incomplete_evidence_cannot_confirm() {
    let f = Fixture::new(true);
    f.seed_ack();
    let before = f.fact();
    let (port, _) = adapter(Value::Null);

    let mut ordinary = sent_frame();
    ordinary["post_type"] = json!("message");
    let mapped = port.map_event(&ordinary, NOW+3.);
    assert!(!matches!(mapped, Ok(Some(PortEvent::OutboundEvidence(_)))),
        "ordinary self message is not independent message_sent evidence");

    let mut wrong_sender = sent_frame();
    wrong_sender["sender"]["user_id"] = json!(100);
    wrong_sender["user_id"] = json!(100);
    assert!(!matches!(port.map_event(&wrong_sender, NOW+3.), Ok(Some(PortEvent::OutboundEvidence(_)))));

    let mut no_time = sent_frame();
    no_time.as_object_mut().unwrap().remove("time");
    assert!(!matches!(port.map_event(&no_time, NOW+3.), Ok(Some(PortEvent::OutboundEvidence(_)))));

    let mut forged = f.evidence();
    forged.sender_id = "100".into();
    assert!(!f.engine.ingest_outbound_evidence(forged).unwrap());
    assert_eq!(f.fact(), before);
    assert!(f.logs.lock().unwrap().iter().any(|(name, data)|
        name == "outbound_reconciliation_failed" && data["chat"] == "group:10"
        && data["message_id"] == ACK && data["reason"].as_str().is_some_and(|s| !s.is_empty())));

    // The ACK is old enough for readback. Let the real timeout path expire;
    // no second send is allowed while evidence remains absent.
    f.engine.tick().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), f.engine.wait_idle()).await.unwrap();
    assert_eq!(f.fact(), before);
    assert!(f.logs.lock().unwrap().iter().any(|(name, data)|
        name == "outbound_reconciliation_timeout" && data["chat"] == "group:10"
        && data["message_id"] == ACK && data["reason"] == "get_msg_timeout"));
    assert_eq!(f.sends(), 0, "invalid or missing evidence must never cause a retry send");
}
