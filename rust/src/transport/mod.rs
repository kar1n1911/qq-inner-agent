//! OneBot v11 正向连接。通知采用单消费者 mpsc，状态与事件共用队列以保持顺序。
pub mod provider;
pub mod provider_transport;

use crate::config::{js_string, truthy, Onebot};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{sleep, timeout, Instant},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
};

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{code}")]
pub struct OneBotError {
    pub code: String,
    /// 不确定表示请求可能已经执行；上层绝不能因此自动重发消息。
    pub uncertain: bool,
}
impl OneBotError {
    fn new(code: impl Into<String>, uncertain: bool) -> Self {
        Self {
            code: code.into(),
            uncertain,
        }
    }
}
type Reply = Result<Value, OneBotError>;
#[derive(Debug, Clone, Default)]
pub struct State {
    pub connected: bool,
    pub online: bool,
    pub self_id: String,
    pub reconnects: u64,
}
#[derive(Debug, Clone)]
pub enum Notification {
    Status(String),
    Event(Value),
}
struct Shared {
    state: State,
    outgoing: Option<mpsc::UnboundedSender<Message>>,
    pending: HashMap<String, oneshot::Sender<Reply>>,
    early: Vec<Value>,
}
#[derive(Clone)]
pub struct OneBot {
    config: Onebot,
    token: String,
    shared: Arc<Mutex<Shared>>,
    notices: mpsc::UnboundedSender<Notification>,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
}
// future 被取消也要移除 echo；锁内删除决定响应/超时竞争的唯一赢家。
struct PendingGuard {
    shared: Arc<Mutex<Shared>>,
    echo: String,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.shared.lock().unwrap().pending.remove(&self.echo);
    }
}
struct SessionGuard(OneBot);
impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.disconnect();
    }
}
impl OneBot {
    pub fn new(config: Onebot, token: String) -> (Self, mpsc::UnboundedReceiver<Notification>) {
        let (notices, rx) = mpsc::unbounded_channel();
        let state = State {
            self_id: if config.self_id.is_truthy {
                config.self_id.text.clone()
            } else {
                String::new()
            },
            ..State::default()
        };
        (
            Self {
                config,
                token,
                shared: Arc::new(Mutex::new(Shared {
                    state,
                    outgoing: None,
                    pending: HashMap::new(),
                    early: Vec::new(),
                })),
                notices,
                lifecycle: Arc::new(tokio::sync::Mutex::new(())),
            },
            rx,
        )
    }
    pub fn state(&self) -> State {
        self.shared.lock().unwrap().state.clone()
    }
    fn status(&self, code: &str) {
        let _ = self.notices.send(Notification::Status(code.into()));
    }
    fn disconnect(&self) {
        let mut s = self.shared.lock().unwrap();
        s.state.connected = false;
        s.state.online = false;
        s.outgoing = None;
        s.early.clear();
        for (_, p) in s.pending.drain() {
            let _ = p.send(Err(OneBotError::new("connection_lost", true)));
        }
    }
    pub async fn call(&self, action: &str, params: Value) -> Reply {
        let echo = uuid();
        let (tx, mut rx) = oneshot::channel();
        {
            let mut s = self.shared.lock().unwrap();
            let Some(out) = s.outgoing.clone() else {
                return Err(OneBotError::new("not_connected", false));
            };
            s.pending.insert(echo.clone(), tx);
            if out
                .send(Message::Text(
                    json!({"action":action,"params":params,"echo":echo}).to_string(),
                ))
                .is_err()
            {
                s.pending.remove(&echo);
                return Err(OneBotError::new("send_failed", true));
            }
        }
        let _guard = PendingGuard {
            shared: self.shared.clone(),
            echo: echo.clone(),
        };
        match timeout(self.request_timeout(), &mut rx).await {
            Ok(result) => result.unwrap_or_else(|_| Err(OneBotError::new("connection_lost", true))),
            Err(_) => {
                let removed = self.shared.lock().unwrap().pending.remove(&echo).is_some();
                if removed {
                    Err(OneBotError::new("action_timeout", true))
                } else {
                    rx.await
                        .unwrap_or_else(|_| Err(OneBotError::new("connection_lost", true)))
                }
            }
        }
    }
    /// 高风险身份操作；调用方负责身份护栏、冷却及原值备份。
    pub async fn set_group_card(&self, group_id: &str, card: &str) -> Reply {
        self.call(
            "set_group_card",
            json!({"group_id":group_id,"user_id":self.state().self_id,"card":card}),
        )
        .await
    }
    pub async fn set_qq_profile(&self, nickname: &str) -> Reply {
        self.call("set_qq_profile", json!({"nickname":nickname}))
            .await
    }
    pub async fn set_qq_avatar(&self, file: &str) -> Reply {
        self.call("set_qq_avatar", json!({"file":file})).await
    }
    /// NapCat 参数大小写为 longNick；与 get_stranger_info 的 long_nick 不同。
    pub async fn set_signature(&self, text: &str) -> Reply {
        self.call("set_self_longnick", json!({"longNick":text}))
            .await
    }
    pub async fn send(&self, chat: &str, text: &str, face_id: Option<&str>) -> Reply {
        self.send_targeted(chat, text, face_id, None, None).await
    }
    pub async fn send_targeted(
        &self,
        chat: &str,
        text: &str,
        face_id: Option<&str>,
        reply_to: Option<&str>,
        mention: Option<&str>,
    ) -> Reply {
        let s = self.state();
        if !s.connected || !s.online {
            return Err(OneBotError::new("qq_offline", false));
        }
        let (kind, id) = chat.split_once(':').unwrap_or(("", ""));
        if !matches!(kind, "group" | "private")
            || !id.starts_with(|c: char| ('1'..='9').contains(&c))
            || !id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(OneBotError::new("invalid_chat", false));
        }
        if face_id
            .is_some_and(|s| s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(OneBotError::new("invalid_face", false));
        }
        let message = message_segments(chat, text, face_id, reply_to, mention);
        // 数组文本段与 auto_escape 同时保留，模型生成的 CQ 码只能作为惰性文本。
        let mut params = json!({"message":message,"auto_escape":true});
        params[if kind == "group" {
            "group_id"
        } else {
            "user_id"
        }] = json!(id.parse::<f64>().unwrap_or(f64::INFINITY));
        self.call(
            if kind == "group" {
                "send_group_msg"
            } else {
                "send_private_msg"
            },
            params,
        )
        .await
    }
    /// 素材专用发送：禁止 text/at 等段，原始上下文没有可进入输出的字段。
    pub async fn send_media(&self, chat: &str, segment: Value) -> Reply {
        let state = self.state();
        if !state.connected || !state.online {
            return Err(OneBotError::new("qq_offline", false));
        }
        let Some(id) = chat
            .strip_prefix("group:")
            .and_then(|id| id.parse::<u64>().ok())
            .filter(|id| *id > 0)
        else {
            return Err(OneBotError::new("invalid_chat", false));
        };
        let valid = match segment["type"].as_str() {
            Some("face") => segment["data"]["id"].as_str().is_some_and(|id| {
                !id.is_empty() && id.len() <= 5 && id.bytes().all(|b| b.is_ascii_digit())
            }),
            Some("image") => segment["data"]["file"]
                .as_str()
                .is_some_and(|file| file.starts_with("file://")),
            _ => false,
        };
        if !valid {
            return Err(OneBotError::new("invalid_media_segment", false));
        }
        // 重建白名单字段；即使调用者附带昵称或旧正文也不会透传。
        let data = if segment["type"] == "face" {
            json!({"id":segment["data"]["id"]})
        } else {
            json!({"file":segment["data"]["file"]})
        };
        self.call("send_group_msg",json!({"group_id":id,"message":[{"type":segment["type"],"data":data}],"auto_escape":true})).await
    }
    pub fn forward_enabled(&self) -> bool {
        self.config.forward_enabled
    }
    /// 安全闸门比“感兴趣”更靠前：仅允许已有消息引用，禁止构造正文或 file 段。
    pub async fn send_forward(&self, chat: &str, nodes: Vec<Value>) -> Reply {
        if !self.forward_enabled() {
            return Err(OneBotError::new("forward_disabled", false));
        }
        if nodes.is_empty()
            || nodes.iter().any(|node| {
                let Some(fields) = node.as_object() else {
                    return true;
                };
                fields
                    .keys()
                    .any(|key| !matches!(key.as_str(), "user_id" | "uin" | "id" | "message_id"))
                    || !["user_id", "uin"]
                        .iter()
                        .any(|key| fields.contains_key(*key))
                    || !["id", "message_id"]
                        .iter()
                        .any(|key| fields.contains_key(*key))
                    || fields.iter().any(|(key, value)| {
                        !reference_id(value, matches!(key.as_str(), "user_id" | "uin"))
                    })
            })
        {
            return Err(OneBotError::new("invalid_forward_nodes", false));
        }
        let (kind, id) = chat.split_once(':').unwrap_or(("", ""));
        if !matches!(kind, "group" | "private") || !reference_id(&json!(id), true) {
            return Err(OneBotError::new("invalid_chat", false));
        }
        let state = self.state();
        if !state.connected || !state.online {
            return Err(OneBotError::new("qq_offline", false));
        }
        let mut params =
            json!({"message":[{"type":"forward","data":{"nodes":nodes}}],"auto_escape":true});
        params[if kind == "group" {
            "group_id"
        } else {
            "user_id"
        }] = json!(id.parse::<u64>().unwrap());
        self.call(
            if kind == "group" {
                "send_group_msg"
            } else {
                "send_private_msg"
            },
            params,
        )
        .await
    }
    /// forwardId 为桥返回的不透明标识，不套用消息整数 ID 的规则。
    pub async fn get_forward_msg(&self, id: &str) -> Reply {
        if !self.forward_enabled() {
            return Err(OneBotError::new("forward_disabled", false));
        }
        if id.trim().is_empty() {
            return Err(OneBotError::new("invalid_forward_id", false));
        }
        self.call("get_forward_msg", json!({"id":id})).await
    }
    fn request_timeout(&self) -> Duration {
        Duration::from_secs_f64(self.config.request_timeout_seconds)
    }
    fn receive(&self, frame: Message) {
        let Message::Text(text) = frame else {
            return;
        };
        // JS length 计 UTF-16 单元，不是 UTF-8 字节，也不是 Unicode 码点。
        if text.encode_utf16().count() > 1_000_000 {
            return;
        }
        let Ok(data) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if !data.is_object() && !data.is_array() {
            return;
        }
        let mut s = self.shared.lock().unwrap();
        if let Some(p) = data
            .get("echo")
            .filter(|v| !v.is_null())
            .and_then(|v| s.pending.remove(&js_string(v)))
        {
            let retcode = data.get("retcode").map(js_number).unwrap_or(f64::NAN);
            let result = if data["status"] == "ok" && retcode == 0.0 {
                Ok(data["data"].clone())
            } else {
                Err(OneBotError::new(
                    format!(
                        "onebot_action_failed_{}",
                        if retcode == 0.0 || retcode.is_nan() {
                            "unknown".into()
                        } else {
                            retcode.to_string()
                        }
                    ),
                    false,
                ))
            };
            let _ = p.send(result);
        } else if truthy(&data["post_type"]) {
            if s.state.connected {
                let _ = self.notices.send(Notification::Event(data));
            } else if s.early.len() < 200 {
                s.early.push(data);
            }
        }
    }
    async fn handshake(&self) -> Result<(), OneBotError> {
        let login = self.call("get_login_info", json!({})).await?;
        if !truthy(&login["user_id"]) {
            return Err(OneBotError::new("missing_account", false));
        }
        let id = js_string(&login["user_id"]);
        if self.config.self_id.is_truthy && id != self.config.self_id.text {
            return Err(OneBotError::new("wrong_qq_account", false));
        }
        self.shared.lock().unwrap().state.self_id = id;
        let status = self.call("get_status", json!({})).await?;
        let mut s = self.shared.lock().unwrap();
        s.state.online = status["online"] == true;
        s.state.connected = true;
        self.status(if s.state.online {
            "connected"
        } else {
            "qq_offline"
        });
        // 置位、状态通知和 early 补发在同一锁内完成，后来的事件不能插队。
        for event in s.early.drain(..) {
            let _ = self.notices.send(Notification::Event(event));
        }
        Ok(())
    }
    async fn session(&self, check_only: bool) -> Result<State, OneBotError> {
        let _cleanup = SessionGuard(self.clone());
        let parsed = ureq::get(&self.config.url)
            .request_url()
            .map_err(|_| OneBotError::new("connection_failed", false))?;
        let mut url = parsed.as_url().clone();
        if !self.token.is_empty() {
            let pairs: Vec<_> = url
                .query_pairs()
                .filter(|(key, _)| key != "access_token")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            url.query_pairs_mut()
                .clear()
                .extend_pairs(pairs)
                .append_pair("access_token", &self.token);
        }
        // 由应用按 JS 字符数忽略超限消息，不能让库的字节上限提前断开连接。
        let ws_config = WebSocketConfig {
            max_message_size: None,
            max_frame_size: None,
            ..WebSocketConfig::default()
        };
        let socket = timeout(
            self.request_timeout(),
            connect_async_with_config(url.as_str(), Some(ws_config), false),
        )
        .await;
        let (mut ws, _) = match socket {
            Err(_) => return Err(OneBotError::new("connect_timeout", false)),
            Ok(Err(_)) => {
                self.status("websocket_error");
                return Err(OneBotError::new("connect_closed", false));
            }
            Ok(Ok(ws)) => ws,
        };
        let (tx, mut outgoing) = mpsc::unbounded_channel();
        self.shared.lock().unwrap().outgoing = Some(tx);
        let io = async {
            loop {
                tokio::select! {
                    frame = ws.next() => match frame {
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(frame)) => self.receive(frame),
                        Some(Err(_)) => { self.status("websocket_error"); break; }
                    },
                    Some(packet) = outgoing.recv() => {
                        if !matches!(timeout(self.request_timeout(), ws.send(packet.clone())).await, Ok(Ok(()))) {
                            // 不打印底层错误；其中可能含带 token 的 URL。
                            if let Message::Text(text) = packet {
                                if let Ok(data) = serde_json::from_str::<Value>(&text) {
                                    if let Some(p) = self.shared.lock().unwrap().pending.remove(&js_string(&data["echo"])) { let _ = p.send(Err(OneBotError::new("send_failed", true))); }
                                }
                            }
                            self.status("websocket_error"); break;
                        }
                    }
                }
            }
            if self.state().connected {
                Ok(self.state())
            } else {
                Err(OneBotError::new("connection_lost", true))
            }
        };
        let protocol = async {
            self.handshake().await?;
            if check_only {
                return Ok(self.state());
            }
            let period = Duration::from_secs_f64(self.config.heartbeat_seconds);
            let mut ticks = tokio::time::interval_at(Instant::now() + period, period);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut checking = false;
            let mut heartbeat = Box::pin(std::future::pending::<Reply>())
                as std::pin::Pin<Box<dyn std::future::Future<Output = Reply> + Send>>;
            loop {
                tokio::select! {
                    _ = ticks.tick() => if !checking {
                        checking = true;
                        heartbeat = Box::pin(self.call("get_status", json!({})));
                    },
                    result = &mut heartbeat, if checking => {
                        checking = false;
                        match result {
                            Ok(s) => self.shared.lock().unwrap().state.online = s["online"] == true,
                            Err(_) => { self.shared.lock().unwrap().state.online = false; return Ok(self.state()); }
                        }
                    }
                }
            }
        };
        let result = tokio::select! { result = io => result, result = protocol => result };
        self.disconnect();
        // 限时关闭，避免不响应的桥阻止停机或重连。
        let _ = timeout(self.request_timeout(), ws.close(None)).await;
        result
    }
    /// 单次校验，不重连；返回关闭前的鉴权/在线快照。
    pub async fn check(&self) -> Result<State, OneBotError> {
        let _lock = self.lifecycle.lock().await;
        self.session(true).await
    }
    /// shutdown 为 true 或发送端被释放时退出；调用者应等待此 future 完成。
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        let _lock = self.lifecycle.lock().await;
        let mut delay = 1.0;
        while !*shutdown.borrow() {
            let started = Instant::now();
            tokio::select! {
                _ = shutdown.changed() => break,
                result = self.session(false) => if let Err(e) = result { self.status(&e.code); }
            }
            self.disconnect();
            if *shutdown.borrow() {
                break;
            }
            self.shared.lock().unwrap().state.reconnects += 1;
            // JS 从会话尝试开始计时；严格超过 30 秒才重置，恰好 30 秒不重置。
            let wait = backoff(
                &mut delay,
                started.elapsed(),
                self.config.reconnect_max_seconds,
                rand::random(),
            );
            tokio::select! { _ = shutdown.changed() => break, _ = sleep(wait) => {} }
        }
        self.disconnect();
    }
}
// OneBot 消息 ID 可为负数；QQ 用户 ID 必须为正整数。拒绝零、浮点数和非数字内容。
fn reference_id(value: &Value, user: bool) -> bool {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return false,
    };
    let digits = if user {
        text.as_str()
    } else {
        text.strip_prefix('-').unwrap_or(&text)
    };
    !digits.starts_with('0')
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && if user {
            text.parse::<u64>().is_ok()
        } else {
            text.parse::<i64>().is_ok()
        }
}
fn backoff(delay: &mut f64, elapsed: Duration, max: f64, jitter: f64) -> Duration {
    if elapsed > Duration::from_secs(30) {
        *delay = 1.0;
    }
    let wait = Duration::from_secs_f64(*delay + jitter);
    *delay = (*delay * 2.0).min(max);
    wait
}
fn uuid() -> String {
    // 复用 rand 生成 RFC 4122 UUID v4，不增加 uuid 依赖。
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    let hex: String = b.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
pub(crate) fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        _ => {
            let s = js_string(v);
            let s = s.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'));
            if s.is_empty() {
                return 0.0;
            }
            for (lower, upper, radix) in [("0x", "0X", 16), ("0b", "0B", 2), ("0o", "0O", 8)] {
                if let Some(digits) = s.strip_prefix(lower).or_else(|| s.strip_prefix(upper)) {
                    if digits.is_empty() {
                        return f64::NAN;
                    }
                    return digits
                        .chars()
                        .try_fold(0.0, |value, c| {
                            c.to_digit(radix).map(|d| value * radix as f64 + d as f64)
                        })
                        .unwrap_or(f64::NAN);
                }
            }
            match s {
                "Infinity" | "+Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ if s
                    .bytes()
                    .any(|c| c.is_ascii_alphabetic() && c != b'e' && c != b'E') =>
                {
                    f64::NAN
                }
                _ => s.parse().unwrap_or(f64::NAN),
            }
        }
    }
}

