use anyhow::Result;
use futures_util::{future::BoxFuture, SinkExt, StreamExt};
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        io::{adapters::onebot::connect::ConnectConfig, factory::{self, HostServices}, plugin_list, Capabilities, ChatAddress, ChatKind, ChatPort, Command, CommandResult,
            Incoming, IoError, PortKind, PortManifest, PortEvent, PortReply, PortState, Segment, Sender,
            TransportKind, VersionReq},
        orientation::{OrientationProvider, OrientationTransport},
        Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, sync::{atomic::{AtomicUsize, Ordering}, Arc, Mutex}};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle, time::{sleep, timeout, Duration}};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{handshake::server::{Request, Response}, Message},
};

const NOW: f64 = 1_700_000_000.;
const CHAT: &str = "group:42";
const REPLY: &str = "收到，我们继续。";
const TOKEN: &str = "a+b &?";

struct MockOneBot {
    url: String,
    sends: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}
impl Drop for MockOneBot {
    fn drop(&mut self) { self.task.abort(); }
}

#[allow(clippy::result_large_err)]
async fn mock_onebot() -> MockOneBot {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let sends = Arc::new(AtomicUsize::new(0));
    let counted = sends.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = accept_hdr_async(stream, |request: &Request, response: Response| {
                if request.uri().query() == Some("access_token=a%2Bb+%26%3F") {
                    Ok(response)
                } else {
                    Err(tokio_tungstenite::tungstenite::http::Response::builder()
                        .status(401).body(Some("unauthorized".into())).unwrap())
                }
            }).await;
            let Ok(mut socket) = socket else { continue; };
            let mut first_status = true;
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let data = match request["action"].as_str() {
                    Some("get_login_info") => Some(json!({"user_id":999})),
                    Some("get_status") if first_status => {
                        first_status = false;
                        Some(json!({"online":true}))
                    }
                    Some("send_group_msg") => {
                        counted.fetch_add(1, Ordering::SeqCst);
                        Some(json!({"message_id":"port-1"}))
                    }
                    _ => None,
                };
                if let Some(data) = data {
                    let reply = json!({"echo":request["echo"],"status":"ok","retcode":0,"data":data});
                    if socket.send(Message::Text(reply.to_string())).await.is_err() { break; }
                }
            }
        }
    });
    MockOneBot { url, sends, task }
}

fn onebot_connect(mock: &MockOneBot) -> (ConnectConfig, String) {
    (ConnectConfig {
        url: mock.url.clone(), self_id: "999".into(), self_id_is_truthy: true,
        heartbeat_seconds: 10.0, request_timeout_seconds: 0.15,
        reconnect_max_seconds: 1.0, max_lost_seconds: 30.0,
        degraded_probe_seconds: 1.0, forward_enabled: false,
    }, TOKEN.into())
}

async fn wait_port_online(port: &dyn ChatPort) {
    timeout(Duration::from_secs(15), async {
        loop {
            let state = port.state();
            if state.connected && state.online { break; }
            sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("fixture OneBot port must connect");
}

struct Model;
impl OrientationProvider for Model {
    fn json<'a>(&'a self, _system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if payload.get("lengthTarget").is_some() {
                return Ok(json!({"text":REPLY}));
            }
            if payload.get("candidates").is_some() {
                return Ok(json!({"ratings":[{"id":payload["candidates"][0]["id"],
                    "motivation":5.,"relevance":5.,"originality":5.}]}));
            }
            Ok(json!({"allocation":"self","candidates":[{"kind":"system2","text":"接话"}]}))
        })
    }
}

#[derive(Default)]
struct BuiltIn { sent: Mutex<Vec<(String, String)>> }
impl OrientationTransport for BuiltIn {
    fn self_id(&self) -> String { "999".into() }
    fn call<'a>(&'a self, _action: &'a str, _params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { Ok(json!({})) })
    }
}
impl EngineTransport for BuiltIn {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "999".into(), ..State::default() }
    }
    fn send<'a>(&'a self, chat: &'a str, text: &'a str, _face: Option<&'a str>,
        _reply_to: Option<&'a str>, _mention: Option<&'a str>)
        -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            let mut sent = self.sent.lock().unwrap();
            sent.push((chat.into(), text.into()));
            Ok(json!({"message_id":format!("built-in-{}",sent.len())}))
        })
    }
}

