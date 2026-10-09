//! Telegram Bot API 正向连接（长轮询）。与 OneBot 平级：只翻译事件与请求，
//! 决策/记忆/发送策略仍由共享 Engine 负责。
//!
//! 传输约束：
//! - 全部阻塞 `ureq` 调用经 `tokio::task::spawn_blocking`，不阻塞 runtime worker。
//! - `getUpdates` 的 offset 持久化在 `data_dir/telegram-offset.json`，重启不重复消费。
//! - 未授权 chat 的消息仍会翻译为事件（引擎会拒绝），同时记录到 seen registry 并
//!   打 `telegram_chat_ignored` 日志，方便用户配置白名单。
use super::{Notification, OneBotError};
use crate::config;
use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    time::{sleep, Instant},
};

/// 与 `engine::Options::log` 相同形状的回调；Stage C 的 Gateways 会注入真实 logger。
pub type Log = Arc<dyn Fn(&str, Value) + Send + Sync>;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[error("{code}")]
pub struct TelegramError {
    pub code: String,
    /// 请求可能已经执行；上层绝不能因此自动重发消息。
    pub uncertain: bool,
    /// 429 响应里的 `parameters.retry_after`（秒）。
    pub retry_after: Option<u64>,
}
impl TelegramError {
    fn new(code: impl Into<String>, uncertain: bool, retry_after: Option<u64>) -> Self {
        Self {
            code: code.into(),
            uncertain,
            retry_after,
        }
    }
    fn transient(code: impl Into<String>) -> Self {
        Self::new(code, true, None)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramState {
    pub connected: bool,
    pub online: bool,
    pub self_id: String,
    pub username: String,
    pub reconnects: u64,
    pub last_error: String,
}

/// 未授权 chat 的观测记录；供仪表盘 contacts 与日志提示使用。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenChat {
    /// `group:<id>` 或 `private:<id>`。
    pub chat: String,
    pub kind: String,
    pub id: String,
    pub title: String,
    pub user_id: String,
    pub username: String,
    pub nickname: String,
    pub at: f64,
}

#[derive(Clone)]
pub struct Telegram {
    config: config::Telegram,
    token: String,
    base_url: String,
    agent: ureq::Agent,
    state: Arc<Mutex<TelegramState>>,
    /// 下一个 getUpdates offset；None 表示从未成功消费过 update。
    offset: Arc<Mutex<Option<i64>>>,
    offset_path: PathBuf,
    notices: mpsc::UnboundedSender<Notification>,
    lifecycle: Arc<tokio::sync::Mutex<()>>,
    seen: Arc<Mutex<HashMap<String, SeenChat>>>,
    log: Log,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    random: Arc<dyn Fn() -> f64 + Send + Sync>,
    #[cfg(test)]
    sleep: Option<Arc<dyn Fn(Duration) + Send + Sync>>,
}

impl Telegram {
    pub fn new(
        config: config::Telegram,
        token: String,
        data_dir: PathBuf,
    ) -> (Self, mpsc::UnboundedReceiver<Notification>) {
        let (notices, rx) = mpsc::unbounded_channel();
        let offset_path = data_dir.join("telegram-offset.json");
        let offset = read_offset(&offset_path);
        // 复用与 Provider 相同的代理/超时模式；禁用环境代理，只认显式配置。
        let mut builder = ureq::AgentBuilder::new()
            .redirects(0)
            .try_proxy_from_env(false)
            .timeout(Duration::from_secs_f64(config.request_timeout_seconds));
        let mut proxy_error = None;
        if !config.proxy.trim().is_empty() {
            match ureq::Proxy::new(config.proxy.as_str()) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(error) => proxy_error = Some(error.to_string()),
            }
        }
        let mut state = TelegramState::default();
        if let Some(error) = proxy_error {
            // 配置层已校验 URL；这里是最后一道防线，降级为直连并把原因写进状态。
            state.last_error = format!("telegram_proxy_invalid: {error}");
        }
        (
            Self {
                config,
                token,
                base_url: "https://api.telegram.org".into(),
                agent: builder.build(),
                state: Arc::new(Mutex::new(state)),
                offset: Arc::new(Mutex::new(offset)),
                offset_path,
                notices,
                lifecycle: Arc::new(tokio::sync::Mutex::new(())),
                seen: Arc::new(Mutex::new(HashMap::new())),
                log: Arc::new(|_, _| {}),
                now: Arc::new(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64()
                }),
                random: Arc::new(rand::random),
                #[cfg(test)]
                sleep: None,
            },
            rx,
        )
    }

    /// 注入日志回调；必须在包进 `Arc` 之前调用。
    pub fn with_log(mut self, log: Log) -> Self {
        self.log = log;
        self
    }

    /// 覆盖 API 根地址，仅用于测试 mock 与私有部署。
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn state(&self) -> TelegramState {
        self.state.lock().unwrap().clone()
    }

    /// 仅测试：直接填充身份，避免网关路由单测访问网络。
    #[cfg(test)]
    pub(crate) fn set_identity_for_test(&self, self_id: &str, username: &str) {
        let mut state = self.state.lock().unwrap();
        state.self_id = self_id.into();
        state.username = username.into();
        state.connected = true;
        state.online = true;
    }

    pub fn self_id(&self) -> String {
        self.state.lock().unwrap().self_id.clone()
    }

    /// 本次运行见过的未授权 chat，按 chat key 排序。
    pub fn seen(&self) -> Vec<SeenChat> {
        let mut list: Vec<SeenChat> = self.seen.lock().unwrap().values().cloned().collect();
        list.sort_by(|a, b| a.chat.cmp(&b.chat));
        list
    }

    /// 取走并清空 seen registry；Gateways 合并后调用。
    pub fn take_seen(&self) -> Vec<SeenChat> {
        let mut map = self.seen.lock().unwrap();
        let mut list: Vec<SeenChat> = map.drain().map(|(_, value)| value).collect();
        list.sort_by(|a, b| a.chat.cmp(&b.chat));
        list
    }

    fn request_timeout(&self) -> Duration {
        Duration::from_secs_f64(self.config.request_timeout_seconds)
    }

    fn status(&self, code: &str) {
        let _ = self.notices.send(Notification::Status(code.into()));
    }

    fn disconnect(&self) {
        let mut state = self.state.lock().unwrap();
        state.connected = false;
        state.online = false;
    }

    /// 单次认证，不重连；成功时填充 self_id/username 并置 connected/online。
    pub async fn check(&self) -> std::result::Result<TelegramState, TelegramError> {
        let _guard = self.lifecycle.lock().await;
        self.check_inner().await
    }

    async fn check_inner(&self) -> std::result::Result<TelegramState, TelegramError> {
        let this = self.clone();
        let timeout = self.request_timeout();
        let me = tokio::task::spawn_blocking(move || this.api("getMe", &json!({}), timeout))
            .await
            .map_err(|_| TelegramError::transient("telegram_task_failed"))??;
        if me["is_bot"] != true {
            return Err(TelegramError::new("telegram_not_a_bot", false, None));
        }
        let id = crate::config::js_string(&me["id"]);
        if id.is_empty() || id == "null" {
            return Err(TelegramError::new("telegram_missing_identity", false, None));
        }
        let username = me["username"].as_str().unwrap_or("").to_string();
        let mut state = self.state.lock().unwrap();
        state.self_id = id;
        state.username = username;
        state.connected = true;
        state.online = true;
        state.last_error.clear();
        Ok(state.clone())
    }

    /// 发送纯文本。Telegram 无 face 段，`face` 直接忽略；空正文按无效处理。
    pub async fn send(
        &self,
        chat: &str,
        text: &str,
        face: Option<&str>,
    ) -> std::result::Result<Value, OneBotError> {
        let _ = face;
        let Some((_kind, id)) = parse_chat(chat) else {
            return Err(OneBotError {
                code: "invalid_chat".into(),
                uncertain: false,
            });
        };
        let state = self.state();
        if !state.connected || !state.online {
            return Err(OneBotError {
                code: "telegram_offline".into(),
                uncertain: false,
            });
        }
        if text.is_empty() {
            return Err(OneBotError {
                code: "invalid_text".into(),
                uncertain: false,
            });
        }
        let Ok(chat_id) = id.parse::<i64>() else {
            return Err(OneBotError {
                code: "invalid_chat".into(),
                uncertain: false,
            });
        };
        let params = json!({"chat_id": chat_id, "text": text, "disable_web_page_preview": true});
        let this = self.clone();
        let timeout = self.request_timeout();
        let result = tokio::task::spawn_blocking(move || this.api("sendMessage", &params, timeout))
            .await
            .map_err(|_| OneBotError {
                code: "telegram_task_failed".into(),
                uncertain: false,
            })?;
        match result {
            Ok(value) => Ok(json!({"message_id": value["message_id"]})),
            Err(error) => Err(OneBotError {
                code: error.code,
                uncertain: error.uncertain,
            }),
        }
    }

    /// OneBot action 映射表（design §3.3）；不支持的 QQ 专属 action 返回 `unsupported_action`。
    pub async fn call(&self, action: &str, params: Value) -> Result<Value> {
        let this = self.clone();
        let action = action.to_string();
        tokio::task::spawn_blocking(move || this.call_blocking(&action, &params))
            .await
            .map_err(|_| anyhow::anyhow!("telegram_task_failed"))?
    }

    fn call_blocking(&self, action: &str, params: &Value) -> Result<Value> {
        let request = |method: &str, params: Value| -> Result<Value> {
            self.api(method, &params, self.request_timeout())
                .map_err(|error| anyhow::anyhow!("{}", error.code))
        };
        match action {
            "get_login_info" => {
                let me = request("getMe", json!({}))?;
                Ok(json!({"user_id": me["id"], "nickname": display_name(&me)}))
            }
            "get_group_info" => {
                let group_id =
                    as_id(&params["group_id"]).ok_or_else(|| anyhow::anyhow!("invalid_chat"))?;
                let chat = request("getChat", json!({"chat_id": group_id}))?;
                let count = request("getChatMemberCount", json!({"chat_id": group_id}))?;
                Ok(json!({
                    "group_id": group_id,
                    "group_name": chat["title"],
                    "member_count": count,
                    "max_member_count": 0,
                }))
            }
            "get_group_member_info" => {
                let group_id =
                    as_id(&params["group_id"]).ok_or_else(|| anyhow::anyhow!("invalid_chat"))?;
                let user_id =
                    as_id(&params["user_id"]).ok_or_else(|| anyhow::anyhow!("invalid_chat"))?;
                let member = request(
                    "getChatMember",
                    json!({"chat_id": group_id, "user_id": user_id}),
                )?;
                let user = &member["user"];
                Ok(json!({
                    "user_id": user["id"],
                    "nickname": display_name(user),
                    "card": user["username"],
                }))
            }
            "_get_group_notice" => {
                let group_id =
                    as_id(&params["group_id"]).ok_or_else(|| anyhow::anyhow!("invalid_chat"))?;
                let chat = request("getChat", json!({"chat_id": group_id}))?;
                let pinned = &chat["pinned_message"];
                if pinned.is_null() {
                    Ok(json!([]))
                } else {
                    let text = pinned["text"]
                        .as_str()
                        .or_else(|| pinned["caption"].as_str())
                        .unwrap_or("");
                    Ok(json!([{"message":{"text": text}}]))
                }
            }
            // Bot API 物理上不提供的能力；引擎按降级路径处理。
            "get_group_member_list" | "get_group_msg_history" | "get_forward_msg" => {
                anyhow::bail!("unsupported_action")
            }
            _ => anyhow::bail!("unsupported_action"),
        }
    }

    /// 长轮询主循环：认证 → getUpdates → 翻译事件；错误按类别退避重连。
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        let _guard = self.lifecycle.lock().await;
        let mut delay = 1.0f64;
        while !*shutdown.borrow() {
            let started = Instant::now();
            let mut error: Option<TelegramError> = None;
            match self.check_inner().await {
                Ok(_) => {
                    self.status("connected");
                    loop {
                        let polled = tokio::select! {
                            _ = shutdown.changed() => break,
                            result = self.poll_once() => result,
                        };
                        match polled {
                            Ok(updates) => {
                                delay = 1.0;
                                self.state.lock().unwrap().online = true;
                                for update in &updates {
                                    self.dispatch(update);
                                }
                            }
                            Err(failure) => {
                                error = Some(failure);
                                break;
                            }
                        }
                    }
                }
                Err(failure) => error = Some(failure),
            }
            if *shutdown.borrow() {
                break;
            }
            if let Some(failure) = error {
                {
                    let mut state = self.state.lock().unwrap();
                    state.online = false;
                    state.last_error = failure.code.clone();
                }
                self.status(&failure.code);
                if let Some(retry_after) = failure.retry_after {
                    delay = delay.max(retry_after as f64);
                }
            }
            self.state.lock().unwrap().reconnects += 1;
            if self
                .wait_backoff(&mut delay, started.elapsed(), &mut shutdown)
                .await
            {
                break;
            }
        }
        self.disconnect();
    }

    async fn wait_backoff(
        &self,
        delay: &mut f64,
        elapsed: Duration,
        shutdown: &mut watch::Receiver<bool>,
    ) -> bool {
        let wait = backoff(
            delay,
            elapsed,
            self.config.reconnect_max_seconds,
            (self.random)(),
        );
        #[cfg(test)]
        if let Some(sleep) = &self.sleep {
            sleep(wait);
            return *shutdown.borrow();
        }
        tokio::select! {
            _ = shutdown.changed() => true,
            _ = sleep(wait) => false,
        }
    }

    async fn poll_once(&self) -> std::result::Result<Vec<Value>, TelegramError> {
        let mut params = json!({
            // Telegram 只接受整数 timeout；f64 会序列化成 `20.0` 被拒绝。
            "timeout": self.config.poll_timeout_seconds.round() as i64,
            "allowed_updates": ["message", "my_chat_member"],
        });
        if let Some(offset) = *self.offset.lock().unwrap() {
            params["offset"] = json!(offset);
        }
        // 长轮询请求要等 Telegram 端 timeout 到期，客户端超时必须更长。
        let timeout = Duration::from_secs_f64(self.config.poll_timeout_seconds + 5.0);
        let this = self.clone();
        let updates = tokio::task::spawn_blocking(move || this.api("getUpdates", &params, timeout))
            .await
            .map_err(|_| TelegramError::transient("telegram_task_failed"))??;
        let list = updates.as_array().cloned().unwrap_or_default();
        if let Some(max) = list
            .iter()
            .filter_map(|update| update["update_id"].as_i64())
            .max()
        {
            let next = max + 1;
            *self.offset.lock().unwrap() = Some(next);
            self.persist_offset(next);
        }
        Ok(list)
    }

    fn persist_offset(&self, offset: i64) {
        if let Some(parent) = self.offset_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) =
            crate::settings::atomic_json(&self.offset_path, &json!({"offset": offset}))
        {
            (self.log)(
                "telegram_offset_write_failed",
                json!({"error": error.to_string()}),
            );
        }
    }

    fn dispatch(&self, update: &Value) {
        let (self_id, username) = {
            let state = self.state.lock().unwrap();
            (state.self_id.clone(), state.username.clone())
        };
        let Some(event) = update_to_event(update, &self_id, &username) else {
            return;
        };
        if event["post_type"] == "message" {
            self.observe_chat(update, &event);
        }
        let _ = self.notices.send(Notification::Event(event));
    }

    /// 未授权 chat 记录 id/标题/发送者；引擎稍后会按白名单拒绝该事件。
    fn observe_chat(&self, update: &Value, event: &Value) {
        let kind = event["message_type"].as_str().unwrap_or("");
        let target = if kind == "group" {
            &event["group_id"]
        } else {
            &event["user_id"]
        };
        let chat = format!("{kind}:{}", crate::config::js_string(target));
        if self.allowed(&chat) {
            return;
        }
        let sender = &update["message"]["from"];
        let record = SeenChat {
            chat: chat.clone(),
            kind: kind.to_string(),
            id: chat.split_once(':').map(|(_, id)| id).unwrap_or("").into(),
            title: update["message"]["chat"]["title"]
                .as_str()
                .unwrap_or("")
                .into(),
            user_id: crate::config::js_string(&sender["id"]),
            username: sender["username"].as_str().unwrap_or("").into(),
            nickname: display_name(sender),
            at: (self.now)(),
        };
        self.seen.lock().unwrap().insert(chat.clone(), record);
        (self.log)(
            "telegram_chat_ignored",
            json!({
                "chat": chat,
                "title": update["message"]["chat"]["title"],
                "user_id": crate::config::js_string(&sender["id"]),
                "username": sender["username"],
            }),
        );
    }

    fn allowed(&self, chat: &str) -> bool {
        match chat.split_once(':') {
            Some(("group", id)) => self.config.allowed_groups.iter().any(|value| value == id),
            Some(("private", id)) => self.config.allowed_users.iter().any(|value| value == id),
            _ => false,
        }
    }

    /// 同步 HTTP 请求，必须只在 blocking 池调用；返回 Telegram 的 `result` 字段。
    fn api(
        &self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> std::result::Result<Value, TelegramError> {
        let url = format!("{}/bot{}/{}", self.base_url, self.token, method);
        let response = match self
            .agent
            .post(&url)
            .timeout(timeout)
            .send_json(params.clone())
        {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(_)) => {
                return Err(TelegramError::transient("telegram_network_error"))
            }
        };
        let status = response.status();
        let mut bytes = Vec::new();
        if response
            .into_reader()
            .take(4_000_001)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return Err(TelegramError::transient("telegram_network_error"));
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if body["ok"] == true {
            return Ok(body["result"].clone());
        }
        Err(classify(status, &body))
    }
}

