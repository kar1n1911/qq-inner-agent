//! 网关路由器：把 OneBot 与 Telegram 两个可选后端聚合成一个 `EngineTransport`。
//! 决策/记忆/发送策略全部由共享 Engine 负责；这里只做 chat/action 归属路由、
//! 通知合并与状态聚合。未启用 Telegram 时行为与只有 OneBot 时完全一致。
use super::telegram::{Log, SeenChat, Telegram, TelegramState};
use super::{Notification, OneBot, OneBotError, State as TransportState};
use crate::config::{self, Config};
use crate::engine::orientation::OrientationTransport;
use crate::engine::EngineTransport;
use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, watch};

/// 合并通知；`Runtime` 据此区分事件来源，日志与 debug.receive 过滤依赖它。
#[derive(Debug, Clone)]
pub enum GatewayNotice {
    OneBot(Notification),
    Telegram(Notification),
}

/// 待启动的后端及其私有通知队列；`run()` 取出后建立转发任务。
enum Feed {
    OneBot(Arc<OneBot>, mpsc::UnboundedReceiver<Notification>),
    Telegram(Arc<Telegram>, mpsc::UnboundedReceiver<Notification>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    OneBot,
    Telegram,
}

pub struct Gateways {
    onebot: Option<Arc<OneBot>>,
    telegram: Option<Arc<Telegram>>,
    telegram_groups: Vec<String>,
    telegram_users: Vec<String>,
    tx: mpsc::UnboundedSender<GatewayNotice>,
    feeds: Mutex<Vec<Feed>>,
}

impl Gateways {
    /// 按配置构造后端。OneBot 始终存在（保持既有 QQ 行为）；Telegram 仅在
    /// `telegram.enabled` 且配置了 token 时加入，避免空 token 反复认证失败。
    pub fn new(
        config: &Config,
        data_dir: PathBuf,
        log: Log,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<GatewayNotice>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (bot, notices) = OneBot::new(config.onebot.clone(), config.onebot_token.text.clone());
        let onebot = Arc::new(bot);
        let mut feeds = vec![Feed::OneBot(onebot.clone(), notices)];
        let telegram = if config.telegram.enabled && config.telegram_token.is_truthy {
            let (bot, notices) = Telegram::new(
                config.telegram.clone(),
                config.telegram_token.text.clone(),
                data_dir,
            );
            let bot = Arc::new(bot.with_log(log));
            feeds.push(Feed::Telegram(bot.clone(), notices));
            Some(bot)
        } else {
            None
        };
        (
            Arc::new(Self {
                onebot: Some(onebot),
                telegram,
                telegram_groups: config.telegram.allowed_groups.clone(),
                telegram_users: config.telegram.allowed_users.clone(),
                tx,
                feeds: Mutex::new(feeds),
            }),
            rx,
        )
    }

    /// 复用现有网关（热重载不重连）。返回的接收端只是占位：`Runtime::reload`
    /// 会立即用现有通知队列替换它，转发任务仍向原有合并通道投递。
    pub fn reuse(existing: Arc<Self>) -> (Arc<Self>, mpsc::UnboundedReceiver<GatewayNotice>) {
        let (_placeholder, rx) = mpsc::unbounded_channel();
        (existing, rx)
    }

    pub fn onebot(&self) -> Option<&Arc<OneBot>> {
        self.onebot.as_ref()
    }

    pub fn telegram(&self) -> Option<&Arc<Telegram>> {
        self.telegram.as_ref()
    }

    pub fn onebot_state(&self) -> Option<TransportState> {
        self.onebot.as_ref().map(|bot| bot.state())
    }

    pub fn telegram_state(&self) -> Option<TelegramState> {
        self.telegram.as_ref().map(|bot| bot.state())
    }

    /// 历史回填只依赖 OneBot；Telegram 不提供 group history。
    pub fn onebot_connected(&self) -> bool {
        self.onebot_state().is_some_and(|state| state.connected)
    }

    /// 未授权 chat 观测记录；Telegram 传输层已按 R8 记录并打日志，这里直接透传。
    pub fn seen_chats(&self) -> Vec<SeenChat> {
        self.telegram
            .as_ref()
            .map(|bot| bot.seen())
            .unwrap_or_default()
    }

    /// 聚合状态快照；`Runtime` 组装 status.json 时逐字段取用。
    pub fn status(&self) -> Value {
        let onebot = self.onebot_state();
        let telegram = self.telegram_state();
        json!({
            "onebotConnected": onebot.as_ref().is_some_and(|state| state.connected),
            "qqOnline": onebot.as_ref().is_some_and(|state| state.online),
            "selfId": onebot.as_ref().map(|state| state.self_id.clone()).unwrap_or_default(),
            "reconnects": onebot.as_ref().map(|state| state.reconnects).unwrap_or(0),
            "telegramConnected": telegram.as_ref().is_some_and(|state| state.connected),
            "telegramOnline": telegram.as_ref().is_some_and(|state| state.online),
            "telegramSelfId": telegram.as_ref().map(|state| state.self_id.clone()).unwrap_or_default(),
            "telegramUsername": telegram.as_ref().map(|state| state.username.clone()).unwrap_or_default(),
            "telegramReconnects": telegram.as_ref().map(|state| state.reconnects).unwrap_or(0),
            "telegramLastError": telegram.as_ref().map(|state| state.last_error.clone()).unwrap_or_default(),
        })
    }

