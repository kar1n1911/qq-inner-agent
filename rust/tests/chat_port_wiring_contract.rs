//! Contract for the engine seams used when a ChatPort owns the OneBot connection.
use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        io::{
            Capabilities, ChatAddress, ChatKind, ChatPort, Command, CommandResult, Incoming,
            IoError, Notice, NoticeKind, OutboundEvidence, OutboundEvidenceSource, PortEvent,
            PortKind, PortManifest, PortReply, PortState, ReplyObservation, Segment, Sender,
            TransportKind, VersionReq,
        },
        orientation::{OrientationProvider, OrientationTransport},
        Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const CHAT: &str = "group:10";
const NOW: f64 = 43_200.0;

struct Transport {
    media: Mutex<Vec<(String, Value)>>,
    text: Mutex<usize>,
}
impl OrientationTransport for Transport {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected_orientation_call") })
    }
}
impl EngineTransport for Transport {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "99".into(), reconnects: 0, degraded: false }
    }
    fn send_media<'a>(&'a self, chat: &'a str, segment: Value)
        -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            self.media.lock().unwrap().push((chat.into(), segment));
            Ok(json!({"message_id":"built-in:1"}))
        })
    }
    fn send<'a>(&'a self, _: &'a str, _: &'a str, _: Option<&'a str>,
        _: Option<&'a str>, _: Option<&'a str>)
        -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            *self.text.lock().unwrap() += 1;
            Ok(json!({"message_id":"built-in:1"}))
        })
    }
}
impl OrientationProvider for Transport {
    fn json<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected_model_call") })
    }
}

struct Port {
    manifest: PortManifest,
    commands: Mutex<Vec<Command>>,
    fail_uncertain: bool,
}
impl Port {
    fn new(fail_uncertain: bool) -> Arc<Self> {
        Arc::new(Self {
            manifest: PortManifest {
                name: "onebot".into(), kind: PortKind::Chat, transport: TransportKind::InProcess,
                core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
                host_permissions: vec![], dependencies: vec![], config_fields: vec![],
            },
            commands: Mutex::new(vec![]), fail_uncertain,
        })
    }
}
impl ChatPort for Port {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> Capabilities {
        Capabilities { media: true, notices: true, ..Default::default() }
    }
    fn state(&self) -> PortState {
        PortState { connected: true, online: true, self_id: Some("99".into()), degraded: None }
    }
    fn deliver<'a>(&'a self, command: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        Box::pin(async move {
            self.commands.lock().unwrap().push(command);
            if self.fail_uncertain {
                PortReply::Failed { code: "action_timeout".into(), uncertain: true }
            } else {
                PortReply::Ok(CommandResult::Sent { message_id: "port:1".into() })
            }
        })
    }
    fn attach(&self, _: mpsc::Sender<PortEvent>) -> std::result::Result<(), IoError> { Ok(()) }
}

struct Fixture {
    engine: Arc<Engine>,
    store: Arc<Mutex<Store>>,
    transport: Arc<Transport>,
    events: Arc<Mutex<Vec<(String, Value)>>>,
}
fn fixture() -> Fixture {
    let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
    let transport = Arc::new(Transport { media: Mutex::new(vec![]), text: Mutex::new(0) });
    let events = Arc::new(Mutex::new(vec![]));
    let logged = events.clone();
    let options = Options {
        now: Arc::new(|| NOW),
        log: Arc::new(move |name, data| logged.lock().unwrap().push((name.into(), data))),
        ..Default::default()
    };
    let config = Config::from_value(&merge(&defaults(), &json!({
        "apiKey":"mock", "onebotToken":"", "dataDir":"unused",
        "provider":{"model":"mock"},
        "agent":{"allowedGroups":["10"],"allowedUsers":["20"],
            "observation":{"enabled":true},"schedule":{"enabled":false},
            "rhythm":{"enabled":false}}
    }))).unwrap();
    let engine = Engine::new(config, store.clone(), transport.clone(), transport.clone(), options).unwrap();
    Fixture { engine, store, transport, events }
}
fn address() -> ChatAddress { ChatAddress::new(ChatKind::Group, "onebot", "10").unwrap() }
fn image() -> Value { json!({"type":"image","data":{"file":"https://example.test/p.png"}}) }
fn face() -> Value { json!({"type":"face","data":{"id":"14"}}) }
fn delivery_rows(f: &Fixture) -> Vec<Value> {
    f.store.lock().unwrap().rows("SELECT chat,status FROM deliveries ORDER BY rowid", []).unwrap()
}

