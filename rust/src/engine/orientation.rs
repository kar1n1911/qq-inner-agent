//! 入群观察闸门。只采集实际返回的数据，失败/不支持/格式错误一律 unavailable，绝不编造。
use crate::{
    config::{js_string, truthy, Agent, Observation},
    engine::policy::{clip_chars, js_trim, replace_cq},
    memory::{array, text, valid_text},
    transport::{js_number, OneBot},
    prompts::ORIENTATION,
    transport::provider::Provider,
    store::{OrientationRow, Store},
};
use anyhow::{ensure, Result};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};
use tokio::sync::watch;

fn clip(v: &Value, max: usize) -> String {
    // 与 normalize 一致，按 char 保留完整字符；非 BMP 截断差异见阶段测试。
    clip_chars(v.as_str().unwrap_or(""), max)
}
fn fallback<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if truthy(a) {
        a
    } else {
        b
    }
}
fn number_or_zero(v: &Value) -> f64 {
    let n = js_number(v);
    if n.is_nan() {
        0.
    } else {
        n
    }
}
fn message_text(v: &Value, max: usize) -> String {
    if let Some(s) = v.as_str() {
        return clip_chars(&replace_cq(s, |_| Some("[附件或引用]".into())), max);
    }
    let parts: String = array(v)
        .iter()
        .take(40)
        .map(|s| {
            if s["type"] == "text" {
                clip(&s["data"]["text"], max)
            } else {
                let kind = clip(&s["type"], 30);
                format!("[{}]", if kind.is_empty() { "附件" } else { &kind })
            }
        })
        .collect();
    clip_chars(&parts, max)
}

pub fn clean_orientation_source(
    kind: &str,
    value: &Value,
    group_id: &str,
    config: &Observation,
    self_id: &str,
    ignored: &[String],
) -> Result<Value> {
    if kind == "info" {
        ensure!(
            value["group_name"].is_string()
                && (value["group_id"].is_null() || js_string(&value["group_id"]) == group_id),
            "invalid_group_info"
        );
        return Ok(
            json!({"name":clip(&value["group_name"],200), "description":clip(fallback(&value["group_memo"], &value["group_description"]),1000), "memberCount":value["member_count"].as_f64().filter(|v| v.is_finite())}),
        );
    }
    if kind == "notices" {
        ensure!(value.is_array(), "invalid_group_notices");
        // JS 对 null 公告会抛错，整项来源不可用；不要偷偷补空公告。
        let mut notices = Vec::new();
        for n in array(value).iter().take(5) {
            ensure!(!n.is_null(), "invalid_group_notices");
            let content = clip(fallback(&n["message"]["text"], &n["text"]), 1500);
            if !content.is_empty() {
                notices.push(json!({"sender": if truthy(&n["sender_id"]) {js_string(&n["sender_id"])} else {String::new()}, "time":number_or_zero(&n["publish_time"]), "text":content}));
            }
        }
        return Ok(json!(notices));
    }
    ensure!(value["messages"].is_array(), "invalid_group_history");
    let mut messages = Vec::new();
    for m in array(&value["messages"]) {
        ensure!(!m.is_null(), "invalid_group_history");
        let sender = js_string(&m["user_id"]);
        if (!truthy(&m["group_id"]) || js_string(&m["group_id"]) == group_id)
            && sender.starts_with(|c: char| ('1'..='9').contains(&c))
            && sender.bytes().all(|b| b.is_ascii_digit())
            && sender != self_id
            && !ignored.contains(&sender)
        {
            messages.push(m);
        }
    }
    let start = messages.len().saturating_sub(config.history_limit as usize);
    Ok(json!(messages[start..].iter().filter_map(|m| {
        let content = message_text(if m["message"].is_null() { &m["raw_message"] } else { &m["message"] },800);
        (!content.is_empty()).then(|| json!({"id":if m["message_id"].is_null() {String::new()} else {js_string(&m["message_id"])}, "sender":js_string(&m["user_id"]), "time":number_or_zero(&m["time"]), "name":clip(fallback(&m["sender"]["card"], &m["sender"]["nickname"]),80), "text":content}))
    }).collect::<Vec<_>>()))
}