fn classify(status: u16, body: &Value) -> TelegramError {
    let code = body["error_code"]
        .as_u64()
        .map(|value| value.min(u16::MAX as u64) as u16)
        .unwrap_or(status);
    let retry_after = body["parameters"]["retry_after"].as_u64();
    let description = body["description"].as_str().unwrap_or("").to_lowercase();
    match code {
        401 => TelegramError::new("telegram_unauthorized", false, None),
        409 => TelegramError::new(
            if description.contains("webhook") {
                "webhook_conflict"
            } else {
                "telegram_conflict"
            },
            false,
            None,
        ),
        429 => TelegramError::new("rate_limited", false, retry_after),
        400 => TelegramError::new("telegram_bad_request", false, None),
        403 => TelegramError::new("telegram_forbidden", false, None),
        404 => TelegramError::new("telegram_not_found", false, None),
        300..=399 => TelegramError::transient("telegram_redirect"),
        500..=599 => TelegramError::transient("telegram_server_error"),
        _ => TelegramError::new(format!("telegram_http_{code}"), false, None),
    }
}

/// 与 OneBot 相同的退避语义：稳定超过 30 秒后重置；上限为 reconnectMaxSeconds。
fn backoff(delay: &mut f64, elapsed: Duration, max: f64, jitter: f64) -> Duration {
    if elapsed > Duration::from_secs(30) {
        *delay = 1.0;
    }
    let wait = Duration::from_secs_f64(*delay + jitter);
    *delay = (*delay * 2.0).min(max);
    wait
}

