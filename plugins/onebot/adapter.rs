//! OneBot v11 boundary. The wire callback keeps this module independent of
//! the legacy transport implementation while preserving its ACK/error state.

use super::{ports::ChatPort, types::*};
use super::connect::{ConnectConfig, Notification, OneBot};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::{sync::{mpsc, watch}, task::JoinHandle};

pub type Action = Arc<dyn Fn(String, Value) -> BoxFuture<'static, PortReply<Value>> + Send + Sync>;
pub type Connection = Arc<dyn Fn() -> (bool, bool, Option<String>) + Send + Sync>;

enum Backend {
    Hosted { action: Action, connection: Connection },
    Owned {
        bot: OneBot,
        notices: Mutex<Option<mpsc::UnboundedReceiver<Notification>>>,
        shutdown: watch::Sender<bool>,
        connection_task: JoinHandle<()>,
        forward_task: Mutex<Option<JoinHandle<()>>>,
    },
}

pub struct OneBotAdapter {
    manifest: PortManifest,
    backend: Backend,
    sink: Mutex<Option<mpsc::Sender<PortEvent>>>,
    self_ref: OnceLock<Weak<Self>>,
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
                dependencies: vec![],
                config_fields: vec![],
            },
            backend: Backend::Hosted { action, connection },
            sink: Mutex::new(None),
            self_ref: OnceLock::new(),
        })
    }

    pub fn connect<C: Into<ConnectConfig>>(name: &str, config: C, token: String) -> Result<Arc<Self>, IoError> {
        if name.is_empty() || name.contains(':') {
            return Err(IoError::new("invalid_port_manifest"));
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| IoError::new("runtime_unavailable"))?;
        let (bot, notices) = OneBot::new(config, token);
        let (shutdown, signal) = watch::channel(false);
        let running = bot.clone();
        let connection_task = runtime.spawn(async move { running.run(signal).await });
        let adapter = Arc::new(Self {
            manifest: PortManifest {
                name: name.into(), kind: PortKind::Chat, transport: TransportKind::InProcess,
                core_api_version: VersionReq { major: 1, min_minor: 0, max_minor: None },
                host_permissions: vec![], dependencies: vec![], config_fields: vec![],
            },
            backend: Backend::Owned { bot, notices: Mutex::new(Some(notices)), shutdown,
                connection_task, forward_task: Mutex::new(None) },
            sink: Mutex::new(None),
            self_ref: OnceLock::new(),
        });
        let _ = adapter.self_ref.set(Arc::downgrade(&adapter));
        Ok(adapter)
    }

    async fn action(&self, action: String, params: Value) -> PortReply<Value> {
        match &self.backend {
            Backend::Hosted { action: callback, .. } => callback(action, params).await,
            Backend::Owned { bot, .. } => match bot.call(&action, params).await {
                Ok(data) => PortReply::Ok(data),
                Err(error) => PortReply::Failed { code: error.code, uncertain: error.uncertain },
            },
        }
    }

    /// Heartbeats are deliberately ignored; notices retain their own typed shape.
    pub fn map_event(&self, event: &Value, received_at: f64) -> Result<Option<PortEvent>, IoError> {
        if event["post_type"] != "message" && event["post_type"] != "message_sent" {
            if event["post_type"] == "meta_event" { return Ok(None); }
            if event["post_type"] != "notice" { return Err(IoError::new("incoming_event_unimplemented")); }
            let kind = event["notice_type"].as_str().filter(|kind| !kind.is_empty())
                .ok_or_else(|| IoError::new("incoming_event_unimplemented"))?;
            let at = event["time"].as_f64()
                .or_else(|| event["time"].as_str().and_then(|s| s.parse::<f64>().ok()))
                .filter(|ts| ts.is_finite() && *ts > 0.)
                .ok_or_else(|| IoError::new("invalid_server_time"))?;
            let chat = wire_string(&event["group_id"])
                .map(|id| ChatAddress::new(ChatKind::Group, &self.manifest.name, id))
                .transpose()?;
            return Ok(Some(PortEvent::Notice(Notice {
                kind: if kind == "group_increase" { NoticeKind::GroupJoined }
                    else { NoticeKind::Unknown(kind.to_owned()) },
                chat,
                actor: wire_string(&event["user_id"]),
                self_id: wire_string(&event["self_id"]),
                at,
            })));
        }
        let kind = match event["message_type"].as_str().or_else(|| {
            (event["post_type"] == "message_sent").then(|| {
                if !event["group_id"].is_null() { "group" } else { "private" }
            })
        }) {
            Some("group") => ChatKind::Group,
            Some("private") => ChatKind::Private,
            _ => return Err(IoError::new("unsupported_message_type")),
        };
        let id = wire_string(&event[if kind == ChatKind::Group { "group_id" } else if event["post_type"] == "message_sent" { "target_id" } else { "user_id" }])
            .ok_or_else(|| IoError::new("missing_chat_id"))?;
        let message_id = wire_string(&event["message_id"])
            .ok_or_else(|| IoError::new("missing_message_id"))?;
        let self_id = wire_string(&event["self_id"])
            .ok_or_else(|| IoError::new("missing_self_id"))?;
        let sender_id = wire_string(&event["user_id"])
            .ok_or_else(|| IoError::new("missing_sender_id"))?;
        if event["post_type"] == "message_sent" && sender_id != self_id {
            return Err(IoError::new("invalid_self_echo_sender"));
        }
        let ts = event["time"].as_f64().or_else(|| event["time"].as_str().and_then(|s| s.parse::<f64>().ok()))
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| IoError::new("invalid_server_time"))?;
        if !received_at.is_finite() || received_at <= 0.0 {
            return Err(IoError::new("invalid_received_at"));
        }
        let payload = if event["post_type"] == "message_sent" && event["message"].is_null() {
            &event["raw_message"]
        } else { &event["message"] };
        let segments = match payload {
            Value::String(text) => parse_cq_text(text),
            Value::Array(parts) => parts.iter().map(map_segment).collect(),
            _ => return Err(IoError::new("invalid_message_segments")),
        };
        let sender = &event["sender"];
        Ok(Some(PortEvent::Incoming(Incoming {
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
        })))
    }

    pub fn publish(&self, event: PortEvent) -> Result<(), IoError> {
        let guard = self.sink.lock().map_err(|_| IoError::new("sink_poisoned"))?;
        let sink = guard.as_ref().ok_or_else(|| IoError::new("sink_not_attached"))?;
        sink.try_send(event).map_err(|_| IoError::new("incoming_sink_unavailable"))
    }

    async fn command(&self, command: Command) -> PortReply<CommandResult> {
        if command.port() != self.manifest.name {
            return failed("wrong_port", false);
        }
        let state = self.state();
        let (connected, online) = (state.connected, state.online);
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
                        Segment::Image { src, summary } => {
                            let data = match image_data(&src, summary.as_deref()) {
                                Ok(data) => data,
                                Err(code) => return failed(code, false),
                            };
                            message.push(json!({"type":"image","data":data}));
                        }
                        _ => return failed("unsupported_send_segment", false),
                    }
                }
                if message.is_empty() { return failed("empty_message", false); }
                let (action, params) = match chat.kind {
                    ChatKind::Group => ("send_group_msg", json!({"group_id":id,"message":message,"auto_escape":true})),
                    ChatKind::Private => ("send_private_msg", json!({"user_id":id,"message":message,"auto_escape":true})),
                };
                match self.action(action.into(), params).await {
                    PortReply::Ok(data) => match wire_string(&data["message_id"]) {
                        Some(message_id) => PortReply::Ok(CommandResult::Sent { message_id }),
                        None => failed("missing_ack_message_id", true),
                    },
                    PortReply::Failed { code, uncertain } => PortReply::Failed { code, uncertain },
                    PortReply::Unsupported { capability } => PortReply::Unsupported { capability },
                    PortReply::Degraded { .. } => failed("degraded_send_ack", true),
                }
            }
            Command::SendMedia { chat, media } => {
                if media.kind != "image" {
                    return PortReply::Unsupported { capability: Capability::Custom(media.kind) };
                }
                let id = match numeric_id(&chat.id) {
                    Some(id) => id,
                    None => return failed("invalid_chat", false),
                };
                let data = match image_data(&media.source, None) {
                    Ok(data) => data,
                    Err(code) => return failed(code, false),
                };
                let message = vec![json!({"type":"image","data":data})];
                let (action, params) = match chat.kind {
                    ChatKind::Group => ("send_group_msg", json!({"group_id":id,"message":message,"auto_escape":true})),
                    ChatKind::Private => ("send_private_msg", json!({"user_id":id,"message":message,"auto_escape":true})),
                };
                match self.action(action.into(), params).await {
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
                match self.action("delete_msg".into(), json!({"message_id":message_id})).await {
                    PortReply::Ok(_) => PortReply::Ok(CommandResult::Ok),
                    PortReply::Failed { code, uncertain } => PortReply::Failed { code, uncertain },
                    PortReply::Unsupported { capability } => PortReply::Unsupported { capability },
                    PortReply::Degraded { .. } => failed("degraded_recall_ack", true),
                }
            }
            _ => PortReply::Unsupported { capability: Capability::Custom("onebot_command_unimplemented".into()) },
        }
    }
}