/// Typed segments only: model text (including CQ codes) stays inert.
pub fn message_segments(
    chat: &str,
    text: &str,
    face: Option<&str>,
    reply_to: Option<&str>,
    mention: Option<&str>,
) -> Vec<Value> {
    let mut parts = Vec::new();
    if let Some(id) = reply_to {
        parts.push(json!({"type":"reply","data":{"id":id}}));
    }
    if let Some(qq) = mention.filter(|_| chat.starts_with("group:")) {
        parts.push(json!({"type":"at","data":{"qq":qq}}));
    }
    if !text.is_empty() || face.is_none() {
        parts.push(json!({"type":"text","data":{"text":text}}));
    }
    if let Some(id) = face {
        parts.push(json!({"type":"face","data":{"id":id}}));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backoff_cap_jitter_and_strict_reset() {
        let mut delay = 1.0;
        for expected in [1., 2., 4., 8., 16., 32., 60., 60.] {
            assert_eq!(
                backoff(&mut delay, Duration::from_secs(30), 60., 0.5),
                Duration::from_secs_f64(expected + 0.5)
            );
        }
        assert_eq!(
            backoff(&mut delay, Duration::from_millis(30_001), 60., 0.),
            Duration::from_secs(1)
        );
        assert_eq!(delay, 2.);
    }
    #[tokio::test]
    async fn send_failure_and_cancellation_clean_pending() {
        let c = serde_json::from_value(crate::config::defaults()["onebot"].clone()).unwrap();
        let (bot, _) = OneBot::new(c, String::new());
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        bot.shared.lock().unwrap().outgoing = Some(tx);
        let e = bot.call("test", json!({})).await.unwrap_err();
        assert_eq!(e.code, "send_failed");
        assert!(e.uncertain);
        assert!(bot.shared.lock().unwrap().pending.is_empty());
        let (tx, mut rx) = mpsc::unbounded_channel();
        bot.shared.lock().unwrap().outgoing = Some(tx);
        let b = bot.clone();
        let task = tokio::spawn(async move { b.call("cancel", json!({})).await });
        rx.recv().await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(bot.shared.lock().unwrap().pending.is_empty());
    }
}