fn parse_chat(chat: &str) -> Option<(&str, &str)> {
    let (kind, id) = chat.split_once(':')?;
    if !matches!(kind, "group" | "private") || id.is_empty() {
        return None;
    }
    let digits = id.strip_prefix('-').unwrap_or(id);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // 群 id 可负、私聊必须为正；与 OneBot 的 chat 规则保持一致。
    let value = id.parse::<i64>().ok()?;
    if kind == "private" && value <= 0 {
        return None;
    }
    if kind == "group" && value == 0 {
        return None;
    }
    Some((kind, id))
}

/// 将 OneBot 传来的 id（字符串、整数或 f64）统一成 i64，避免把 `-100.0` 发给 Telegram。
fn as_id(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().map(|value| value as i64))
            .or_else(|| {
                number
                    .as_f64()
                    .filter(|value| value.is_finite() && value.fract() == 0.0)
                    .map(|value| value as i64)
            }),
        Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn read_offset(path: &Path) -> Option<i64> {
    let value = crate::settings::read_json(path).ok().flatten()?;
    value
        .get("offset")
        .and_then(Value::as_i64)
        .or_else(|| value.as_i64())
}

fn display_name(user: &Value) -> String {
    let first = user["first_name"].as_str().unwrap_or("");
    let last = user["last_name"].as_str().unwrap_or("");
    let full = format!("{first} {last}");
    let full = full.trim();
    if !full.is_empty() {
        return full.to_string();
    }
    if let Some(username) = user["username"].as_str().filter(|value| !value.is_empty()) {
        return username.to_string();
    }
    let id = crate::config::js_string(&user["id"]);
    if id == "null" {
        String::new()
    } else {
        id
    }
}