struct PublishedChat {
    manifest: PortManifest,
    commands: Mutex<Vec<Command>>,
    result: PortReply<CommandResult>,
}
impl PublishedChat {
    fn new(result: PortReply<CommandResult>) -> Self {
        Self {
            manifest: PortManifest { name: "fixture-chat".into(), kind: PortKind::Chat,
                transport: TransportKind::InProcess,
                core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
                host_permissions: vec![], dependencies: vec![], config_fields: vec![] },
            commands: Mutex::new(vec![]), result,
        }
    }
}
impl ChatPort for PublishedChat {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> Capabilities { Capabilities::default() }
    fn state(&self) -> PortState { PortState::available() }
    fn deliver<'a>(&'a self, command: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        Box::pin(async move {
            self.commands.lock().unwrap().push(command);
            self.result.clone()
        })
    }
    fn attach(&self, _sink: mpsc::Sender<PortEvent>) -> std::result::Result<(), IoError> {
        Ok(())
    }
}

struct Harness {
    engine: Arc<Engine>,
    built_in: Arc<BuiltIn>,
    store: Arc<Mutex<Store>>,
    events: Arc<Mutex<Vec<(String, Value)>>>,
}
impl Harness {
    fn new() -> Self {
        let raw = merge(&defaults(), &json!({
            "apiKey":"fixture", "onebotToken":"fixture", "dataDir":"unused",
            "provider":{"model":"fixture-model"},
            "agent":{"allowedGroups":["42"],"quietHours":null,"debounceSeconds":0,
                "minThinkIntervalSeconds":1,"threshold":1,
                "learning":{"enabled":false},"observation":{"enabled":false},
                "emoji":{"enabled":false,"faceOnly":false,"learnFrequency":false},
                "sending":{"enabled":false}}
        }));
        let config = Config::from_value(&raw).unwrap();
        let built_in = Arc::new(BuiltIn::default());
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let options = Options { now: Arc::new(|| NOW), random: Arc::new(|| 0.),
            selection_random: Arc::new(|| 0.), expression_random: Arc::new(|| 0.7),
            activity_random: Arc::new(|| 0.),
            log: Arc::new(move |event, value| captured.lock().unwrap().push((event.into(), value))),
            ..Options::default() };
        let engine = Engine::new(config, store.clone(), Arc::new(Model), built_in.clone(), options).unwrap();
        Self { engine, built_in, store, events }
    }
    fn ingest_plugin(&self, port: Arc<dyn ChatPort>) {
        let name = port.manifest().name.clone();
        self.engine.ingest_incoming(&Incoming {
            port: name.clone(),
            chat: ChatAddress::new(ChatKind::Group, name, "42").unwrap(),
            message_id: "in-1".into(), self_id: "999".into(), ts: NOW, received_at: NOW,
            sender: Sender { id: "7".into(), name: "群友".into(), card: None,
                role: None, title: None, is_robot: None },
            segments: vec![Segment::Mention { id: "999".into() }, Segment::Text("哈哈".into())],
            self_message: false, raw: None,
        }, port).unwrap();
    }
    fn ingest_legacy(&self) {
        self.engine.ingest(&json!({"post_type":"message","message_type":"group",
            "group_id":42,"user_id":7,"self_id":999,"message_id":"in-1",
            "time":NOW,"sender":{"nickname":"群友"},
            "message":"[CQ:at,qq=999]哈哈"})).unwrap();
    }
    async fn finish(&self) {
        self.engine.tick().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), self.engine.wait_idle())
            .await.expect("send turn must finish");
    }
}

fn accepted() -> PortReply<CommandResult> {
    PortReply::Ok(CommandResult::Sent { message_id: "port-1".into() })
}

struct PluginFixture { root: PathBuf, list: Value }
impl Drop for PluginFixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
}
fn onebot_package(enabled: bool) -> PluginFixture {
    let root = std::env::temp_dir().join(format!("plugin-enablement-{}", rand::random::<u64>()));
    let package = root.join("onebot");
    fs::create_dir_all(&package).unwrap();
    let manifest = json!({"name":"onebot","version":"1.0.0","kind":"Chat",
        "transport":"InProcess","core_api_version":{"major":1,"min_minor":0,"max_minor":null},
        "capabilities":{"supported":[],"unsupported":[],"unknown":[]},
        "dependencies":[],"host_permissions":[],"config_schema":[],"hot_reload":true});
    fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
    let list = json!({"schema_version":1,"mode":"manual","plugins":[
        {"id":"fixture-chat","directory":"onebot","enabled":enabled,
            "params":{},"bindings":{},"secret_refs":{}}]});
    fs::write(root.join("plugins.json"), list.to_string()).unwrap();
    PluginFixture { root, list }
}

