use futures_util::future::BoxFuture;
use qq_inner_core::engine::io::{
    adapters::onebot::OneBotAdapter, ChatAddress, ChatKind, ChatPort, Command, CommandResult,
    MediaRef, MediaSrc, PortReply, Segment,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const MESSAGE_ID: &str = "sent-image-42";

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

fn adapter(connected: bool, online: bool, answer: PortReply<Value>) -> (OneBotAdapter, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let action = Arc::new(move |name: String, params: Value| -> BoxFuture<'static, PortReply<Value>> {
        recorded.lock().unwrap().push((name, params));
        let answer = answer.clone();
        Box::pin(async move { answer })
    });
    let connection = Arc::new(move || (connected, online, None::<String>));
    (OneBotAdapter::new("onebot", action, connection).unwrap(), calls)
}

fn available() -> (OneBotAdapter, Calls) {
    adapter(true, true, PortReply::Ok(json!({"message_id":MESSAGE_ID})))
}

fn chat(kind: ChatKind) -> ChatAddress {
    ChatAddress::new(kind, "onebot", "42").unwrap()
}

fn image() -> MediaRef {
    MediaRef { source: MediaSrc::Url("https://example.invalid/picture.png".into()), kind: "image".into() }
}

fn image_segment(params: &Value) -> bool {
    params["message"].as_array().is_some_and(|segments| segments.iter().any(|segment| {
        segment["type"] == "image" && segment["data"].as_object()
            .is_some_and(|data| data.values().any(|value| value.as_str().is_some_and(|text| !text.is_empty())))
    }))
}

fn sent(reply: PortReply<CommandResult>) {
    assert!(matches!(&reply, PortReply::Ok(CommandResult::Sent { message_id }) if message_id == MESSAGE_ID),
        "a successful image command must return the protocol message ID, got {reply:?}");
}

#[tokio::test]
async fn send_media_reaches_group_action_as_an_image_segment() {
    let (port, calls) = available();
    let reply = port.deliver(Command::SendMedia { chat: chat(ChatKind::Group), media: image() }).await;
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "group image must result in exactly one protocol action");
    assert_eq!(calls[0].0, "send_group_msg");
    assert!(calls[0].1["group_id"] == 42 || calls[0].1["group_id"] == "42");
    assert!(image_segment(&calls[0].1), "protocol message must contain a real image segment: {}", calls[0].1);
    sent(reply);
}

#[tokio::test]
async fn send_media_reaches_private_action_as_an_image_segment() {
    let (port, calls) = available();
    let reply = port.deliver(Command::SendMedia { chat: chat(ChatKind::Private), media: image() }).await;
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "private image must result in exactly one protocol action");
    assert_eq!(calls[0].0, "send_private_msg");
    assert!(calls[0].1["user_id"] == 42 || calls[0].1["user_id"] == "42");
    assert!(image_segment(&calls[0].1), "protocol message must contain a real image segment: {}", calls[0].1);
    sent(reply);
}

#[tokio::test]
async fn send_with_image_segment_preserves_the_image_and_text() {
    let (port, calls) = available();
    let reply = port.deliver(Command::Send {
        chat: chat(ChatKind::Group),
        segments: vec![Segment::Text("看这张图".into()),
            Segment::Image { src: image().source, summary: Some("一张图".into()) }],
        reply_to: None,
    }).await;
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "send_group_msg");
    assert!(image_segment(&calls[0].1), "image must not disappear or become a text placeholder: {}", calls[0].1);
    assert!(calls[0].1["message"].as_array().unwrap().iter().any(|segment|
        segment["type"] == "text" && segment["data"]["text"] == "看这张图"));
    sent(reply);
}

#[tokio::test]
async fn media_declaration_matches_the_send_media_reply() {
    let (port, _calls) = available();
    let claimed = port.capabilities().media;
    let reply = port.deliver(Command::SendMedia { chat: chat(ChatKind::Group), media: image() }).await;
    assert_eq!(matches!(&reply, PortReply::Unsupported { .. }), !claimed,
        "a port claiming media may not reject it, and an unclaimed media capability may not pretend success; media={claimed}, reply={reply:?}");
}

#[test]
fn onebot_advertises_the_media_capability_required_for_image_delivery() {
    let (port, _calls) = available();
    assert!(port.capabilities().media,
        "deleting the built-in connection requires the published OneBot ChatPort to provide images");
}

#[tokio::test]
async fn offline_or_disconnected_is_a_definite_unsent_failure() {
    let (online, online_calls) = available();
    sent(online.deliver(Command::SendMedia { chat: chat(ChatKind::Group), media: image() }).await);
    assert_eq!(online_calls.lock().unwrap().len(), 1,
        "control case must prove the same media command is deliverable while online");
    for (connected, online) in [(false, false), (true, false)] {
        let (port, calls) = adapter(connected, online, PortReply::Ok(json!({"message_id":MESSAGE_ID})));
        let reply = port.deliver(Command::SendMedia { chat: chat(ChatKind::Group), media: image() }).await;
        assert!(matches!(reply, PortReply::Failed { uncertain: false, .. }),
            "offline image send must be a definite failure, got {reply:?}");
        assert!(calls.lock().unwrap().is_empty(), "offline port must not invoke the action callback");
    }
}

#[tokio::test]
async fn action_timeout_or_disconnect_preserves_uncertain_delivery() {
    for code in ["timeout", "connection_lost"] {
        let (port, calls) = adapter(true, true,
            PortReply::Failed { code: code.into(), uncertain: true });
        let reply = port.deliver(Command::SendMedia { chat: chat(ChatKind::Group), media: image() }).await;
        assert_eq!(calls.lock().unwrap().len(), 1, "the action must have been attempted");
        assert!(matches!(reply, PortReply::Failed { uncertain: true, .. }),
            "possibly sent image must not become success or a definite failure: {reply:?}");
    }
}

#[tokio::test]
async fn invalid_chat_or_media_is_rejected_before_any_action() {
    let (valid, valid_calls) = available();
    sent(valid.deliver(Command::SendMedia { chat: chat(ChatKind::Group), media: image() }).await);
    assert_eq!(valid_calls.lock().unwrap().len(), 1,
        "control case must prove rejection is due to bad input, not blanket media unsupported");
    for (chat, media) in [
        (ChatAddress { kind: ChatKind::Group, port: "onebot".into(), id: String::new() }, image()),
        (chat(ChatKind::Group), MediaRef { source: MediaSrc::Url("not a url".into()), kind: "image".into() }),
        (chat(ChatKind::Group), MediaRef { source: MediaSrc::Opaque(String::new()), kind: "image".into() }),
    ] {
        let (port, calls) = available();
        let reply = port.deliver(Command::SendMedia { chat, media }).await;
        assert!(matches!(reply, PortReply::Failed { uncertain: false, .. }),
            "invalid input must fail definitely, got {reply:?}");
        assert!(calls.lock().unwrap().is_empty(), "invalid input must not reach NapCat action");
    }
}
