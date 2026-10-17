//! Contract for a OneBot ChatPort that owns its loopback WebSocket connection.
use futures_util::{SinkExt, StreamExt};
use qq_inner_core::{
    config,
    engine::io::{
        adapters::onebot::OneBotAdapter, ChatAddress, ChatKind, ChatPort, Command,
        CommandResult, NoticeKind, PortEvent, PortReply, Segment,
    },
};
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    sync::mpsc,
    task::JoinHandle,
    time::{sleep, timeout, Duration},
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

const TOKEN: &str = "a+b &?";
const SELF_ID: &str = "123";

struct Mock {
    url: String,
    requests: mpsc::UnboundedReceiver<Value>,
    frames: mpsc::UnboundedSender<Message>,
    task: JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

// The existing mock's URL-token check and first status response are protocol
// fixtures. The tests below observe only the plugin's public ChatPort surface.
#[allow(clippy::result_large_err)]
async fn mock() -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let (request_tx, requests) = mpsc::unbounded_channel();
    let (frames, mut frame_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = accept_hdr_async(stream, |req: &Request, response: Response| {
                if req.uri().query() == Some("access_token=a%2Bb+%26%3F") {
                    Ok(response)
                } else {
                    Err(tokio_tungstenite::tungstenite::http::Response::builder()
                        .status(401)
                        .body(Some("unauthorized".into()))
                        .unwrap())
                }
            })
            .await;
            let Ok(mut ws) = socket else {
                continue;
            };
            let mut handshaking = true;
            loop {
                tokio::select! {
                    Some(frame) = frame_rx.recv() => {
                        if ws.send(frame).await.is_err() { break; }
                    }
                    incoming = ws.next() => {
                        let Some(Ok(Message::Text(text))) = incoming else { break; };
                        let req: Value = serde_json::from_str(&text).unwrap();
                        let data = match req["action"].as_str().unwrap() {
                            "get_login_info" => Some(json!({ "user_id": 123 })),
                            "get_status" if handshaking => {
                                handshaking = false;
                                Some(json!({ "online": true }))
                            }
                            _ => None,
                        };
                        if let Some(data) = data {
                            if ws.send(response(&req, data)).await.is_err() { break; }
                        } else {
                            request_tx.send(req).unwrap();
                        }
                    }
                }
            }
        }
    });
    Mock { url, requests, frames, task }
}

fn response(request: &Value, data: Value) -> Message {
    Message::Text(
        json!({ "echo": request["echo"], "status": "ok", "retcode": 0, "data": data })
            .to_string(),
    )
}

fn config(mock: &Mock) -> config::Onebot {
    let mut config: config::Onebot =
        serde_json::from_value(config::defaults()["onebot"].clone()).unwrap();
    config.url = mock.url.clone();
    config.self_id.text = SELF_ID.into();
    config.self_id.is_truthy = true;
    config.request_timeout_seconds = 0.15;
    config.heartbeat_seconds = 10.0;
    config.reconnect_max_seconds = 1.0;
    config
}

