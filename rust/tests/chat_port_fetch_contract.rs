use futures_util::future::BoxFuture;
use qq_inner_core::engine::io::{
    adapters::onebot::OneBotAdapter, ChatAddress, ChatKind, ChatPort, Command,
    CommandResult, Fetch, MediaRef, MediaSrc, PortReply,
};
use serde_json::{json, Value};
use std::{collections::VecDeque, sync::{Arc, Mutex}};

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

fn adapter(answers: Vec<PortReply<Value>>) -> (OneBotAdapter, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let answers = Arc::new(Mutex::new(VecDeque::from(answers)));
    let action = Arc::new(move |name: String, params: Value| -> BoxFuture<'static, PortReply<Value>> {
        recorded.lock().unwrap().push((name, params));
        let answer = answers.lock().unwrap().pop_front().expect("unexpected protocol action");
        Box::pin(async move { answer })
    });
    let connection = Arc::new(|| (true, true, Some("99".into())));
    (OneBotAdapter::new("onebot", action, connection).unwrap(), calls)
}

fn group(id: &str) -> ChatAddress {
    ChatAddress::new(ChatKind::Group, "onebot", id).unwrap()
}

fn fetched(reply: PortReply<CommandResult>, expected: &Value) {
    assert!(matches!(&reply, PortReply::Ok(CommandResult::Fetched(data)) if data == expected),
        "the protocol data must remain unchanged, got {reply:?}");
}

#[tokio::test]
async fn group_info_history_and_members_are_raw_fetches() {
    let group_info = json!({"group_id":42,"group_name":"项目群","group_memo":"保留原文",
        "member_count":7,"unexpected_field":{"keep":true}});
    let history = json!({"messages":[{"message_id":"7","message_seq":17,
        "sender":{"nickname":"甲"},"raw_message":"你好"}]});
    let members = json!([{"user_id":7,"nickname":"甲","role":"admin",
        "extra":{"keep":[1,2]}}]);
    let (port, calls) = adapter(vec![
        PortReply::Ok(group_info.clone()),
        PortReply::Ok(history.clone()),
        PortReply::Ok(members.clone()),
    ]);

    fetched(port.deliver(Command::Fetch {
        port: "onebot".into(), request: Fetch::GroupInfo { chat: group("42"), no_cache: true },
    }).await, &group_info);
    fetched(port.deliver(Command::Fetch {
        port: "onebot".into(), request: Fetch::History {
            chat: group("42"), count: 12, before: Some("cursor-7".into()),
        },
    }).await, &history);
    fetched(port.deliver(Command::Fetch {
        port: "onebot".into(), request: Fetch::Members { chat: group("42"), no_cache: false },
    }).await, &members);

    let (without_cursor, no_cursor_calls) = adapter(vec![PortReply::Ok(history.clone())]);
    fetched(without_cursor.deliver(Command::Fetch {
        port: "onebot".into(), request: Fetch::History {
            chat: group("42"), count: 12, before: None,
        },
    }).await, &history);
    assert!(no_cursor_calls.lock().unwrap()[0].1.get("message_seq").is_none(),
        "history without a cursor must omit message_seq");

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, "get_group_info");
    assert_eq!(calls[1].0, "get_group_msg_history");
    assert_eq!(calls[2].0, "get_group_member_list");
    for (_, params) in calls.iter() {
        assert!(params["group_id"] == 42 || params["group_id"] == "42");
    }
    assert_eq!(calls[0].1["no_cache"], true);
    assert_eq!(calls[1].1["count"], 12);
    assert_eq!(calls[1].1["message_seq"], "cursor-7");
    assert_eq!(calls[2].1["no_cache"], false);
}

#[tokio::test]
async fn login_info_is_a_raw_fetch() {
    let login = json!({"user_id":99,"nickname":"机器人","extra":{"keep":"原样"}});
    let (port, calls) = adapter(vec![PortReply::Ok(login.clone())]);
    fetched(port.deliver(Command::Fetch {
        port: "onebot".into(), request: Fetch::Login { port: "onebot".into() },
    }).await, &login);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "get_login_info");
}

#[tokio::test]
async fn profile_avatar_and_group_card_are_typed_writes() {
    let (port, calls) = adapter(vec![PortReply::Ok(json!({})); 5]);
    for (nickname, signature) in [
        (Some("新名字"), None),
        (None, Some("新签名")),
        (Some("第二个名字"), Some("第二个签名")),
    ] {
        let reply = port.deliver(Command::SetProfile {
            port: "onebot".into(),
            nickname: nickname.map(str::to_owned),
            signature: signature.map(str::to_owned),
        }).await;
        assert!(matches!(reply, PortReply::Ok(CommandResult::Ok)));
    }
    let avatar = MediaRef {
        source: MediaSrc::Url("https://example.invalid/avatar.png".into()),
        kind: "image".into(),
    };
    assert!(matches!(port.deliver(Command::SetAvatar {
        port: "onebot".into(), media: avatar,
    }).await, PortReply::Ok(CommandResult::Ok)));
    assert!(matches!(port.deliver(Command::SetGroupCard {
        chat: group("42"), card: "群名片".into(),
    }).await, PortReply::Ok(CommandResult::Ok)));

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 5);
    for (action, params) in &calls[..3] {
        assert_eq!(action, "set_qq_profile");
        assert!(params.as_object().is_some());
    }
    assert_eq!(calls[0].1["nickname"], "新名字");
    assert!(calls[0].1.get("signature").is_none());
    assert!(calls[1].1.get("nickname").is_none());
    assert_eq!(calls[1].1["signature"], "新签名");
    assert_eq!(calls[2].1["nickname"], "第二个名字");
    assert_eq!(calls[2].1["signature"], "第二个签名");
    assert_eq!(calls[3].0, "set_qq_avatar");
    assert_eq!(calls[3].1["file"], "https://example.invalid/avatar.png");
    assert_eq!(calls[4].0, "set_group_card");
    assert!(calls[4].1["group_id"] == 42 || calls[4].1["group_id"] == "42");
    assert!(calls[4].1["user_id"] == 99 || calls[4].1["user_id"] == "99");
    assert_eq!(calls[4].1["card"], "群名片");
}

#[tokio::test]
async fn query_data_survives_byte_for_byte() {
    let history = json!({"messages":[{"message_id":"7","sender":{"nickname":"甲"},
        "raw_message":"你好"}]});
    let (port, calls) = adapter(vec![PortReply::Ok(history.clone())]);
    fetched(port.deliver(Command::Fetch {
        port: "onebot".into(), request: Fetch::History {
            chat: group("42"), count: 1, before: None,
        },
    }).await, &history);
    assert_eq!(calls.lock().unwrap()[0].0, "get_group_msg_history");
}

#[tokio::test]
async fn invalid_query_inputs_fail_before_any_action() {
    let (port, calls) = adapter(vec![]);
    for request in [
        Fetch::GroupInfo { chat: group("not-a-number"), no_cache: false },
        Fetch::GroupMember { chat: group("42"), user: String::new() },
        Fetch::History { chat: group("42"), count: 1, before: Some(String::new()) },
    ] {
        let reply = port.deliver(Command::Fetch { port: "onebot".into(), request }).await;
        assert!(matches!(reply, PortReply::Failed { uncertain: false, .. }),
            "invalid query must fail definitely before the protocol action: {reply:?}");
    }
    assert!(calls.lock().unwrap().is_empty());
}
