//! Contract tests for the observable ACK -> independent evidence -> confirmed lifecycle.
//! Fixtures use the public Engine entry and persisted rows; no reconciliation helper is called.

use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        orientation::{OrientationProvider, OrientationTransport},
        Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

const CHAT: &str = "group:10";
const TEXT: &str = "closure ping";
const ACK_ID: i64 = 88001;
const LOCAL_TIME: f64 = 5. * 86400. + 43200.;
const SERVER_TIME: f64 = LOCAL_TIME + 5.;

#[derive(Default)]
struct Wire {
    sends: AtomicUsize,
    calls: Mutex<Vec<String>>,
    get_msg_reply: Mutex<Option<Value>>,
}
impl OrientationTransport for Wire {
    fn self_id(&self) -> String {
        "99".into()
    }
    fn call<'a>(&'a self, action: &'a str, _params: Value) -> BoxFuture<'a, Result<Value>> {
        self.calls.lock().unwrap().push(action.into());
        let reply = (action == "get_msg")
            .then(|| self.get_msg_reply.lock().unwrap().clone())
            .flatten();
        Box::pin(async move { reply.ok_or_else(|| anyhow::anyhow!("message_not_found")) })
    }
}
impl EngineTransport for Wire {
    fn state(&self) -> State {
        State {
            connected: true,
            online: true,
            self_id: "99".into(),
            ..State::default()
        }
    }
    fn send<'a>(
        &'a self,
        _chat: &'a str,
        text: &'a str,
        _face: Option<&'a str>,
        _reply_to: Option<&'a str>,
        _mention: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        assert_eq!(text, TEXT);
        self.sends.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!({"message_id": ACK_ID})) })
    }
}

struct Model;
impl OrientationProvider for Model {
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if system == qq_inner_core::persona::NAME_PROMPT {
                return Ok(json!("Bot"));
            }
            if system.contains("TASK: FORM") {
                return Ok(
                    json!({"allocation":"self","candidates":[{"text":"answer the question","kind":"system1"}]}),
                );
            }
            if system.contains("TASK: EVALUATE") {
                let ratings: Vec<_> = payload["candidates"].as_array().unwrap().iter()
                    .map(|candidate| json!({"id":candidate["id"],"motivation":5,"relevance":5,"originality":5}))
                    .collect();
                return Ok(json!({"ratings":ratings}));
            }
            if system.contains("TASK: ARTICULATE") {
                return Ok(json!({"text":TEXT}));
            }
            anyhow::bail!("unexpected_model_stage")
        })
    }
}

struct Fixture {
    engine: Arc<Engine>,
    store: Arc<Mutex<Store>>,
    wire: Arc<Wire>,
    now: Arc<Mutex<f64>>,
    logs: Arc<Mutex<Vec<(String, Value)>>>,
}
impl Fixture {
    fn new() -> Self {
        let config = Config::from_value(&merge(
            &defaults(),
            &json!({
                "apiKey":"test-only", "onebotToken":"", "dataDir":"/tmp/outbound-closure-contract",
                "provider":{"model":"stub"},
                "agent":{
                "allowedGroups":["10"], "observation":{"enabled":false},
                "sending":{"enabled":false}, "proactive":false,
                "emoji":{"enabled":false,"learnFrequency":false,"faceOnly":false}
            }}),
        ))
        .unwrap();
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let wire = Arc::new(Wire::default());
        let now = Arc::new(Mutex::new(LOCAL_TIME));
        let logs = Arc::new(Mutex::new(Vec::new()));
        let options = Options {
            now: {
                let now = now.clone();
                Arc::new(move || *now.lock().unwrap())
            },
            random: Arc::new(|| 0.),
            selection_random: Arc::new(|| 0.),
            expression_random: Arc::new(|| 0.5),
            log: {
                let logs = logs.clone();
                Arc::new(move |event, data| logs.lock().unwrap().push((event.into(), data)))
            },
            ..Options::default()
        };
        let engine = Engine::new(
            config,
            store.clone(),
            Arc::new(Model),
            wire.clone(),
            options,
        )
        .unwrap();
        Self {
            engine,
            store,
            wire,
            now,
            logs,
        }
    }
    async fn send_and_ack(&self) {
        self.engine
            .ingest(&json!({
                "post_type":"message", "message_type":"group", "self_id":99,
                "group_id":10, "user_id":20, "message_id":"human-1", "time":LOCAL_TIME - 1.,
                "sender":{"nickname":"Human"}, "message":"[CQ:at,qq=99] ping"
            }))
            .unwrap();
        *self.now.lock().unwrap() += 4.;
        self.engine.tick().unwrap();
        self.engine.wait_idle().await;
        assert_eq!(
            self.engine.last_error(),
            None,
            "test fixture did not complete sending"
        );
        assert_eq!(
            self.wire.sends.load(Ordering::SeqCst),
            1,
            "test fixture did not send exactly once: chats={:?}, logs={:?}",
            self.engine.chats(),
            self.logs.lock().unwrap()
        );
        assert_eq!(self.row()["status"], "sent_unconfirmed");
        assert_eq!(self.row()["time_source"], "local_estimate");
    }
    fn row(&self) -> Value {
        let rows = self
            .store
            .lock()
            .unwrap()
            .rows(
                "SELECT d.status,d.message_id,f.time_source,m.ts,m.text,m.message_key \
             FROM deliveries d JOIN outbound_message_facts f ON f.delivery_id=d.id \
             JOIN messages m ON m.message_key=f.id WHERE d.chat=?",
                [CHAT],
            )
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "one ACK must create one queryable outbound fact"
        );
        rows[0].clone()
    }
    fn echo(&self, id: i64, group: i64, text: &str, time: f64) {
        self.engine
            .ingest(&json!({
                "post_type":"message_sent", "message_sent_type":"self", "self_id":99,
                "user_id":99, "group_id":group, "message_id":id, "time":time,
                "sender":{"user_id":99,"nickname":"Bot"}, "raw_message":text,
                "message":text
            }))
            .unwrap();
    }
}

