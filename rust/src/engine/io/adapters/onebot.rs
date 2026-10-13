//! OneBot v11 boundary. The wire callback keeps this module independent of
//! the legacy transport implementation while preserving its ACK/error state.

use super::{ports::ChatPort, types::*};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub type Action = Arc<dyn Fn(String, Value) -> BoxFuture<'static, PortReply<Value>> + Send + Sync>;
pub type Connection = Arc<dyn Fn() -> (bool, bool) + Send + Sync>;

pub struct OneBotAdapter {
    manifest: PortManifest,
    action: Action,
    connection: Connection,
    sink: Mutex<Option<mpsc::Sender<Incoming>>>,
}

impl OneBotAdapter {
    pub fn new(name: &str, action: Action, connection: Connection) -> Result<Self, IoError> {
        if name.is_empty() || name.contains(':') {
            return Err(IoError::new("invalid_port_manifest"));
        }
        Ok(Self {
            manifest: PortManifest {
                name: name.into(),
                kind: PortKind::Chat,
                transport: TransportKind::InProcess,
                core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
                host_permissions: vec![],
            },
            action,
            connection,
            sink: Mutex::new(None),
        })
    }

    /// Heartbeats are deliberately ignored; other non-message events lack a
    /// P0 `Incoming` representation and fail visibly on the opt-in path.
    pub fn map_event(&self, event: &Value, received_at: f64) -> Result<Option<Incoming>, IoError> {
        if event["post_type"] != "message" {
            return if event["post_type"] == "meta_event" { Ok(None) }
                else { Err(IoError::new("incoming_event_unimplemented")) };
        }
        let kind = match event["message_type"].as_str() {
            Some("group") => ChatKind::Group,
            Some("private") => ChatKind::Private,
            _ => return Err(IoError::new("unsupported_message_type")),
        };
        let id = wire_string(&event[if kind == ChatKind::Group { "group_id" } else { "user_id" }])
            .ok_or_else(|| IoError::new("missing_chat_id"))?;
        let message_id = wire_string(&event["message_id"])
            .ok_or_else(|| IoError::new("missing_message_id"))?;
        let self_id = wire_string(&event["self_id"])
            .ok_or_else(|| IoError::new("missing_self_id"))?;
        let sender_id = wire_string(&event["user_id"])
            .ok_or_else(|| IoError::new("missing_sender_id"))?;
        let ts = event["time"].as_f64().or_else(|| event["time"].as_str().and_then(|s| s.parse::<f64>().ok()))
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| IoError::new("invalid_server_time"))?;
        if !received_at.is_finite() || received_at <= 0.0 {
            return Err(IoError::new("invalid_received_at"));
        }
        let segments = match &event["message"] {
            Value::String(text) => parse_cq_text(text),
            Value::Array(parts) => parts.iter().map(map_segment).collect(),
            _ => return Err(IoError::new("invalid_message_segments")),
        };
        let sender = &event["sender"];
        Ok(Some(Incoming {
            port: self.manifest.name.clone(),
            chat: ChatAddress::new(kind, &self.manifest.name, id)?,
            message_id,
            self_id: self_id.clone(),
            ts,
            received_at,
            sender: Sender {
                id: sender_id.clone(),
                name: wire_string(&sender["card"])
                    .or_else(|| wire_string(&sender["nickname"]))
                    .unwrap_or_else(|| sender_id.clone()),
                card: wire_string(&sender["card"]),
                role: wire_string(&sender["role"]),
                title: wire_string(&sender["title"]),
                is_robot: sender["is_robot"].as_bool(),
            },
            segments,
            self_message: sender_id == self_id,
            raw: Some(event.clone()),
        }))
    }

    pub fn publish(&self, incoming: Incoming) -> Result<(), IoError> {
        let guard = self.sink.lock().map_err(|_| IoError::new("sink_poisoned"))?;
        let sink = guard.as_ref().ok_or_else(|| IoError::new("sink_not_attached"))?;
        sink.try_send(incoming).map_err(|_| IoError::new("incoming_sink_unavailable"))
    }

    async fn command(&self, command: Command) -> PortReply<CommandResult> {
        if command.port() != self.manifest.name {
            return failed("wrong_port", false);
        }
        let (connected, online) = (self.connection)();
        if matches!(command, Command::Health { .. }) {
            return PortReply::Ok(CommandResult::Health(HealthInfo { connected, online }));
        }
        if !connected || !online {
            return failed("qq_offline", false);
        }
        match command {
            Command::Send { chat, segments, reply_to } => {
                let id = match numeric_id(&chat.id) {
                    Some(id) => id,
                    None => return failed("invalid_chat", false),
                };
                let mut message = Vec::new();
                if let Some(reply) = reply_to {
                    if reply.is_empty() { return failed("invalid_reply", false); }
                    message.push(json!({"type":"reply","data":{"id":reply}}));
                }
                for segment in segments {
                    match segment {
                        Segment::Text(text) => message.push(json!({"type":"text","data":{"text":text}})),
                        Segment::Mention { id } if chat.kind == ChatKind::Group && numeric_id(&id).is_some() =>
                            message.push(json!({"type":"at","data":{"qq":id}})),
                        Segment::Face { id } if !id.is_empty() && id.len() <= 5 && id.bytes().all(|b| b.is_ascii_digit()) =>
                            message.push(json!({"type":"face","data":{"id":id}})),
                        _ => return failed("unsupported_send_segment", false),
                    }
                }
                if message.is_empty() { return failed("empty_message", false); }
                let (action, params) = match chat.kind {
                    ChatKind::Group => ("send_group_msg", json!({"group_id":id,"message":message,"auto_escape":true})),
                    ChatKind::Private => ("send_private_msg", json!({"user_id":id,"message":message,"auto_escape":true})),
                };
                match (self.action)(action.into(), params).await {
                    PortReply::Ok(data) => match wire_string(&data["message_id"]) {
                        Some(message_id) => PortReply::Ok(CommandResult::Sent { message_id }),
                        None => failed("missing_ack_message_id", true),
                    },
                    PortReply::Failed { code, uncertain } => PortReply::Failed { code, uncertain },
                    PortReply::Unsupported { capability } => PortReply::Unsupported { capability },
                    PortReply::Degraded { .. } => failed("degraded_send_ack", true),
                }
            }
            Command::Recall { message_id, .. } => {
                if message_id.is_empty() { return failed("invalid_message_id", false); }
                match (self.action)("delete_msg".into(), json!({"message_id":message_id})).await {
                    PortReply::Ok(_) => PortReply::Ok(CommandResult::Ok),
                    PortReply::Failed { code, uncertain } => PortReply::Failed { code, uncertain },
                    PortReply::Unsupported { capability } => PortReply::Unsupported { capability },
                    PortReply::Degraded { .. } => failed("degraded_recall_ack", true),
                }
            }
            _ => failed("onebot_command_unimplemented", false),
        }
    }
}