fn utf16_range(text: &str, start: i64, end: i64) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let len = units.len() as i64;
    let begin = start.clamp(0, len) as usize;
    let finish = end.clamp(0, len) as usize;
    if begin >= finish {
        return String::new();
    }
    String::from_utf16_lossy(&units[begin..finish])
}

fn push_text(out: &mut Vec<Value>, text: &str) {
    if !text.is_empty() {
        out.push(json!({"type":"text","data":{"text": text}}));
    }
}

/// 按 Telegram 的 UTF-16 `entities` 切段；`text_mention`/`mention` 转 at 段。
fn entity_segments(base: &str, entities: &[Value], bot_id: &str, bot_username: &str) -> Vec<Value> {
    let len = base.encode_utf16().count() as i64;
    let mut out = Vec::new();
    let mut cursor = 0i64;
    let mut sorted: Vec<&Value> = entities.iter().collect();
    sorted.sort_by_key(|entity| entity["offset"].as_i64().unwrap_or(0));
    for entity in sorted {
        let offset = entity["offset"].as_i64().unwrap_or(-1);
        let length = entity["length"].as_i64().unwrap_or(0);
        // 不可信输入：用 checked_add 防止 offset/length 极大时回绕或 panic；
        // 越界实体整体跳过，clamp 由 utf16_range 兜底。
        let Some(end) = offset.checked_add(length) else {
            continue;
        };
        if offset < cursor || length <= 0 || end > len {
            continue;
        }
        if offset > cursor {
            push_text(&mut out, &utf16_range(base, cursor, offset));
        }
        let text = utf16_range(base, offset, end);
        match entity["type"].as_str() {
            Some("text_mention") => {
                let id = crate::config::js_string(&entity["user"]["id"]);
                if id.is_empty() || id == "null" {
                    push_text(&mut out, &text);
                } else {
                    out.push(json!({"type":"at","data":{"qq": id}}));
                }
            }
            Some("mention")
                if !bot_username.is_empty()
                    && text.eq_ignore_ascii_case(&format!("@{bot_username}")) =>
            {
                out.push(json!({"type":"at","data":{"qq": bot_id}}));
            }
            _ => push_text(&mut out, &text),
        }
        cursor = end;
    }
    if cursor < len {
        push_text(&mut out, &utf16_range(base, cursor, len));
    }
    out
}