/// JS 纯函数只判阈值；disabled 的直接放行位于 before_speak，不改变此函数契约。
pub fn observation_satisfied(row: &OrientationRow, now: f64, config: &Observation) -> bool {
    let time = now - row.started >= config.min_seconds;
    let volume = row.message_count as f64 >= config.min_messages;
    if config.threshold_mode == "both" {
        time && volume
    } else {
        time || volume
    }
}

/// 可注入边界；真实 OneBot 与 provider 使用已有实现，测试无需外网。
pub trait OrientationTransport: Send + Sync {
    fn self_id(&self) -> String;
    fn call<'a>(&'a self, action: &'a str, params: Value) -> BoxFuture<'a, Result<Value>>;
}
impl OrientationTransport for OneBot {
    fn self_id(&self) -> String {
        self.state().self_id
    }
    fn call<'a>(&'a self, action: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move { Ok(OneBot::call(self, action, params).await?) })
    }
}
pub trait OrientationProvider: Send + Sync {
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>>;
    /// 仅昵称生成使用纯文本；默认适配便于已有测试/替代 provider 返回字符串值。
    fn text<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            self.json(system, payload)
                .await?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("invalid_model_text"))
        })
    }
}
impl OrientationProvider for Provider {
    fn text<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Ok(self.complete(system, &payload.to_string()).await?) })
    }
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move { Ok(Provider::json(self, system, &payload).await?) })
    }
}

pub struct GroupOrientation {
    store: Arc<Mutex<Store>>,
    agent: Agent,
    provider: Arc<dyn OrientationProvider>,
    transport: Arc<dyn OrientationTransport>,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    signal: watch::Receiver<bool>,
    collection_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}