#[tokio::test]
async fn port_incoming_notice_and_evidence_reach_their_entries() {
    let f = fixture();
    let port = Port::new(false);
    let incoming = Incoming {
        port: "onebot".into(), chat: address(), message_id: "human:1".into(),
        self_id: "99".into(), ts: NOW - 2.0, received_at: NOW,
        sender: Sender { id: "20".into(), name: "Human".into(), card: None,
            role: None, title: None, is_robot: None },
        segments: vec![Segment::Text("你好".into())], reply: ReplyObservation::NoReply,
        self_message: false, raw: None,
    };
    f.engine.ingest_port_event(PortEvent::Incoming(Box::new(incoming)), port.clone()).await.unwrap();
    let messages = f.store.lock().unwrap().rows("SELECT id,self FROM messages", []).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"], "human:1");
    assert_eq!(messages[0]["self"], 0);
    let before = f.store.lock().unwrap().orientation_state(CHAT).unwrap().map(|s| s.epoch);

    f.engine.ingest_port_event(PortEvent::Notice(Notice {
        kind: NoticeKind::GroupJoined, chat: Some(address()), actor: Some("99".into()),
        self_id: Some("99".into()), at: NOW,
    }), port.clone()).await.unwrap();
    let after = f.store.lock().unwrap().orientation_state(CHAT).unwrap().map(|s| s.epoch);
    assert!(after.is_some() && after != before, "notice must advance the join epoch");
    assert_eq!(f.store.lock().unwrap().rows("SELECT id FROM messages", []).unwrap().len(), 1);

    {
        let db = f.store.lock().unwrap();
        let id = db.delivery(CHAT, false, NOW).unwrap();
        db.finish_delivery(&id, "sent_unconfirmed", Some(&json!("ack:1"))).unwrap();
        db.outbound_message(&json!({"chat":CHAT,"id":"ack:1","sender":"99",
            "name":"Bot","text":"sent","ts":NOW,"received_at":NOW,"self":true}), &id).unwrap();
    }
    f.engine.ingest_port_event(PortEvent::OutboundEvidence(OutboundEvidence {
        chat: address(), message_id: "ack:1".into(), sender_id: "99".into(),
        text: "sent".into(), server_ts: NOW + 1.0, received_at: NOW + 2.0,
        source: OutboundEvidenceSource::MessageSent,
    }), port).await.unwrap();
    let deliveries = delivery_rows(&f);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["status"], "confirmed");
    assert_eq!(f.store.lock().unwrap().rows("SELECT id FROM messages WHERE self=0", []).unwrap().len(), 1);
}

#[tokio::test]
async fn media_send_routes_by_port_binding() {
    let no_port = fixture();
    no_port.engine.send_media_to(CHAT, image()).await.unwrap();
    assert_eq!(no_port.transport.media.lock().unwrap().len(), 1);

    let bound_image = fixture();
    let image_port = Port::new(false);
    bound_image.engine.set_chat_port(image_port.clone());
    bound_image.engine.bind_configured_chats().unwrap();
    bound_image.engine.send_media_to(CHAT, image()).await.unwrap();
    assert_eq!(bound_image.transport.media.lock().unwrap().len(), 0);
    let commands = image_port.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::SendMedia { chat, .. } if chat == &address()));
    drop(commands);

    let bound_face = fixture();
    let face_port = Port::new(false);
    bound_face.engine.set_chat_port(face_port.clone());
    bound_face.engine.bind_configured_chats().unwrap();
    bound_face.engine.send_media_to(CHAT, face()).await.unwrap();
    assert_eq!(bound_face.transport.media.lock().unwrap().len(), 0);
    assert_eq!(*bound_face.transport.text.lock().unwrap(), 0);
    let commands = face_port.commands.lock().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::Send { chat, segments, .. }
        if chat == &address() && matches!(segments.as_slice(), [Segment::Face { id }] if id == "14")));
    drop(commands);

    let unbound = fixture();
    let unbound_port = Port::new(false);
    unbound.engine.set_chat_port(unbound_port.clone());
    assert!(unbound.engine.chat_port_for(CHAT).is_none(), "no inbound or configured bind occurred");
    let error = unbound.engine.send_media_to(CHAT, image()).await.unwrap_err();
    assert_eq!(error.code, "chat_port_unbound");
    assert!(!error.uncertain);
    assert!(unbound_port.commands.lock().unwrap().is_empty());
    assert!(unbound.transport.media.lock().unwrap().is_empty());
    assert!(unbound.events.lock().unwrap().iter().any(|(name, data)|
        name == "chat_route_unbound" && data["chat"] == CHAT
        && data["reason"] == "chat_port_unbound" && data["uncertain"] == false));
    assert_eq!(unbound.engine.last_error().as_deref(), Some("chat_port_unbound"));
    assert!(delivery_rows(&unbound).iter().any(|row| row["chat"] == CHAT && row["status"] == "failed"));
}

#[tokio::test]
async fn media_failure_keeps_uncertain_and_never_resends() {
    let f = fixture();
    let port = Port::new(true);
    f.engine.set_chat_port(port.clone());
    f.engine.bind_configured_chats().unwrap();
    let error = f.engine.send_media_to(CHAT, image()).await.unwrap_err();
    assert_eq!(error.code, "action_timeout");
    assert!(error.uncertain);
    assert_eq!(port.commands.lock().unwrap().len(), 1);
    assert!(f.transport.media.lock().unwrap().is_empty());
    assert_eq!(*f.transport.text.lock().unwrap(), 0);
    assert!(delivery_rows(&f).iter().any(|row| row["chat"] == CHAT && row["status"] == "uncertain"));
    assert!(f.engine.last_error().is_some());
}

#[tokio::test]
async fn unsupported_media_segment_fails_explicitly() {
    for with_port in [false, true] {
        let f = fixture();
        let port = Port::new(false);
        if with_port {
            f.engine.set_chat_port(port.clone());
            f.engine.bind_configured_chats().unwrap();
        }
        let error = f.engine.send_media_to(CHAT,
            json!({"type":"video","data":{"file":"https://example.test/v.mp4"}})).await.unwrap_err();
        assert!(!error.uncertain);
        assert!(!error.code.is_empty());
        assert!(port.commands.lock().unwrap().is_empty());
        assert!(f.transport.media.lock().unwrap().is_empty());
    }
}
