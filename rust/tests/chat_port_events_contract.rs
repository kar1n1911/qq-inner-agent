use futures_util::future::BoxFuture;
use qq_inner_core::engine::io::{
    adapters::onebot::OneBotAdapter, ChatAddress, ChatKind, ChatPort, NoticeKind, PortEvent,
    PortReply,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

fn adapter(connected: bool, online: bool, self_id: Option<&str>) -> OneBotAdapter {
    let action = Arc::new(
        |_action: String, _params: Value| -> BoxFuture<'static, PortReply<Value>> {
            Box::pin(async { PortReply::Ok(json!({})) })
        },
    );
    let self_id = self_id.map(str::to_owned);
    let connection = Arc::new(move || (connected, online, self_id.clone()));
    OneBotAdapter::new("onebot", action, connection).expect("construct OneBot adapter")
}

fn group_message(message_id: i64, text: &str) -> Value {
    json!({
        "post_type": "message",
        "message_type": "group",
        "sub_type": "normal",
        "group_id": 1127261371_i64,
        "user_id": 42,
        "self_id": 3879337324_i64,
        "message_id": message_id,
        "time": 1760000000_i64,
        "sender": { "user_id": 42, "nickname": "群友" },
        "message": [{ "type": "text", "data": { "text": text } }],
        "raw_message": text,
    })
}

fn group_increase() -> Value {
    json!({
        "post_type": "notice",
        "notice_type": "group_increase",
        "group_id": 1127261371_i64,
        "user_id": 3879337324_i64,
        "self_id": 3879337324_i64,
        "time": 1760000000_i64,
    })
}

#[test]
fn notices_reach_the_engine_on_the_same_ordered_stream() {
    let port = adapter(true, true, Some("3879337324"));
    let (sink, mut events) = mpsc::channel(16);
    port.attach(sink).expect("attach one ordered event stream");

    for frame in [
        group_message(101, "第一条"),
        group_increase(),
        group_message(102, "第二条"),
    ] {
        let event = port
            .map_event(&frame, 1760000000.0)
            .expect("map OneBot frame")
            .expect("message and notice must both become events");
        port.publish(event).expect("deliver mapped event");
    }

    assert!(matches!(
        events.try_recv(),
        Ok(PortEvent::Incoming(incoming)) if incoming.message_id == "101"
    ));
    assert!(matches!(
        events.try_recv(),
        Ok(PortEvent::Notice(notice)) if matches!(notice.kind, NoticeKind::GroupJoined)
    ));
    assert!(matches!(
        events.try_recv(),
        Ok(PortEvent::Incoming(incoming)) if incoming.message_id == "102"
    ));
    assert!(events.try_recv().is_err(), "only three frames were delivered");
}

#[test]
fn group_increase_keeps_chat_actor_self_id_and_time() {
    let port = adapter(true, true, Some("3879337324"));
    let event = port
        .map_event(&group_increase(), 1760000001.0)
        .expect("map group increase")
        .expect("group increase must not disappear");
    let PortEvent::Notice(notice) = event else {
        panic!("group increase must remain a notice, not a message");
    };

    assert!(matches!(notice.kind, NoticeKind::GroupJoined));
    assert_eq!(
        notice.chat,
        Some(ChatAddress::new(ChatKind::Group, "onebot", "1127261371").unwrap())
    );
    assert_eq!(notice.actor.as_deref(), Some("3879337324"));
    assert_eq!(notice.self_id.as_deref(), Some("3879337324"));
    assert_eq!(notice.at, 1760000000.0);
}

