use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{
        io::{
            adapters::onebot::OneBotAdapter, ChatAddress, ChatKind, ChatPort, Command,
            CommandResult, Fetch, PortReply,
        },
        orientation::{OrientationProvider, OrientationTransport},
        Engine, EngineTransport, Options,
    },
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

fn adapter(answer: PortReply<Value>) -> (OneBotAdapter, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let action = Arc::new(move |name: String, params: Value| -> BoxFuture<'static, PortReply<Value>> {
        recorded.lock().unwrap().push((name, params));
        let answer = answer.clone();
        Box::pin(async move { answer })
    });
    let connection = Arc::new(|| (true, true, Some("99".into())));
    (OneBotAdapter::new("onebot", action, connection).unwrap(), calls)
}

fn group() -> ChatAddress {
    ChatAddress::new(ChatKind::Group, "onebot", "42").unwrap()
}

fn fetched(reply: PortReply<CommandResult>, expected: &Value) {
    assert!(matches!(&reply, PortReply::Ok(CommandResult::Fetched(data)) if data == expected),
        "protocol response must be returned unchanged, got {reply:?}");
}

#[tokio::test]
async fn group_notices_are_a_typed_command() {
    let notices = json!([{"id":"notice-1","message":"群公告"}]);
    let (port, calls) = adapter(PortReply::Ok(notices.clone()));
    let reply = port.deliver(Command::Fetch {
        port: "onebot".into(),
        request: Fetch::GroupNotices { chat: group() },
    }).await;

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "_get_group_notice");
    assert!(calls[0].1["group_id"] == 42 || calls[0].1["group_id"] == "42");
    fetched(reply, &notices);
}

#[tokio::test]
async fn group_member_and_stranger_queries_are_typed_commands() {
    let member = json!({"user_id":7,"nickname":"群友","role":"member"});
    let (port, calls) = adapter(PortReply::Ok(member.clone()));
    let reply = port.deliver(Command::Fetch {
        port: "onebot".into(),
        request: Fetch::GroupMember { chat: group(), user: "7".into() },
    }).await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0].0, "get_group_member_info");
    assert!(calls.lock().unwrap()[0].1["group_id"] == 42
        || calls.lock().unwrap()[0].1["group_id"] == "42");
    assert!(calls.lock().unwrap()[0].1["user_id"] == 7
        || calls.lock().unwrap()[0].1["user_id"] == "7");
    fetched(reply, &member);

    let stranger = json!({"user_id":8,"nickname":"陌生人"});
    let (port, calls) = adapter(PortReply::Ok(stranger.clone()));
    let reply = port.deliver(Command::Fetch {
        port: "onebot".into(),
        request: Fetch::Stranger { port: "onebot".into(), user: "8".into() },
    }).await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0].0, "get_stranger_info");
    assert!(calls.lock().unwrap()[0].1["user_id"] == 8
        || calls.lock().unwrap()[0].1["user_id"] == "8");
    fetched(reply, &stranger);

    let (port, calls) = adapter(PortReply::Ok(json!({"user_id":8})));
    let mismatch = port.deliver(Command::Fetch {
        port: "onebot".into(),
        request: Fetch::Stranger { port: "other".into(), user: "8".into() },
    }).await;
    assert!(matches!(mismatch, PortReply::Failed { uncertain: false, .. }),
        "outer and request ports must match before any protocol action");
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn long_nick_is_a_typed_command() {
    let text = "一句新的个性签名";
    let (port, calls) = adapter(PortReply::Ok(json!({})));
    let reply = port.deliver(Command::SetLongNick { port: "onebot".into(), text: text.into() }).await;
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "set_self_longnick");
    assert_eq!(calls[0].1["longNick"], text);
    assert!(matches!(reply, PortReply::Ok(CommandResult::Ok)));

    let (port, calls) = adapter(PortReply::Failed { code: "action_timeout".into(), uncertain: true });
    let reply = port.deliver(Command::SetLongNick { port: "onebot".into(), text: text.into() }).await;
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(matches!(reply, PortReply::Failed { code, uncertain: true } if code == "action_timeout"));
}