impl Drop for OneBotAdapter {
    fn drop(&mut self) {
        if let Backend::Owned { shutdown, connection_task, forward_task, .. } = &self.backend {
            shutdown.send_replace(true);
            connection_task.abort();
            if let Ok(mut task) = forward_task.lock() {
                if let Some(task) = task.take() { task.abort(); }
            }
        }
    }
}

/// QQ-specific secondary evidence. A readback has no post_type; an event must
/// explicitly be message_sent. Missing fields are never inferred from ACKs.
pub(crate) fn outbound_evidence(event: &Value, self_id: &str, readback: bool) -> Option<OutboundEvidence> {
    if (!readback && event["post_type"] != "message_sent")
        || (readback && !event["post_type"].is_null()) {
        return None;
    }
    if !readback && wire_string(&event["self_id"])? != self_id { return None; }
    let sender = wire_string(&event["sender"]["user_id"])
        .or_else(|| wire_string(&event["user_id"]))?;
    if sender != self_id { return None; }
    let kind = event["message_type"].as_str().unwrap_or_else(|| {
        if event["group_id"].is_null() { "private" } else { "group" }
    });
    let chat = match kind {
        "group" => format!("group:{}", wire_string(&event["group_id"])?),
        "private" => format!("private:{}", wire_string(&event["target_id"])?),
        _ => return None,
    };
    let message_id = wire_string(&event["message_id"])?;
    let text = event["raw_message"].as_str()
        .or_else(|| event["message"].as_str())?.to_owned();
    let server_ts = event["time"].as_f64()
        .or_else(|| event["time"].as_str().and_then(|s| s.parse().ok()))
        .filter(|ts| ts.is_finite() && *ts > 0. && *ts < 100_000_000_000.)?;
    Some(OutboundEvidence { chat, message_id, text, server_ts })
}

