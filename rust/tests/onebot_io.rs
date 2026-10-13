//! P2 OneBot boundary: protocol identity/time, capability truthfulness, ACKs.
use futures_util::future::BoxFuture;
use qq_inner_core::engine::io::{
    adapters::onebot::{Action, OneBotAdapter}, ChatPort, ChatAddress, Command,
    CommandResult, PortReply, Segment,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn adapter(
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    reply: PortReply<Value>,
) -> OneBotAdapter {
    let action: Action = Arc::new(move |name, params| {
        calls.lock().unwrap().push((name, params));
        let reply = reply.clone();
        Box::pin(async move { reply }) as BoxFuture<'static, PortReply<Value>>
    });
    OneBotAdapter::new("onebot", action, Arc::new(|| (true, true))).unwrap()
}

#[test]
fn incoming_keeps_protocol_id_and_both_clocks_separate() {
    let adapter = adapter(Arc::new(Mutex::new(vec![])), PortReply::Ok(json!({})));
    let incoming = adapter.map_event(&json!({
        "post_type":"message","message_type":"group","group_id":"room:alpha",
        "self_id":"99","user_id":"user-x","message_id":"proto:7","time":100.0,
        "sender":{"nickname":"甲"},"message":"[CQ:at,qq=99]你好"
    }), 105.0).unwrap().unwrap();
    assert_eq!(incoming.chat.to_string(), "group:onebot:room:alpha");
    assert_eq!(incoming.message_id, "proto:7");
    assert_eq!((incoming.ts, incoming.received_at), (100.0, 105.0));
    assert_eq!(incoming.segments, vec![Segment::Mention { id: "99".into() }, Segment::Text("你好".into())]);
    assert!(adapter.map_event(&json!({"post_type":"meta_event"}), 105.0).unwrap().is_none());
    assert_eq!(adapter.map_event(&json!({"post_type":"notice"}), 105.0).unwrap_err().code,
        "incoming_event_unimplemented");
}

#[tokio::test]
async fn send_and_recall_require_confirmed_action_ack() {
    let calls = Arc::new(Mutex::new(vec![]));
    let port = adapter(calls.clone(), PortReply::Ok(json!({"message_id":"ack:8"})));
    let chat: ChatAddress = "group:onebot:123".parse().unwrap();
    assert!(!port.capabilities().history);
    assert!(!port.capabilities().media);
    assert!(port.capabilities().mention && port.capabilities().reply);
    assert_eq!(port.deliver(Command::Send {
        chat, segments: vec![Segment::Text("你好".into())], reply_to: None,
    }).await, PortReply::Ok(CommandResult::Sent { message_id: "ack:8".into() }));
    assert_eq!(calls.lock().unwrap()[0].0, "send_group_msg");
    assert_eq!(port.deliver(Command::Recall { port: "onebot".into(), message_id: "ack:8".into() }).await,
        PortReply::Ok(CommandResult::Ok));
    assert_eq!(calls.lock().unwrap()[1], ("delete_msg".into(), json!({"message_id":"ack:8"})));

    let missing = adapter(Arc::new(Mutex::new(vec![])), PortReply::Ok(json!({})));
    assert_eq!(missing.deliver(Command::Send {
        chat: "private:onebot:123".parse().unwrap(),
        segments: vec![Segment::Text("x".into())], reply_to: None,
    }).await, PortReply::Failed { code: "missing_ack_message_id".into(), uncertain: true });
    let timed_out = adapter(Arc::new(Mutex::new(vec![])), PortReply::Failed {
        code: "action_timeout".into(), uncertain: true,
    });
    assert_eq!(timed_out.deliver(Command::Recall { port: "onebot".into(), message_id: "7".into() }).await,
        PortReply::Failed { code: "action_timeout".into(), uncertain: true });
}