fn known_command_only(command: &Command) {
    match command {
        Command::Send { .. }
        | Command::SendMedia { .. }
        | Command::Recall { .. }
        | Command::FetchHistory { .. }
        | Command::FetchForward { .. }
        | Command::FetchMembers { .. }
        | Command::FetchGroupInfo { .. }
        | Command::FetchFriendList { .. }
        | Command::FetchGroupList { .. }
        | Command::LoginInfo { .. }
        | Command::Health { .. }
        | Command::SetGroupCard { .. }
        | Command::SetProfile { .. }
        | Command::SetAvatar { .. }
        | Command::Fetch { .. }
        | Command::SetLongNick { .. } => {}
    }
}

#[test]
fn unknown_actions_are_not_invented() {
    // Deliberately exhaustive: a future Command::Raw must require review here.
    known_command_only(&Command::Health { port: "onebot".into() });
}

struct LegacyTransport { sends: Mutex<usize> }

impl OrientationTransport for LegacyTransport {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, _action: &'a str, _params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected raw transport call") })
    }
}

impl EngineTransport for LegacyTransport {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "99".into(), ..State::default() }
    }
    fn send<'a>(&'a self, _chat: &'a str, _text: &'a str, _face: Option<&'a str>,
        _reply_to: Option<&'a str>, _mention: Option<&'a str>)
        -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            *self.sends.lock().unwrap() += 1;
            Ok(json!({"message_id":"unexpected-built-in-send"}))
        })
    }
}

struct ReplyModel;

impl OrientationProvider for ReplyModel {
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if system == qq_inner_core::persona::NAME_PROMPT { return Ok(json!("清风")); }
            let stage = system.split("TASK: ").nth(1).unwrap_or_default()
                .split_whitespace().next().unwrap_or_default();
            Ok(match stage {
                "FORM" => json!({"allocation":"self","candidates":[{"kind":"system2","text":"接住问题"}]}),
                "EVALUATE" => json!({"ratings":payload["candidates"].as_array().unwrap().iter()
                    .map(|candidate| json!({"id":candidate["id"],"motivation":5,
                        "relevance":5,"originality":5,"for":["relevance"],"against":[]}))
                    .collect::<Vec<_>>()}),
                "FORECAST" => json!({"shouldSend":true,"outcomes":{"reply":1.,"silence":0.,"negative":0.},
                    "responseMode":"answer","plan":"回复"}),
                "ARTICULATE" => json!({"text":"你好，我在。"}),
                "ORIENT" => json!({"style":"谨慎接话","summary":"当前群聊","topics":[]}),
                other => panic!("unexpected model stage: {other}"),
            })
        })
    }
}