fn media_marker(msg: &Value) -> Option<&'static str> {
    for (key, kind) in [
        ("photo", "image"),
        ("video", "video"),
        ("animation", "image"),
        ("voice", "voice"),
        ("audio", "voice"),
        ("document", "file"),
        ("sticker", "face"),
        ("video_note", "video"),
    ] {
        if msg.get(key).is_some_and(|value| !value.is_null()) {
            return Some(kind);
        }
    }
    None
}

fn forwarded_prefix(msg: &Value) -> Option<String> {
    let labeled = |name: &str| {
        if name.is_empty() {
            "[forwarded]".to_string()
        } else {
            format!("[forwarded from {name}]")
        }
    };
    if let Some(origin) = msg.get("forward_origin").filter(|value| value.is_object()) {
        let name = match origin["type"].as_str() {
            Some("user") => display_name(&origin["sender_user"]),
            Some("hidden_user") => origin["sender_user_name"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            Some("chat") => origin["sender_chat"]["title"]
                .as_str()
                .unwrap_or("")
                .to_string(),
            Some("channel") => origin["chat"]["title"].as_str().unwrap_or("").to_string(),
            _ => String::new(),
        };
        return Some(labeled(&name));
    }
    if let Some(user) = msg.get("forward_from").filter(|value| value.is_object()) {
        return Some(labeled(&display_name(user)));
    }
    if let Some(chat) = msg
        .get("forward_from_chat")
        .filter(|value| value.is_object())
    {
        return Some(labeled(chat["title"].as_str().unwrap_or("")));
    }
    if let Some(name) = msg["forward_sender_name"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        return Some(labeled(name));
    }
    None
}

fn my_chat_member_event(member: &Value, bot_id: &str) -> Option<Value> {
    let chat = member.get("chat").filter(|value| value.is_object())?;
    if !matches!(chat["type"].as_str(), Some("group" | "supergroup")) {
        return None;
    }
    let old = member["old_chat_member"]["status"].as_str().unwrap_or("");
    let new = member["new_chat_member"]["status"].as_str().unwrap_or("");
    if !matches!(new, "member" | "administrator" | "creator") || !matches!(old, "left" | "kicked") {
        return None;
    }
    Some(json!({
        "post_type": "notice",
        "notice_type": "group_increase",
        "sub_type": "invite",
        "group_id": chat["id"],
        "user_id": bot_id,
        "self_id": bot_id,
        "time": member["date"],
    }))
}

/// 纯函数：Telegram update → OneBot 形状事件。`None` 表示忽略该 update。
pub fn update_to_event(update: &Value, bot_id: &str, bot_username: &str) -> Option<Value> {
    if let Some(member) = update
        .get("my_chat_member")
        .filter(|value| !value.is_null())
    {
        return my_chat_member_event(member, bot_id);
    }
    let msg = update.get("message").filter(|value| value.is_object())?;
    let from = msg.get("from").filter(|value| value.is_object())?;
    let chat = msg.get("chat").filter(|value| value.is_object())?;
    let message_type = match chat["type"].as_str()? {
        "private" => "private",
        "group" | "supergroup" => "group",
        _ => return None,
    };
    let target = if message_type == "group" {
        chat["id"].clone()
    } else {
        from["id"].clone()
    };
    if target.is_null() || msg["message_id"].is_null() {
        return None;
    }

    let mut segments = Vec::new();
    if let Some(reply) = msg
        .get("reply_to_message")
        .filter(|value| value.is_object())
    {
        segments.push(json!({"type":"reply","data":{}}));
        let reply_from = &reply["from"]["id"];
        if !reply_from.is_null() && crate::config::js_string(reply_from) == bot_id {
            segments.push(json!({"type":"at","data":{"qq": bot_id}}));
        }
    }
    if let Some(prefix) = forwarded_prefix(msg) {
        push_text(&mut segments, &prefix);
    }
    if let Some(kind) = media_marker(msg) {
        segments.push(json!({"type": kind}));
    }
    let empty: Vec<Value> = Vec::new();
    if let Some(text) = msg["text"].as_str() {
        let entities = msg["entities"].as_array().unwrap_or(&empty);
        segments.extend(entity_segments(text, entities, bot_id, bot_username));
    } else if let Some(caption) = msg["caption"].as_str() {
        let entities = msg["caption_entities"].as_array().unwrap_or(&empty);
        segments.extend(entity_segments(caption, entities, bot_id, bot_username));
    }
    for (key, label) in [
        ("location", "location"),
        ("venue", "venue"),
        ("contact", "contact"),
        ("poll", "poll"),
    ] {
        if msg.get(key).is_some_and(|value| !value.is_null()) {
            push_text(&mut segments, &format!("[{label}]"));
        }
    }
    if segments.is_empty() {
        return None;
    }

    let card = from["username"].as_str().unwrap_or("");
    let mut sender = json!({"user_id": from["id"], "nickname": display_name(from)});
    if !card.is_empty() {
        sender["card"] = json!(card);
    }
    let mut event = json!({
        "post_type": "message",
        "message_type": message_type,
        "sub_type": "normal",
        "message_id": msg["message_id"],
        "user_id": from["id"],
        "self_id": bot_id,
        "time": msg["date"],
        "sender": sender,
        "message": segments,
    });
    if message_type == "group" {
        event["group_id"] = target;
    }
    Some(event)
}

#[cfg(test)]
#[path = "telegram_tests.rs"]
mod tests;