pub(crate) struct OutboundEvidence {
    pub chat: String,
    pub message_id: String,
    pub text: String,
    pub server_ts: f64,
}

impl ChatPort for OneBotAdapter {
    fn manifest(&self) -> &PortManifest { &self.manifest }
    fn capabilities(&self) -> Capabilities {
        Capabilities { mention: true, reply: true, media: true, notices: true, ..Capabilities::default() }
    }
    fn state(&self) -> PortState {
        let (connected, online, self_id) = match &self.backend {
            Backend::Hosted { connection, .. } => connection(),
            Backend::Owned { bot, .. } => {
                let state = bot.state();
                (state.connected, state.online, Some(state.self_id))
            }
        };
        PortState { connected, online, self_id: self_id.filter(|id| !id.is_empty()),
            degraded: (connected && !online).then(|| "qq_offline".to_owned()) }
    }
    fn deliver<'a>(&'a self, command: Command) -> BoxFuture<'a, PortReply<CommandResult>> {
        Box::pin(self.command(command))
    }
    fn attach(&self, sink: mpsc::Sender<PortEvent>) -> Result<(), IoError> {
        let mut guard = self.sink.lock().map_err(|_| IoError::new("sink_poisoned"))?;
        if guard.is_some() { return Err(IoError::new("sink_already_attached")); }
        let owned = match &self.backend {
            Backend::Hosted { .. } => None,
            Backend::Owned { notices, .. } => {
                let runtime = tokio::runtime::Handle::try_current().map_err(|_| IoError::new("runtime_unavailable"))?;
                let receiver = notices.lock().map_err(|_| IoError::new("notice_receiver_poisoned"))?
                    .take().ok_or_else(|| IoError::new("notice_receiver_unavailable"))?;
                let weak = self.self_ref.get().cloned().ok_or_else(|| IoError::new("adapter_unavailable"))?;
                Some((runtime, receiver, weak))
            }
        };
        *guard = Some(sink);
        drop(guard);
        if let Some((runtime, mut notices, weak)) = owned {
            let task = runtime.spawn(async move {
                while let Some(notification) = notices.recv().await {
                    let Notification::Event(frame) = notification else { continue; };
                    let Some(adapter) = weak.upgrade() else { break; };
                    let received_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH).map_or(0., |time| time.as_secs_f64());
                    match adapter.map_event(&frame, received_at) {
                        Ok(Some(event)) => { let _ = adapter.publish(event); }
                        Ok(None) => {}
                        Err(error) => eprintln!("onebot_event_rejected code={}", error.code),
                    }
                }
            });
            if let Backend::Owned { forward_task, .. } = &self.backend {
                *forward_task.lock().map_err(|_| IoError::new("forward_task_poisoned"))? = Some(task);
            }
        }
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

fn image_data(source: &MediaSrc, summary: Option<&str>) -> Result<Value, &'static str> {
    let file = match source {
        MediaSrc::Url(value) => {
            let url = url::Url::parse(value).map_err(|_| "invalid_media")?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
                || !url.username().is_empty() || url.password().is_some() {
                return Err("invalid_media");
            }
            value.clone()
        }
        MediaSrc::LocalPath(value) => url::Url::from_file_path(value)
            .map_err(|_| "invalid_media")?.to_string(),
        MediaSrc::Opaque(value) if !value.trim().is_empty() => value.clone(),
        MediaSrc::Opaque(_) => return Err("invalid_media"),
    };
    let mut data = json!({"file":file});
    if let Some(summary) = summary.filter(|text| !text.trim().is_empty()) {
        data["summary"] = json!(summary);
    }
    Ok(data)
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