#[tokio::test]
async fn unbound_send_fails_when_a_primary_port_is_bound() {
    let config = Config::from_value(&merge(&defaults(), &json!({
        "apiKey":"mock", "onebotToken":"", "dataDir":"unused", "provider":{"model":"mock"},
        "agent":{"allowedGroups":["42"],"allowedUsers":["7"],"quietHours":null,
            "debounceSeconds":0,"minThinkIntervalSeconds":1,"activeWindowSeconds":1000,
            "maxMessagesPerHour":100,"maxProactivePerHour":100,
            "schedule":{"enabled":false},"rhythm":{"enabled":false},
            "observation":{"enabled":false},"learning":{"enabled":false},
            "sending":{"enabled":false}}
    }))).unwrap();
    let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
    let transport = Arc::new(LegacyTransport { sends: Mutex::new(0) });
    let observations = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let observed = observations.clone();
    let options = Options {
        now: Arc::new(|| 1_700_000_000.),
        random: Arc::new(|| 0.),
        expression_random: Arc::new(|| 0.7),
        activity_random: Arc::new(|| 0.),
        selection_random: Arc::new(|| 0.),
        log: Arc::new(move |name, value| observed.lock().unwrap().push((name.into(), value))),
        ..Options::default()
    };
    let engine = Engine::new(config, store.clone(), Arc::new(ReplyModel), transport.clone(), options).unwrap();
    let (primary, _) = adapter(PortReply::Ok(json!({})));
    engine.set_chat_port(Arc::new(primary));
    assert!(engine.chat_port_for("group:42").is_none(), "the configured primary is not a chat binding");
    engine.ingest(&json!({"post_type":"message","message_type":"group",
        "group_id":42,"user_id":7,"self_id":99,"message_id":"in-1",
        "time":1_700_000_000.,"sender":{"nickname":"群友"},
        "message":"[CQ:at,qq=99]你好"})).unwrap();
    engine.tick().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), engine.wait_idle()).await.unwrap();

    assert_eq!(*transport.sends.lock().unwrap(), 0, "unbound group must never use the built-in sender");
    assert!(observations.lock().unwrap().iter().any(|(name, data)|
        name == "chat_route_unbound" && data["chat"] == "group:42"
            && data["reason"] == "chat_port_unbound" && data["uncertain"] == false),
        "a definite unbound failure must be visible with the group and reason");
    assert_eq!(engine.last_error().as_deref(), Some("chat_port_unbound"));
    let statuses = store.lock().unwrap()
        .rows("SELECT status FROM deliveries WHERE chat='group:42'", []).unwrap();
    assert!(statuses.iter().any(|row| row["status"] == "failed"),
        "the attempted send must be recorded as definitely failed: {statuses:?}");
}

#[tokio::test]
async fn unbound_send_falls_back_while_no_port_is_bound() {
    let config = Config::from_value(&merge(&defaults(), &json!({
        "apiKey":"mock", "onebotToken":"", "dataDir":"unused", "provider":{"model":"mock"},
        "agent":{"allowedGroups":["42"],"allowedUsers":["7"],"quietHours":null,
            "debounceSeconds":0,"minThinkIntervalSeconds":1,"activeWindowSeconds":1000,
            "maxMessagesPerHour":100,"maxProactivePerHour":100,
            "schedule":{"enabled":false},"rhythm":{"enabled":false},
            "observation":{"enabled":false},"learning":{"enabled":false},
            "sending":{"enabled":false}}
    }))).unwrap();
    let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
    let transport = Arc::new(LegacyTransport { sends: Mutex::new(0) });
    let observations = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let observed = observations.clone();
    let options = Options {
        now: Arc::new(|| 1_700_000_000.),
        random: Arc::new(|| 0.),
        expression_random: Arc::new(|| 0.7),
        activity_random: Arc::new(|| 0.),
        selection_random: Arc::new(|| 0.),
        log: Arc::new(move |name, value| observed.lock().unwrap().push((name.into(), value))),
        ..Options::default()
    };
    let engine = Engine::new(config, store, Arc::new(ReplyModel), transport.clone(), options).unwrap();
    assert!(engine.chat_port_for("group:42").is_none());
    engine.ingest(&json!({"post_type":"message","message_type":"group",
        "group_id":42,"user_id":7,"self_id":99,"message_id":"in-1",
        "time":1_700_000_000.,"sender":{"nickname":"群友"},
        "message":"[CQ:at,qq=99]你好"})).unwrap();
    engine.tick().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), engine.wait_idle()).await.unwrap();

    assert_eq!(*transport.sends.lock().unwrap(), 1, "no primary port must preserve the built-in send path");
    assert!(observations.lock().unwrap().iter().any(|(name, data)|
        name == "chat_route_fallback" && data["chat"] == "group:42"
            && data["route"] == "built_in_fallback"
            && data["reason"] == "chat_port_unbound"),
        "the transitional fallback must remain visible for this group");
}