impl ChatPort for OneBotAdapter {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> Capabilities {
        Capabilities { mention: true, reply: true, ..Capabilities::default() }
    }
    fn state(&self) -> PortState {
        let (connected, online) = (self.connection)();
        if !connected { PortState::Disconnected } else if !online { PortState::Degraded } else { PortState::Available }
    }
    fn deliver<'a>(&'a self, command: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        Box::pin(self.command(command))
    }
    fn attach(&self, sink: mpsc::Sender<Incoming>) -> Result<(), IoError> {
        let mut guard = self.sink.lock().map_err(|_| IoError::new("sink_poisoned"))?;
        if guard.is_some() { return Err(IoError::new("sink_already_attached")); }
        *guard = Some(sink);
        Ok(())
    }
}

fn wire_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn numeric_id(value: &str) -> Option<u64> {
    if !value.starts_with(|c: char| ('1'..='9').contains(&c)) || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok()
}

fn map_segment(segment: &Value) -> Segment {
    let data = &segment["data"];
    match segment["type"].as_str() {
        Some("text") => data["text"].as_str().map(|s| Segment::Text(s.into())),
        Some("at") => wire_string(&data["qq"]).map(|id| Segment::Mention { id }),
        Some("reply") => wire_string(&data["id"]).map(|id| Segment::Reply { id }),
        Some("face") => wire_string(&data["id"]).map(|id| Segment::Face { id }),
        Some("image") => data["url"].as_str().or_else(|| data["file"].as_str())
            .map(|s| Segment::Image { src: MediaSrc::Opaque(s.into()), summary: None }),
        Some("forward") => Some(Segment::Forward { id: wire_string(&data["id"]), nodes: vec![] }),
        _ => None,
    }.unwrap_or_else(|| Segment::Unknown { kind: segment["type"].as_str().unwrap_or("attachment").into() })
}

fn parse_cq_text(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[CQ:") {
        if start > 0 {
            segments.push(Segment::Text(decode_cq(&rest[..start])));
        }
        rest = &rest[start..];
        let Some(end) = rest.find(']') else {
            segments.push(Segment::Text(decode_cq(rest)));
            return segments;
        };
        let body = &rest[4..end];
        let kind = body.split(',').next().unwrap_or("");
        let data = body.split(',').skip(1).find_map(|part| part.split_once('='));
        let segment = match (kind, data) {
            ("at", Some(("qq", id))) => Segment::Mention { id: id.into() },
            ("face", Some(("id", id))) => Segment::Face { id: id.into() },
            ("reply", Some(("id", id))) => Segment::Reply { id: id.into() },
            _ => Segment::Unknown { kind: kind.into() },
        };
        segments.push(segment);
        rest = &rest[end + 1..];
    }
    if !rest.is_empty() { segments.push(Segment::Text(decode_cq(rest))); }
    segments
}

fn decode_cq(value: &str) -> String {
    value.replace("&#44;", ",").replace("&#91;", "[").replace("&#93;", "]").replace("&amp;", "&")
}

fn failed<T>(code: &str, uncertain: bool) -> PortReply<T> {
    PortReply::Failed { code: code.into(), uncertain }
}