impl GroupOrientation {
    pub fn new(
        store: Arc<Mutex<Store>>,
        agent: Agent,
        provider: Arc<dyn OrientationProvider>,
        transport: Arc<dyn OrientationTransport>,
        now: Arc<dyn Fn() -> f64 + Send + Sync>,
        signal: watch::Receiver<bool>,
    ) -> Result<Self> {
        let this = Self {
            store,
            agent,
            provider,
            transport,
            now,
            signal,
            collection_locks: Mutex::default(),
        };
        if this.agent.observation.enabled {
            for id in &this.agent.allowed_groups {
                this.ensure(&format!("group:{id}"))?;
            }
        }
        Ok(this)
    }
    fn db(&self) -> Result<MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| anyhow::anyhow!("store_poisoned"))
    }
    pub fn ensure(&self, chat: &str) -> Result<OrientationRow> {
        self.db()?.ensure_orientation(chat, (self.now)())
    }
    pub fn get(&self, chat: &str) -> Result<Option<OrientationRow>> {
        self.db()?.orientation_state(chat)
    }
    pub fn joined(&self, chat: &str, timestamp: f64) -> Result<()> {
        self.db()?.orientation_joined(chat, timestamp, (self.now)())
    }
    pub fn observe(&self, chat: &str) -> Result<()> {
        if chat.starts_with("group:") && self.agent.observation.enabled {
            self.db()?.orientation_observe(chat, (self.now)())?;
        }
        Ok(())
    }
    pub fn profile(&self, chat: &str) -> Result<Option<Value>> {
        if !self.agent.observation.enabled || !chat.starts_with("group:") {
            return Ok(None);
        }
        // 保持现行 JS 的 summary、分析元数据与 availability 契约；不暴露原始来源内容。
        Ok(self.get(chat)?.filter(|r| r.status == "ready").map(|r| {
            let mut analysis = r.analysis;
            analysis["sources"] = r.sources["availability"].clone();
            analysis
        }))
    }
    fn fresh(&self, chat: &str, epoch: i64) -> Result<bool> {
        // 重新入群递增 epoch 后，采集和 provider 的在途结果都失效；取消也不得提交。
        Ok(!*self.signal.borrow() && self.get(chat)?.is_some_and(|r| r.epoch == epoch))
    }
    /// 入群/定时立即采集，不再等首次发言；before_speak 共用此入口兜底。
    pub async fn collect(&self, chat: &str, epoch: i64) -> Result<bool> {
        let c = &self.agent.observation;
        if !c.enabled || !chat.starts_with("group:") {
            return Ok(false);
        }
        let lock = self
            .collection_locks
            .lock()
            .map_err(|_| anyhow::anyhow!("collection_locks_poisoned"))?
            .entry(chat.to_owned())
            .or_default()
            .clone();
        // 仅持有异步的按群采集锁；Store 锁在任何 await 前已释放。
        let _guard = lock.lock().await;
        if !self.fresh(chat, epoch)? {
            return Ok(false);
        }
        let Some(row) = self.get(chat)? else {
            return Ok(false);
        };
        if row.collected != 0 || row.status == "ready" {
            return Ok(true);
        }
        let group_id = &chat[6..];
        let id = group_id.parse::<f64>().unwrap_or(f64::NAN);
        let methods = [
            (
                "info",
                "get_group_info",
                json!({"group_id":id,"no_cache":true}),
            ),
            ("notices", "_get_group_notice", json!({"group_id":id})),
            (
                "history",
                "get_group_msg_history",
                json!({"group_id":id,"count":c.history_limit}),
            ),
        ];
        // join_all 保留各项 Result，等价 allSettled：单项失败不阻止另两项采集。
        let results = futures_util::future::join_all(methods.into_iter().map(
            |(kind, action, params)| async move {
                let result = match self.transport.call(action, params).await {
                    Ok(value) => clean_orientation_source(
                        kind,
                        &value,
                        group_id,
                        c,
                        &self.transport.self_id(),
                        &self.agent.ignored_users,
                    ),
                    Err(e) => Err(e),
                };
                (kind, result)
            },
        ))
        .await;
        if !self.fresh(chat, epoch)? {
            return Ok(false);
        }
        let mut sources = json!({"availability":{}});
        for (kind, result) in results {
            sources["availability"][kind] = json!(if result.is_ok() {
                "available"
            } else {
                "unavailable"
            });
            if let Ok(value) = result {
                sources[kind] = value;
            }
        }
        if !self.db()?.orientation_sources(chat, epoch, &sources)? {
            return Ok(false);
        }
        Ok(true)
    }
    pub async fn before_speak(&self, chat: &str) -> Result<bool> {
        let c = &self.agent.observation;
        if !c.enabled || !chat.starts_with("group:") {
            return Ok(true);
        }
        let mut r = self.ensure(chat)?;
        if r.status == "ready" {
            return Ok(true);
        }
        if (self.now)() < r.retry_at || *self.signal.borrow() {
            return Ok(false);
        }
        let epoch = r.epoch;
        if r.collected == 0 {
            if !self.collect(chat, epoch).await? {
                return Ok(false);
            }
            r = self.ensure(chat)?;
            if r.epoch != epoch {
                return Ok(false);
            }
        }
        if !observation_satisfied(&r, (self.now)(), c) {
            return Ok(false);
        }
        let recent = self.db()?.history(chat,Some(c.history_limit as i64))?.into_iter().filter(|m| !truthy(&m["self"])).map(|m| json!({"id":m["id"],"sender":m["sender"],"name":m["name"],"time":m["ts"],"text":clip(&m["text"],800)})).collect::<Vec<_>>();
        let payload = json!({"persona":crate::persona::persona(&*self.db()?, chat, &self.agent.persona.text, &self.agent.identity)?,"group":chat,"observedSeconds":(self.now)()-r.started,"observedMessages":r.message_count,"sources":r.sources,"recentMessages":recent});
        let result = self.provider.json(ORIENTATION, payload).await;
        if !self.fresh(chat, epoch)? {
            return Ok(false);
        }
        if let Ok(value) = result {
            if valid_text(&value["style"], 600)
                && valid_text(&value["summary"], 600)
                && value["topics"].is_array()
                && array(&value["topics"]).len() <= 6
                && array(&value["topics"]).iter().all(|t| valid_text(t, 60))
            {
                let analysis = json!({"style":js_trim(text(&value,"style")), "summary":js_trim(text(&value,"summary")), "topics":value["topics"], "analyzedAt":(self.now)(), "analyzedMessages":r.message_count});
                return self.db()?.orientation_ready(chat, epoch, &analysis);
            }
        }
        self.db()?.orientation_failed(chat, epoch, (self.now)())?;
        Ok(false)
    }
}
