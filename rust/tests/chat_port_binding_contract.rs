use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        io::{adapters::onebot::OneBotAdapter, ChatPort, PortReply},
        orientation::{OrientationProvider, OrientationTransport},
        Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct HarnessTransport {
    state: Mutex<State>,
}

impl OrientationTransport for HarnessTransport {
    fn self_id(&self) -> String {
        self.state.lock().unwrap().self_id.clone()
    }

    fn call<'a>(&'a self, _action: &'a str, _params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected transport call") })
    }
}

impl EngineTransport for HarnessTransport {
    fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    fn send<'a>(
        &'a self,
        _chat: &'a str,
        _text: &'a str,
        _face: Option<&'a str>,
        _reply_to: Option<&'a str>,
        _mention: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async { panic!("these binding and state tests must not send") })
    }
}

struct UnusedProvider;

impl OrientationProvider for UnusedProvider {
    fn json<'a>(&'a self, _system: &'a str, _payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected model call") })
    }
}

fn setup(allowed_groups: &[&str], transport_state: State) -> (Arc<Engine>, Arc<HarnessTransport>) {
    let config = Config::from_value(&merge(
        &defaults(),
        &json!({
            "apiKey": "mock",
            "onebotToken": "",
            "dataDir": "unused",
            "provider": { "model": "mock" },
            "agent": { "allowedGroups": allowed_groups },
        }),
    ))
    .unwrap();
    let transport = Arc::new(HarnessTransport {
        state: Mutex::new(transport_state),
    });
    let engine = Engine::new(
        config,
        Arc::new(Mutex::new(Store::in_memory().unwrap())),
        Arc::new(UnusedProvider),
        transport.clone(),
        Options::default(),
    )
    .unwrap();
    (engine, transport)
}

fn transport_state(connected: bool, online: bool, self_id: &str, degraded: bool) -> State {
    State {
        connected,
        online,
        self_id: self_id.into(),
        reconnects: 0,
        degraded,
    }
}

fn primary_port(connected: bool, online: bool, self_id: Option<&str>) -> Arc<dyn ChatPort> {
    let self_id = self_id.map(str::to_owned);
    Arc::new(
        OneBotAdapter::new(
            "onebot",
            Arc::new(|_action, _params| {
                Box::pin(async { PortReply::Ok(json!({ "message_id": "unexpected" })) })
            }),
            Arc::new(move || (connected, online, self_id.clone())),
        )
        .unwrap(),
    )
}

#[test]
fn configured_groups_bind_without_any_inbound() {
    let (engine, _) = setup(&["42", "43"], transport_state(true, true, "99", false));
    let primary = primary_port(true, true, Some("777"));
    engine.set_chat_port(primary.clone());
    engine.bind_configured_chats().unwrap();

    let group_42 = engine.chat_port_for("group:42").expect("configured group 42 bound");
    let group_43 = engine.chat_port_for("group:43").expect("configured group 43 bound");
    assert!(Arc::ptr_eq(&group_42, &primary));
    assert!(Arc::ptr_eq(&group_43, &primary));
    assert!(Arc::ptr_eq(&group_42, &group_43), "both groups use one primary port");
    assert!(engine.chat_port_for("group:44").is_none());
}

#[test]
fn binding_without_a_primary_port_is_a_no_op() {
    let (engine, _) = setup(&["42"], transport_state(true, true, "99", false));

    engine.bind_configured_chats().expect("absence of a plugin is not an error");
    assert!(engine.chat_port_for("group:42").is_none());
}

#[test]
fn primary_port_state_overrides_the_transport() {
    let (engine, _) = setup(&["42"], transport_state(true, true, "99", false));
    engine.set_chat_port(primary_port(false, false, None));

    let offline = engine.connection_state();
    assert!(!offline.connected, "plugin is disconnected although transport is connected");
    assert!(!offline.is_available());

    engine.set_chat_port(primary_port(true, true, Some("777")));
    let online = engine.connection_state();
    assert!(online.connected && online.online);
    assert_eq!(online.self_id.as_deref(), Some("777"));
}

#[test]
fn transport_remains_the_source_only_while_no_port_is_bound() {
    let (engine, transport) = setup(&["42"], transport_state(true, false, "99", true));

    let state = engine.connection_state();
    assert_eq!(state.connected, transport.state().connected);
    assert_eq!(state.online, transport.state().online);
    assert_eq!(state.self_id.as_deref(), Some("99"));
    assert!(state.degraded.as_deref().is_some_and(|reason| !reason.is_empty()));

    transport.state.lock().unwrap().self_id.clear();
    assert_eq!(engine.connection_state().self_id, None);
}

#[test]
fn an_unconfigured_group_still_has_no_port_after_binding() {
    let (engine, _) = setup(&["42"], transport_state(true, true, "99", false));
    engine.set_chat_port(primary_port(true, true, Some("777")));
    engine.bind_configured_chats().unwrap();

    assert!(engine.chat_port_for("group:42").is_some());
    assert!(engine.chat_port_for("group:99").is_none());
}