#[tokio::test]
async fn matching_message_sent_upgrades_ack_and_exposes_server_time() {
    let f = Fixture::new();
    f.send_and_ack().await;
    let estimated = f.row()["ts"].as_f64().unwrap();
    f.echo(ACK_ID, 10, TEXT, SERVER_TIME);
    let row = f.row();
    assert_eq!(row["status"], "confirmed");
    assert_eq!(row["time_source"], "server");
    assert_eq!(row["ts"], SERVER_TIME);
    assert_ne!(
        row["ts"], estimated,
        "server time must replace the local estimate"
    );
    assert_eq!(f.wire.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wrong_id_group_or_content_cannot_confirm_an_ack() {
    for (id, group, body) in [
        (88002, 10, TEXT),
        (ACK_ID, 11, TEXT),
        (ACK_ID, 10, "a different message"),
    ] {
        let f = Fixture::new();
        f.send_and_ack().await;
        let before = f.row();
        f.echo(id, group, body, SERVER_TIME);
        assert_eq!(
            f.row(),
            before,
            "mismatched evidence ({id}, {group}, {body:?}) changed the outbound fact"
        );
        f.echo(ACK_ID, 10, TEXT, SERVER_TIME);
        assert_eq!(
            f.row()["status"],
            "confirmed",
            "a later exact match must still work"
        );
    }
}

#[tokio::test]
async fn duplicate_message_sent_is_one_confirmation_and_one_outbound_fact() {
    let f = Fixture::new();
    f.send_and_ack().await;
    f.echo(ACK_ID, 10, TEXT, SERVER_TIME);
    let once = f.row();
    assert_eq!(once["status"], "confirmed");
    f.echo(ACK_ID, 10, TEXT, SERVER_TIME + 60.);
    assert_eq!(
        f.row(),
        once,
        "replayed echo must not rewrite confirmed evidence or time"
    );
    let db = f.store.lock().unwrap();
    assert_eq!(
        db.rows("SELECT id FROM outbound_message_facts", [])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.rows("SELECT message_key FROM messages WHERE self=1", [])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(f.wire.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recent_matching_get_msg_can_confirm_when_echo_is_missing() {
    let f = Fixture::new();
    *f.wire.get_msg_reply.lock().unwrap() = Some(json!({
        "message_type":"group", "group_id":10, "message_id":ACK_ID, "time":SERVER_TIME,
        "sender":{"user_id":99},
        "raw_message":TEXT, "message":TEXT
    }));
    f.send_and_ack().await;
    *f.now.lock().unwrap() += 30.;
    f.engine.tick().unwrap();
    f.engine.wait_idle().await;
    assert!(
        f.wire
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "get_msg"),
        "missing echo must use the recent readback path"
    );
    let row = f.row();
    assert_eq!(row["status"], "confirmed");
    assert_eq!(row["time_source"], "server");
    assert_eq!(row["ts"], SERVER_TIME);
    assert_eq!(f.wire.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn get_msg_with_different_content_is_not_independent_confirmation() {
    let f = Fixture::new();
    *f.wire.get_msg_reply.lock().unwrap() = Some(json!({
        "message_type":"group", "sender":{"user_id":99},
        "group_id":10, "message_id":ACK_ID, "time":SERVER_TIME,
        "raw_message":"another outbound message", "message":"another outbound message"
    }));
    f.send_and_ack().await;
    let before = f.row();
    *f.now.lock().unwrap() += 30.;
    f.engine.tick().unwrap();
    f.engine.wait_idle().await;
    assert!(f
        .wire
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call == "get_msg"));
    assert_eq!(
        f.row(),
        before,
        "readback with the same ID but different content is not proof"
    );
    assert_eq!(f.wire.sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn missing_echo_and_failed_get_msg_leave_an_observable_unconfirmed_state() {
    let f = Fixture::new();
    f.send_and_ack().await;
    let before = f.row();
    *f.now.lock().unwrap() += 30.;
    f.engine.tick().unwrap();
    f.engine.wait_idle().await;
    let after = f.row();
    assert_eq!(
        after, before,
        "missing secondary evidence must never fabricate confirmation or server time"
    );
    assert_eq!(
        f.wire.sends.load(Ordering::SeqCst),
        1,
        "reconciliation must never resend"
    );
    assert!(
        f.wire
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "get_msg"),
        "after echo timeout, the recent get_msg fallback was never attempted"
    );
    assert!(
        f.logs.lock().unwrap().iter().any(|(event, data)| {
            let event = event.to_ascii_lowercase();
            let detail = data.to_string().to_ascii_lowercase();
            (event.contains("reconcil") || event.contains("outbound"))
                && (event.contains("failed")
                    || event.contains("timeout")
                    || detail.contains("not_found")
                    || detail.contains("timeout")
                    || detail.contains("failed")
                    || detail.contains("unavailable"))
        }),
        "failed or timed-out reconciliation must leave a queryable diagnostic"
    );
}