    /// chat 归属：Telegram 白名单命中则归 Telegram，否则 OneBot（未启用则回退 Telegram）。
    fn owns(&self, chat: &str) -> Route {
        if self.telegram.is_some() {
            if let Some((kind, id)) = chat.split_once(':') {
                match kind {
                    "group" if self.telegram_groups.iter().any(|group| group == id) => {
                        return Route::Telegram
                    }
                    "private" if self.telegram_users.iter().any(|user| user == id) => {
                        return Route::Telegram
                    }
                    _ => {}
                }
            }
        }
        Route::OneBot
    }

    /// action 归属：群/用户 action 按参数归属，无法判定时优先 OneBot。
    fn owns_call(&self, action: &str, params: &Value) -> Route {
        if self.telegram.is_none() {
            return Route::OneBot;
        }
        let group = config::js_string(&params["group_id"]);
        let user = config::js_string(&params["user_id"]);
        match action {
            // OneBot 未连接时用 getMe 兜底；其余无法判定仍优先 OneBot。
            "get_login_info" if !self.onebot_connected() => Route::Telegram,
            "get_group_info"
            | "get_group_member_info"
            | "_get_group_notice"
            | "get_group_member_list"
            | "get_group_msg_history" => {
                if self.telegram_groups.iter().any(|value| value == &group) {
                    Route::Telegram
                } else {
                    Route::OneBot
                }
            }
            "get_friend_list" | "get_stranger_info" => {
                if self.telegram_users.iter().any(|value| value == &user) {
                    Route::Telegram
                } else {
                    Route::OneBot
                }
            }
            _ => Route::OneBot,
        }
    }