#[test]
fn unknown_notice_kind_is_visible_instead_of_dropped() {
    let port = adapter(true, true, Some("3879337324"));
    let (sink, mut events) = mpsc::channel(16);
    port.attach(sink).expect("attach notice stream");
    let frame = json!({
        "post_type": "notice",
        "notice_type": "group_recall",
        "group_id": 1127261371_i64,
        "user_id": 42,
        "self_id": 3879337324_i64,
        "time": 1760000000_i64,
    });

    let event = port
        .map_event(&frame, 1760000001.0)
        .expect("unknown notice must not be rejected")
        .expect("unknown notice must not be dropped");
    assert!(matches!(
        &event,
        PortEvent::Notice(notice) if matches!(&notice.kind, NoticeKind::Unknown(kind) if kind == "group_recall")
    ));
    port.publish(event).expect("deliver unknown notice");
    assert!(matches!(
        events.try_recv(),
        Ok(PortEvent::Notice(notice)) if matches!(notice.kind, NoticeKind::Unknown(kind) if kind == "group_recall")
    ));
    assert!(events.try_recv().is_err());
}

#[test]
fn heartbeat_stays_silent() {
    let port = adapter(true, true, Some("3879337324"));
    let (sink, mut events) = mpsc::channel(16);
    port.attach(sink).expect("attach event stream");
    let heartbeat = json!({
        "post_type": "meta_event",
        "meta_event_type": "heartbeat",
        "time": 1760000000_i64,
    });

    assert!(port.map_event(&heartbeat, 1760000001.0).unwrap().is_none());
    assert!(events.try_recv().is_err(), "heartbeat must not be published");
}

#[test]
fn notice_without_group_is_not_fabricated_as_a_chat() {
    let port = adapter(true, true, Some("3879337324"));
    let frame = json!({
        "post_type": "notice",
        "notice_type": "friend_add",
        "user_id": 42,
        "self_id": 3879337324_i64,
        "time": 1760000000_i64,
    });

    let event = port
        .map_event(&frame, 1760000001.0)
        .expect("map friend notice")
        .expect("friend notice must remain visible");
    let PortEvent::Notice(notice) = event else {
        panic!("friend notice must not masquerade as a message");
    };
    assert!(notice.chat.is_none());
    assert!(matches!(notice.kind, NoticeKind::Unknown(kind) if kind == "friend_add"));
}

#[test]
fn state_distinguishes_connected_online_degraded_and_disconnected() {
    let available = adapter(true, true, Some("3879337324")).state();
    assert!(available.connected);
    assert!(available.online);
    assert!(available.degraded.is_none());
    assert!(available.is_available());
    assert_eq!(available.label(), "available");

    let degraded = adapter(true, false, Some("3879337324")).state();
    assert!(degraded.connected);
    assert!(!degraded.online);
    assert!(degraded.degraded.as_deref().is_some_and(|reason| !reason.is_empty()));
    assert!(!degraded.is_available());
    assert_eq!(degraded.label(), "degraded");

    let disconnected = adapter(false, false, Some("3879337324")).state();
    assert!(!disconnected.connected);
    assert!(!disconnected.online);
    assert!(!disconnected.is_available());
    assert_eq!(disconnected.label(), "disconnected");
}

#[test]
fn self_id_is_absent_not_empty_when_unknown() {
    let unknown = adapter(true, true, None).state();
    assert_eq!(unknown.self_id, None);

    let known = adapter(true, true, Some("3879337324")).state();
    assert_eq!(known.self_id.as_deref(), Some("3879337324"));
}

#[test]
fn onebot_declares_notice_delivery_because_it_delivers_it() {
    let port = adapter(true, true, Some("3879337324"));
    assert!(port.capabilities().notices, "notice support must be declared");

    let (sink, mut events) = mpsc::channel(16);
    port.attach(sink).expect("attach notice stream");
    let event = port
        .map_event(&group_increase(), 1760000001.0)
        .expect("map group increase")
        .expect("declared notice support must produce an event");
    port.publish(event).expect("deliver declared notice capability");
    assert!(matches!(
        events.try_recv(),
        Ok(PortEvent::Notice(notice)) if matches!(notice.kind, NoticeKind::GroupJoined)
    ));
}