async fn connected(port: &OneBotAdapter) {
    timeout(Duration::from_secs(15), async {
        loop {
            let state = port.state();
            if state.connected && state.online { break; }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("plugin must establish its own connection");
}

fn message(message_id: i64) -> Message {
    Message::Text(
        json!({
            "post_type": "message",
            "message_type": "group",
            "group_id": 42,
            "user_id": 456,
            "self_id": 123,
            "message_id": message_id,
            "time": 1760000000_i64,
            "sender": { "user_id": 456, "nickname": "群友" },
            "message": [{ "type": "text", "data": { "text": "你好" } }],
        })
        .to_string(),
    )
}

fn group_increase() -> Message {
    Message::Text(
        json!({
            "post_type": "notice",
            "notice_type": "group_increase",
            "group_id": 42,
            "user_id": 123,
            "self_id": 123,
            "time": 1760000000_i64,
        })
        .to_string(),
    )
}

#[tokio::test]
async fn plugin_connects_without_any_host_callback() {
    let mock = mock().await;
    let port = OneBotAdapter::connect("onebot", config(&mock), TOKEN.into())
        .expect("plugin constructs its own connection");
    connected(&port).await;

    assert_eq!(port.state().self_id.as_deref(), Some(SELF_ID));
}

#[tokio::test]
async fn plugin_receives_messages_and_notices_on_its_own_socket() {
    let mock = mock().await;
    let port = OneBotAdapter::connect("onebot", config(&mock), TOKEN.into()).unwrap();
    let (sink, mut events) = mpsc::channel(16);
    port.attach(sink).unwrap();
    connected(&port).await;

    mock.frames.send(message(101)).unwrap();
    mock.frames.send(group_increase()).unwrap();
    mock.frames.send(message(102)).unwrap();
    let first = timeout(Duration::from_secs(15), events.recv()).await.unwrap().unwrap();
    let second = timeout(Duration::from_secs(15), events.recv()).await.unwrap().unwrap();
    let third = timeout(Duration::from_secs(15), events.recv()).await.unwrap().unwrap();
    assert!(matches!(first, PortEvent::Incoming(incoming) if incoming.message_id == "101"));
    assert!(matches!(second, PortEvent::Notice(notice) if matches!(notice.kind, NoticeKind::GroupJoined)));
    assert!(matches!(third, PortEvent::Incoming(incoming) if incoming.message_id == "102"));

    mock.frames
        .send(Message::Text(json!({ "post_type": "meta_event", "meta_event_type": "heartbeat" }).to_string()))
        .unwrap();
    assert!(timeout(Duration::from_millis(200), events.recv()).await.is_err(), "heartbeat must stay silent");
}

#[tokio::test]
async fn plugin_reports_its_own_disconnect() {
    let mock = mock().await;
    let port = OneBotAdapter::connect("onebot", config(&mock), TOKEN.into()).unwrap();
    connected(&port).await;
    assert!(port.state().connected);

    drop(mock);
    timeout(Duration::from_secs(15), async {
        loop {
            if !port.state().connected { break; }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a lost plugin socket must change the port state");
}

#[tokio::test]
async fn plugin_delivers_commands_over_its_own_socket() {
    let mut mock = mock().await;
    let port = OneBotAdapter::connect("onebot", config(&mock), TOKEN.into()).unwrap();
    connected(&port).await;
    let command = Command::Send {
        chat: ChatAddress::new(ChatKind::Group, "onebot", "42").unwrap(),
        segments: vec![Segment::Text("你好".into())],
        reply_to: None,
    };

    let sent = tokio::spawn(async move { port.deliver(command).await });
    let request = timeout(Duration::from_secs(15), mock.requests.recv())
        .await
        .unwrap()
        .expect("the plugin must send a command over its own socket");
    assert_eq!(request["action"], "send_group_msg");
    assert_eq!(request["params"]["group_id"], 42);
    assert!(request["params"]["message"]
        .as_array()
        .unwrap()
        .iter()
        .any(|segment| segment["type"] == "text" && segment["data"]["text"] == "你好"));
    mock.frames.send(response(&request, json!({ "message_id": "ack:1" }))).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(15), sent).await.unwrap().unwrap(),
        PortReply::Ok(CommandResult::Sent { message_id }) if message_id == "ack:1"
    ));
}

#[tokio::test]
async fn connected_plugin_still_serves_the_typed_port_surface() {
    let mock = mock().await;
    let port = OneBotAdapter::connect("onebot", config(&mock), TOKEN.into()).unwrap();
    connected(&port).await;
    let capabilities = port.capabilities();
    assert!(capabilities.notices);
    assert!(capabilities.media);

    let (first_sink, _first_events) = mpsc::channel(16);
    let (second_sink, _second_events) = mpsc::channel(16);
    port.attach(first_sink).unwrap();
    let error = port.attach(second_sink).expect_err("one stream may attach only once");
    assert_eq!(error.code, "sink_already_attached");
}