#[tokio::test]
async fn enabled_list_preflights_publishes_and_its_chat_port_ingests() {
    let fixture = onebot_package(true);
    let mock = mock_onebot().await;
    let preflight = plugin_list::preflight(&fixture.root, &fixture.list);
    assert_eq!(preflight["reason"], Value::Null, "enabled fixture must pass preflight: {preflight}");
    let mut host = HostServices::without_runtime(Arc::new(|_| None));
    host.onebot_connect = Some(onebot_connect(&mock));
    let outcome = factory::prepare(&fixture.root, &fixture.list, &host);
    let published = outcome.published.unwrap_or_else(|| panic!("enabled port must publish: {}", outcome.status));
    let port = published.onebot.expect("onebot ChatPort must be published");
    wait_port_online(port.as_ref()).await;
    let h = Harness::new();
    h.ingest_plugin(port);
    let inbound = h.store.lock().unwrap().rows("SELECT chat,text FROM messages WHERE self=0", []).unwrap();
    assert_eq!(inbound.len(), 1, "factory-published ingress must reach the engine");
    h.finish().await;
    assert_eq!(mock.sends.load(Ordering::SeqCst), 1, "factory-published endpoint must deliver the reply");
    assert!(h.built_in.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn published_enabled_port_ingests_and_owns_the_group_reply_without_double_send() {
    let h = Harness::new();
    let port = Arc::new(PublishedChat::new(accepted()));
    h.ingest_plugin(port.clone());
    assert_eq!(h.store.lock().unwrap().rows("SELECT chat,text FROM messages WHERE self=0", []).unwrap().len(), 1,
        "plugin ingress must reach persistent engine history");
    h.finish().await;
    let commands = port.commands.lock().unwrap().clone();
    assert_eq!(commands.len(), 1, "published port must receive exactly one outbound command");
    assert!(matches!(&commands[0], Command::Send { chat, segments, .. }
        if chat.id == "42" && segments.iter().any(|s| s == &Segment::Text(REPLY.into()))));
    assert!(h.built_in.sent.lock().unwrap().is_empty(), "built-in transport must not double-send");
}

#[tokio::test]
async fn disabled_plugin_preserves_the_builtin_send_path() {
    let fixture = onebot_package(false);
    let outcome = factory::prepare(&fixture.root, &fixture.list,
        &HostServices::without_runtime(Arc::new(|_| None)));
    assert_eq!(outcome.status["plugins"]["fixture-chat"]["reason"], "disabled_by_user");
    assert!(outcome.published.and_then(|ports| ports.onebot).is_none(),
        "disabled package must not publish a ChatPort");
    let h = Harness::new();
    h.ingest_legacy();
    h.finish().await;
    assert_eq!(*h.built_in.sent.lock().unwrap(), [(CHAT.into(), REPLY.into())]);
}

#[tokio::test]
async fn unbound_group_fallback_is_visible_and_sends_once() {
    let fixture = onebot_package(true);
    let mock = mock_onebot().await;
    let mut host = HostServices::without_runtime(Arc::new(|_| None));
    host.onebot_connect = Some(onebot_connect(&mock));
    let published = factory::prepare(&fixture.root, &fixture.list, &host).published
        .expect("enabled plugin must publish before testing unbound fallback");
    assert!(published.onebot.is_some());
    wait_port_online(published.onebot.as_ref().unwrap().as_ref()).await;
    let h = Harness::new();
    h.ingest_legacy(); // This group has never received inbound through the published port.
    h.finish().await;
    assert_eq!(h.built_in.sent.lock().unwrap().len(), 1);
    assert_eq!(mock.sends.load(Ordering::SeqCst), 0);
    assert!(h.events.lock().unwrap().iter().any(|(_, details)|
        details["chat"] == CHAT && details["route"] == "built_in_fallback"
            && details["reason"] == "chat_port_unbound"),
        "unbound fallback needs a structured, group-specific observation");
}

#[tokio::test]
async fn degraded_or_failed_port_never_silently_sends_through_builtin() {
    for outcome in [
        PortReply::Degraded { result: CommandResult::Sent { message_id: "uncertain".into() },
            missing: vec![] },
        PortReply::Failed { code: "plugin_unavailable".into(), uncertain: false },
    ] {
        let h = Harness::new();
        let port = Arc::new(PublishedChat::new(outcome));
        h.ingest_plugin(port.clone());
        h.finish().await;
        assert_eq!(port.commands.lock().unwrap().len(), 1);
        assert!(h.built_in.sent.lock().unwrap().is_empty(),
            "a failed plugin delivery must never become a second built-in send");
        let error = h.engine.last_error().expect("failed port must leave a queryable error");
        assert!(h.events.lock().unwrap().iter().any(|(event, details)|
            event == "delivery_error" && details["chat"] == CHAT
                && details["code"] == error && details["attempted"] == 1),
            "the visible error must belong to this plugin delivery attempt");
    }
}