    /// 启动各后端并转发其通知到合并队列；停机时等待全部转发任务退出。
    pub async fn run(&self, shutdown: watch::Receiver<bool>) {
        let feeds: Vec<Feed> = std::mem::take(&mut *self.feeds.lock().unwrap());
        // 事件来源必须与 chat 归属一致；过滤只依赖白名单，与后端实例无关。
        let filter = EventFilter {
            telegram: self.telegram.is_some(),
            groups: self.telegram_groups.clone(),
            users: self.telegram_users.clone(),
        };
        let mut tasks = Vec::new();
        for feed in feeds {
            let tx = self.tx.clone();
            let filter = filter.clone();
            match feed {
                Feed::OneBot(bot, rx) => {
                    let stop = shutdown.clone();
                    tasks.push(tokio::spawn(async move {
                        pipe(rx, tx, GatewayNotice::OneBot, filter, async move {
                            bot.run(stop).await
                        })
                        .await;
                    }));
                }
                Feed::Telegram(bot, rx) => {
                    let stop = shutdown.clone();
                    tasks.push(tokio::spawn(async move {
                        pipe(rx, tx, GatewayNotice::Telegram, filter, async move {
                            bot.run(stop).await
                        })
                        .await;
                    }));
                }
            }
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// 把后端通知转发到合并队列；后端 run 结束后退出（不会因队列空闲而挂起）。
async fn pipe<F>(
    mut rx: mpsc::UnboundedReceiver<Notification>,
    tx: mpsc::UnboundedSender<GatewayNotice>,
    wrap: fn(Notification) -> GatewayNotice,
    filter: EventFilter,
    backend: F,
) where
    F: std::future::Future<Output = ()>,
{
    tokio::pin!(backend);
    loop {
        tokio::select! {
            _ = &mut backend => break,
            notice = rx.recv() => match notice {
                Some(notice) => {
                    let wrapped = wrap(notice);
                    if !filter.allows(&wrapped) {
                        continue;
                    }
                    if tx.send(wrapped).is_err() {
                        break;
                    }
                }
                None => break,
            },
        }
    }
}

/// 合并前的事件过滤：来源网关必须与事件 chat 的归属一致，避免把 Telegram 事件的
/// 回复发到同号 QQ（或反向）。只约束 message/notice，其余事件原样通过。
#[derive(Clone)]
struct EventFilter {
    telegram: bool,
    groups: Vec<String>,
    users: Vec<String>,
}
impl EventFilter {
    fn allows(&self, notice: &GatewayNotice) -> bool {
        let (is_telegram, event) = match notice {
            GatewayNotice::OneBot(Notification::Event(event)) => (false, event),
            GatewayNotice::Telegram(Notification::Event(event)) => (true, event),
            _ => return true,
        };
        let chat = match event["post_type"].as_str() {
            Some("message") if event["message_type"] == "group" => {
                format!("group:{}", config::js_string(&event["group_id"]))
            }
            Some("message") => format!("private:{}", config::js_string(&event["user_id"])),
            Some("notice") => format!("group:{}", config::js_string(&event["group_id"])),
            _ => return true,
        };
        let telegram_owned = self.telegram
            && match chat.split_once(':') {
                Some(("group", id)) => self.groups.iter().any(|value| value == id),
                Some(("private", id)) => self.users.iter().any(|value| value == id),
                _ => false,
            };
        telegram_owned == is_telegram
    }
}

impl OrientationTransport for Gateways {
    fn self_id(&self) -> String {
        if let Some(bot) = &self.onebot {
            let id = bot.state().self_id;
            if !id.is_empty() {
                return id;
            }
        }
        self.telegram
            .as_ref()
            .map(|bot| bot.self_id())
            .unwrap_or_default()
    }

    fn event_self_id(&self, event: &Value) -> String {
        let id = config::js_string(&event["self_id"]);
        if !id.is_empty() && id != "null" {
            let known = self
                .onebot
                .as_ref()
                .is_some_and(|bot| bot.state().self_id == id)
                || self
                    .telegram
                    .as_ref()
                    .is_some_and(|bot| bot.self_id() == id);
            if known {
                return id;
            }
        }
        self.self_id()
    }

    fn chat_self_id(&self, chat: &str) -> String {
        match self.owns(chat) {
            Route::Telegram => self
                .telegram
                .as_ref()
                .map(|bot| bot.self_id())
                .unwrap_or_default(),
            Route::OneBot => {
                if let Some(bot) = &self.onebot {
                    let id = bot.state().self_id;
                    if !id.is_empty() {
                        return id;
                    }
                }
                self.telegram
                    .as_ref()
                    .map(|bot| bot.self_id())
                    .unwrap_or_default()
            }
        }
    }

    /// Telegram 归属的群没有 get_group_msg_history；回填据此跳过。
    fn can_fetch_history(&self, group_id: &str) -> bool {
        !matches!(self.owns(&format!("group:{group_id}")), Route::Telegram)
    }

    /// 历史回填只看 OneBot；Telegram 无群历史，不能因为 Telegram 在线就触发回填。
    fn history_available(&self) -> bool {
        self.onebot_connected()
    }

    fn call<'a>(&'a self, action: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            match self.owns_call(action, &params) {
                Route::Telegram => match &self.telegram {
                    Some(bot) => bot.call(action, params).await,
                    None => anyhow::bail!("telegram_offline"),
                },
                Route::OneBot => match &self.onebot {
                    Some(bot) => Ok(bot.call(action, params).await?),
                    None => match &self.telegram {
                        Some(bot) => bot.call(action, params).await,
                        None => anyhow::bail!("qq_offline"),
                    },
                },
            }
        })
    }
}

impl EngineTransport for Gateways {
    fn state(&self) -> TransportState {
        let onebot = self.onebot_state();
        let telegram = self.telegram_state();
        let connected = onebot.as_ref().is_some_and(|state| state.connected)
            || telegram.as_ref().is_some_and(|state| state.connected);
        let online = onebot.as_ref().is_some_and(|state| state.online)
            || telegram.as_ref().is_some_and(|state| state.online);
        let self_id = onebot
            .as_ref()
            .map(|state| state.self_id.clone())
            .filter(|id| !id.is_empty())
            .or_else(|| {
                telegram
                    .as_ref()
                    .map(|state| state.self_id.clone())
                    .filter(|id| !id.is_empty())
            })
            .unwrap_or_default();
        let reconnects = onebot.as_ref().map(|state| state.reconnects).unwrap_or(0)
            + telegram.as_ref().map(|state| state.reconnects).unwrap_or(0);
        TransportState {
            connected,
            online,
            self_id,
            reconnects,
        }
    }

    fn send<'a>(
        &'a self,
        chat: &'a str,
        text: &'a str,
        face: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            match self.owns(chat) {
                Route::Telegram => match &self.telegram {
                    Some(bot) => bot.send(chat, text, face).await,
                    None => Err(OneBotError {
                        code: "telegram_offline".into(),
                        uncertain: false,
                    }),
                },
                Route::OneBot => match &self.onebot {
                    Some(bot) => bot.send(chat, text, face).await,
                    None => match &self.telegram {
                        Some(bot) => bot.send(chat, text, face).await,
                        None => Err(OneBotError {
                            code: "qq_offline".into(),
                            uncertain: false,
                        }),
                    },
                },
            }
        })
    }

    fn send_media<'a>(
        &'a self,
        chat: &'a str,
        segment: Value,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            match self.owns(chat) {
                // Telegram 需要 multipart 上传；ureq 2 无内建 multipart，一期降级。
                Route::Telegram => Err(OneBotError {
                    code: "telegram_media_unsupported".into(),
                    uncertain: false,
                }),
                Route::OneBot => match &self.onebot {
                    Some(bot) => bot.send_media(chat, segment).await,
                    None => Err(OneBotError {
                        code: "media_transport_unavailable".into(),
                        uncertain: false,
                    }),
                },
            }
        })
    }
}

#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;
