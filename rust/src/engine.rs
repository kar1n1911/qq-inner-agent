//! P6a：按 SURVEY §2.2 的 48 步移植 Engine，不包含 main 运行时或素材发送。
//! 同步段持有 core -> store 锁，等价 JS 两个 await 之间不可插入 ingest；网络等待不持锁。
use crate::{
    activity::{ActivityRhythm, ActivitySnapshot},
    config::{js_string, readiness, truthy, Config},
    expression::{
        decorate, decoration_choices, parse_expressions, personality_context, ExpressionMemory,
    },
    media_select,
    memory::{array, num, parse_memory_updates, text, LayeredMemory},
    onebot::{OneBot, OneBotError, State as TransportState},
    orientation::{GroupOrientation, OrientationProvider, OrientationTransport},
    policy::{self, Allocation, Candidate, CandidateKind, Hint},
    prompts,
    sending::{forecast_result, sending_probability, SendingSettings, Timing},
    store::{LayeredUpdate, ScopedOptions, Store},
};
use anyhow::{ensure, Result};
use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{sync::watch, task::JoinHandle};

pub trait EngineTransport: OrientationTransport {
    fn state(&self) -> TransportState;
    fn send_media<'a>(
        &'a self,
        _chat: &'a str,
        _segment: Value,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async {
            Err(OneBotError {
                code: "media_transport_unavailable".into(),
                uncertain: false,
            })
        })
    }
    fn send<'a>(
        &'a self,
        chat: &'a str,
        text: &'a str,
        face: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>>;
}
impl EngineTransport for OneBot {
    fn state(&self) -> TransportState {
        OneBot::state(self)
    }
    fn send_media<'a>(
        &'a self,
        chat: &'a str,
        segment: Value,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(OneBot::send_media(self, chat, segment))
    }
    fn send<'a>(
        &'a self,
        chat: &'a str,
        text: &'a str,
        face: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(OneBot::send(self, chat, text, face))
    }
}
// 不依赖 trait upcasting（保持仓库 MSRV）；与 orientation 共用同一个传输实例。
struct OrientationAdapter(Arc<dyn EngineTransport>);
impl OrientationTransport for OrientationAdapter {
    fn self_id(&self) -> String {
        self.0.self_id()
    }
    fn call<'a>(&'a self, action: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
        self.0.call(action, params)
    }
}
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Random = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Logger = Arc<dyn Fn(&str, Value) + Send + Sync>;
pub struct Options {
    pub now: Clock,
    pub random: Random,
    pub expression_random: Random,
    pub activity_random: Random,
    /// JS select 使用全局 Math.random，而非发送抽签源；单独注入以保持两者独立。
    pub selection_random: Random,
    /// 回调不可重入 Engine，避免同步段内重入锁。
    pub log: Logger,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            now: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64()
            }),
            random: Arc::new(rand::random),
            expression_random: Arc::new(rand::random),
            activity_random: Arc::new(rand::random),
            selection_random: Arc::new(rand::random),
            log: Arc::new(|_, _| {}),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatState {
    pub version: u64,
    pub last_human: f64,
    pub last_id: String,
    pub hint: Hint,
    pub pending: bool,
    pub pause_done: bool,
    pub last_think: f64,
    pub busy: bool,
    pub due: f64,
}
impl Default for ChatState {
    fn default() -> Self {
        Self {
            version: 0,
            last_human: 0.,
            last_id: String::new(),
            hint: Hint::Open,
            pending: false,
            pause_done: true,
            last_think: 0.,
            busy: false,
            due: 0.,
        }
    }
}
#[derive(Default)]
struct Core {
    // Vec 保持 JS Map 的插入顺序；不能用 HashMap 随机顺序决定并发名额。
    chats: Vec<(String, ChatState)>,
    tasks: Vec<JoinHandle<()>>,
    last_error: Option<String>,
    last_cycle: f64,
}
impl Core {
    fn get(&self, chat: &str) -> Option<&ChatState> {
        self.chats.iter().find(|(c, _)| c == chat).map(|(_, s)| s)
    }
    fn get_mut(&mut self, chat: &str) -> Option<&mut ChatState> {
        self.chats
            .iter_mut()
            .find(|(c, _)| c == chat)
            .map(|(_, s)| s)
    }
    fn state(&mut self, chat: &str, max: f64) -> Option<&mut ChatState> {
        if self.get(chat).is_none() {
            if self.chats.len() as f64 >= max {
                return None;
            }
            self.chats.push((chat.into(), ChatState::default()));
        }
        self.get_mut(chat)
    }
}
pub struct Engine {
    pub config: Config,
    pub store: Arc<Mutex<Store>>,
    pub orientation: GroupOrientation,
    provider: Arc<dyn OrientationProvider>,
    transport: Arc<dyn EngineTransport>,
    options: Options,
    media_config: media_select::Config,
    collector: Option<crate::media::Collector>,
    core: Mutex<Core>,
    aborted: watch::Sender<bool>,
    // 多个 stop/wait_idle 调用者不能各自拿走任务后提前报告空闲。
    joining: tokio::sync::Mutex<()>,
}
struct CycleStart {
    version: u64,
    id: String,
    now: f64,
}
struct Turn {
    chat: String,
    trigger: String,
    version: u64,
    id: String,
    now: f64,
    activity_started: Option<f64>,
    orientation_epoch: Option<i64>,
    profile: Value,
    hint: Hint,
    history: Vec<Value>,
    last: Value,
    counts: Value,
    query: String,
    learn_now: bool,
    payload: Value,
}
impl Engine {
    pub fn new(
        config: Config,
        store: Arc<Mutex<Store>>,
        provider: Arc<dyn OrientationProvider>,
        transport: Arc<dyn EngineTransport>,
        options: Options,
    ) -> Result<Arc<Self>> {
        Self::new_with_media(
            config,
            store,
            provider,
            transport,
            options,
            Default::default(),
            Default::default(),
        )
    }
    /// Rust 独立配置入口，不改变既有 config 序列化与默认 parity 路径。
    pub fn new_with_media(
        config: Config,
        store: Arc<Mutex<Store>>,
        provider: Arc<dyn OrientationProvider>,
        transport: Arc<dyn EngineTransport>,
        options: Options,
        media_config: media_select::Config,
        collection: crate::media::Config,
    ) -> Result<Arc<Self>> {
        media_config.validate()?;
        if config.agent.emoji.learn_frequency || config.agent.emoji.face_only {
            crate::humanize::enable(
                &*store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("store_poisoned"))?,
            )?;
        }
        if media_config.enabled {
            media_select::enable(
                &*store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("store_poisoned"))?,
            )?;
        }
        let collector = collection
            .enabled
            .then(|| crate::media::Collector::new(&config.data_dir, collection));
        let (aborted, signal) = watch::channel(false);
        let orientation = GroupOrientation::new(
            store.clone(),
            config.agent.clone(),
            provider.clone(),
            Arc::new(OrientationAdapter(transport.clone())),
            options.now.clone(),
            signal,
        )?;
        Ok(Arc::new(Self {
            config,
            store,
            provider,
            transport,
            options,
            media_config,
            collector,
            orientation,
            core: Mutex::new(Core::default()),
            aborted,
            joining: tokio::sync::Mutex::new(()),
        }))
    }
    fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().expect("engine core poisoned")
    }
    fn db(&self) -> Result<MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| anyhow::anyhow!("store_poisoned"))
    }
    fn now(&self) -> f64 {
        (self.options.now)()
    }
    fn snapshot(&self, db: &Store, now: f64) -> Result<ActivitySnapshot> {
        let a = &self.config.agent;
        // ActivityRhythm 的状态在 Store，短生命周期视图不重置抽样或块起点。
        ActivityRhythm::new(db, &a.schedule, &a.rhythm, || {
            (self.options.activity_random)()
        })
        .snapshot(now)
    }
    // 持久化成功后转发同一份决策字段，控制服务仅订阅日志，不重跑决策。
    fn record_decision(
        &self,
        db: &Store,
        chat: &str,
        action: &str,
        score: f64,
        tags: &Value,
        ts: f64,
    ) -> Result<()> {
        db.decision(chat, action, score, tags, ts)?;
        (self.options.log)(
            "decision",
            json!({"chat":chat,"action":action,"score":score,"tags":tags,"ts":ts}),
        );
        Ok(())
    }
    pub fn available(&self, now: f64) -> Result<bool> {
        Ok(self.snapshot(&*self.db()?, now)?.active)
    }
    pub fn chats(&self) -> Vec<(String, ChatState)> {
        self.core().chats.clone()
    }
    pub fn state(&self, chat: &str) -> Option<ChatState> {
        self.core()
            .state(chat, self.config.agent.max_active_chats)
            .cloned()
    }
    pub fn last_error(&self) -> Option<String> {
        self.core().last_error.clone()
    }
    pub fn last_cycle(&self) -> f64 {
        self.core().last_cycle
    }
    pub fn ingest(&self, event: &Value) -> Result<()> {
        let mut core = self.core();
        let a = &self.config.agent;
        let now = self.now();
        let self_id = self.transport.self_id();
        if a.observation.enabled
            && event["post_type"] == "notice"
            && event["notice_type"] == "group_increase"
            && (event["self_id"].is_null() || js_string(&event["self_id"]) == self_id)
            && js_string(&event["user_id"]) == self_id
            && a.allowed_groups.contains(&js_string(&event["group_id"]))
        {
            let chat = format!("group:{}", js_string(&event["group_id"]));
            let previous = self.orientation.get(&chat)?.map(|r| r.epoch);
            let ts = crate::onebot::js_number(&event["time"]);
            self.orientation
                .joined(&chat, if ts == 0. || ts.is_nan() { now } else { ts })?;
            if previous != self.orientation.get(&chat)?.map(|r| r.epoch) {
                if let Some(s) = core.get_mut(&chat) {
                    s.version += 1;
                    s.pending = false;
                    s.pause_done = true;
                }
            }
            return Ok(());
        }
        if !self.available(now)? {
            return Ok(());
        }
        let Some(m) = policy::normalize(event, &self_id, a, now) else {
            return Ok(());
        };
        let Some(s) = core.state(&m.chat, a.max_active_chats) else {
            return Ok(());
        };
        let value = serde_json::to_value(&m)?;
        if !self.db()?.message(&value)? {
            return Ok(());
        }
        if a.emoji.learn_frequency {
            crate::humanize::capture(&*self.db()?, &m.chat, &m.id, event)?;
        }
        if let Some(collector) = &self.collector {
            let report = collector.ingest(&*self.db()?, event, &self_id, a, now)?;
            for code in report.failures {
                (self.options.log)("media_collect_failed", json!({"code":code}));
            }
        }
        self.orientation.observe(&m.chat)?;
        let db = self.db()?;
        if a.learning.enabled {
            LayeredMemory::new(&db).capture(&value, now, &a.memory)?;
        }
        db.observe(&value, now)?;
        // 只有去重成功的新消息递增 version；批内 self 优先于后续开放消息。
        s.version += 1;
        s.last_human = now;
        s.last_id = m.id;
        if !(s.pending && s.hint == Hint::SelfChat) {
            s.hint = m.hint;
        }
        s.pending = true;
        s.pause_done = false;
        s.due = now + a.debounce_seconds;
        Ok(())
    }
    pub fn restore(&self) -> Result<()> {
        let mut core = self.core();
        let db = self.db()?;
        for chat in db.active_chats(self.now() - self.config.agent.active_window_seconds)? {
            if !policy::allowed(&chat, &self.config.agent) {
                continue;
            }
            let Some(s) = core.state(&chat, self.config.agent.max_active_chats) else {
                break;
            };
            if let Some(last) = db
                .history(&chat, None)?
                .iter()
                .rev()
                .find(|m| !truthy(&m["self"]))
            {
                s.last_human = num(last, "ts");
                s.last_id = text(last, "id").into();
                s.pause_done = true;
            }
        }
        Ok(())
    }
    /// stop 后把仍允许的状态交给新 Engine；不启动 revision 监视器（P6b）。
    pub fn inherit_chats(&self, chats: Vec<(String, ChatState)>) {
        let mut core = self.core();
        for (chat, mut state) in chats {
            if policy::allowed(&chat, &self.config.agent) {
                state.busy = false;
                state.last_think = 0.;
                core.chats.push((chat, state));
            }
        }
    }
    async fn media_cycle(&self, chat: &str, start: CycleStart) -> Result<()> {
        // 沿用观察准入、单群单飞、版本、主动冷却和配额；群作息不套用 global quiet。
        let a = &self.config.agent;
        {
            let mut core = self.core();
            if let Some(s) = core.get_mut(chat) {
                s.last_think = self.now();
                s.due = self.now() + 60.;
            }
            let db = self.db()?;
            media_select::observe(&db, chat, self.now(), &self.media_config)?;
        }
        let mut abort = self.aborted.subscribe();
        let oriented = tokio::select! { biased; _=abort.changed()=>return Ok(()), r=self.orientation.before_speak(chat)=>r? };
        if !oriented {
            return Ok(());
        }
        let (selection, segment, delivery) = {
            let core = self.core();
            let db = self.db()?;
            let now = self.now();
            if *self.aborted.borrow()
                || core
                    .get(chat)
                    .is_none_or(|s| s.version != start.version || s.pending)
                || !policy::allowed(chat, a)
                || !a.proactive
                || !self.snapshot(&db, now)?.active
            {
                return Ok(());
            }
            let counts = db.counts(chat, now)?;
            if num(&counts, "total") >= a.max_messages_per_hour
                || num(&counts, "proactive") >= a.max_proactive_per_hour
                || now - num(&counts, "last") < a.proactive_cooldown_seconds
            {
                return Ok(());
            }
            let Some(selection) = media_select::select(
                &db,
                chat,
                now,
                &self.media_config,
                (self.options.selection_random)(),
            )?
            else {
                return Ok(());
            };
            let segment = selection.candidate.segment(&self.config.data_dir)?;
            if a.dry_run {
                (self.options.log)(
                    "media_dry_run",
                    json!({"chat":chat,"hash":selection.candidate.hash}),
                );
                return Ok(());
            }
            let transport = self.transport.state();
            if !transport.connected || !transport.online {
                return Ok(());
            }
            let delivery = db.delivery(chat, true, now)?;
            (selection, segment, delivery)
        };
        let sent = self.transport.send_media(chat, segment).await;
        let _core = self.core();
        let db = self.db()?;
        match sent {
            Ok(sent) => {
                db.finish_delivery(&delivery, "sent", sent.get("message_id"))?;
                let id = sent
                    .get("message_id")
                    .filter(|v| !v.is_null())
                    .map(js_string)
                    .unwrap_or(delivery);
                let now = self.now();
                db.message(&json!({"chat":chat,"id":id,"sender":self.transport.self_id(),"name":a.name.text,"text":"","ts":now,"self":true}))?;
                db.execute(
                    "INSERT OR IGNORE INTO media_pending VALUES(?,?,?,?,?,?,?,?)",
                    rusqlite::params![
                        chat,
                        id,
                        selection.candidate.source_chat,
                        selection.candidate.hash,
                        selection.bucket,
                        now,
                        serde_json::to_string(&selection.classification)?,
                        selection.wild as i32
                    ],
                )?;
                (self.options.log)(
                    "media_sent",
                    json!({"chat":chat,"hash":selection.candidate.hash}),
                );
            }
            Err(e) => {
                db.finish_delivery(
                    &delivery,
                    if e.uncertain { "uncertain" } else { "failed" },
                    None,
                )?;
            }
        }
        Ok(())
    }
    pub fn tick(self: &Arc<Self>) -> Result<()> {
        let mut core = self.core();
        let now = self.now();
        let a = &self.config.agent;
        if !self.available(now)? {
            for (_, s) in &mut core.chats {
                s.version += 1;
                s.pending = false;
                s.pause_done = true;
            }
            return Ok(());
        }
        core.chats
            .retain(|(_, s)| s.busy || now - s.last_human <= a.active_window_seconds);
        let transport = self.transport.state();
        if !readiness(&self.config).is_empty()
            || !transport.connected
            || !transport.online
            || *self.aborted.borrow()
        {
            return Ok(());
        }
        core.tasks.retain(|t| !t.is_finished());
        let mut running = core.chats.iter().filter(|(_, s)| s.busy).count();
        let mut ready = Vec::new();
        for (chat, s) in &mut core.chats {
            if running as f64 >= a.max_concurrent_chats {
                break;
            }
            if s.busy || now - s.last_human > a.active_window_seconds || now < s.due {
                continue;
            }
            if now - s.last_think < a.min_think_interval_seconds && s.hint != Hint::SelfChat {
                continue;
            }
            let trigger = if s.pending {
                "message"
            } else if self.media_config.enabled && chat.starts_with("group:") {
                "media"
            } else if !s.pause_done && now - s.last_human >= a.pause_seconds {
                "pause"
            } else {
                continue;
            };
            let quiet = trigger != "media" && policy::quiet(now, a.quiet_hours.as_ref());
            if trigger == "pause" && (!a.proactive || quiet) {
                continue;
            }
            if s.hint != Hint::SelfChat && (!a.proactive || quiet) {
                s.pending = false;
                s.pause_done = true;
                continue;
            }
            // 置 busy 和占用名额在同一锁内，任务启动前就已完成同 chat 单飞保护。
            s.busy = true;
            running += 1;
            // JS 调用 async cycle 时，在第一次 await 前已捕获版本。
            // Tokio spawn 延迟首轮 poll，因此必须在准入锁内捕获，不能吞掉新消息的 debounce。
            ready.push((
                chat.clone(),
                trigger,
                CycleStart {
                    version: s.version,
                    id: s.last_id.clone(),
                    now: self.now(),
                },
            ));
        }
        for (chat, trigger, start) in ready {
            let engine = self.clone();
            core.tasks.push(tokio::spawn(async move {
                let result = if trigger == "media" {
                    engine.media_cycle(&chat, start).await
                } else {
                    engine.cycle(&chat, trigger, start).await
                };
                let mut core = engine.core();
                if let Err(error) = result {
                    let code = error.to_string();
                    core.last_error = Some(code.clone());
                    (engine.options.log)("cycle_error", json!({"chat":chat,"code":code}));
                    if let Some(s) = core.get_mut(&chat) {
                        s.due = engine.now() + 60.;
                    }
                }
                if let Some(s) = core.get_mut(&chat) {
                    s.busy = false;
                }
            }));
        }
        Ok(())
    }
    pub async fn wait_idle(&self) {
        let _joining = self.joining.lock().await;
        loop {
            let tasks = { std::mem::take(&mut self.core().tasks) };
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                task.await.expect("engine task panicked");
            }
        }
    }
    pub async fn stop(&self) {
        {
            let _core = self.core();
            self.aborted.send_replace(true);
        }
        self.wait_idle().await;
    }
    async fn model(&self, system: &str, payload: Value) -> Result<Value> {
        let mut signal = self.aborted.subscribe();
        ensure!(!*signal.borrow(), "aborted");
        // 只能丢弃 future/结果；Provider 的 spawn_blocking 请求仍会跑完，预算绝不退还。
        tokio::select! { biased;
            _ = signal.changed() => anyhow::bail!("aborted"),
            result = self.provider.json(system, payload) => result,
        }
    }
    fn obsolete(&self, core: &Core, db: &Store, t: &Turn) -> Result<bool> {
        // 五个触发源：① stop/热重载；② 新消息、重新入群、休息 tick；
        // ③ 活动块到期/时钟回拨/签名变化；④ reset_learning；⑤ orientation 重新入群。
        Ok(*self.aborted.borrow()
            || core.get(&t.chat).is_none_or(|s| s.version != t.version)
            || self.snapshot(db, self.now())?.started != t.activity_started
            || db.learning_state(&t.chat)?["epoch"] != t.profile["epoch"]
            || db.orientation_state(&t.chat)?.map(|r| r.epoch) != t.orientation_epoch)
    }
    fn fresh(&self, core: &Core, db: &Store, t: &Turn) -> Result<bool> {
        Ok(!self.obsolete(core, db, t)? && self.snapshot(db, self.now())?.active)
    }
    fn finish(&self, core: &mut Core, db: &Store, t: &Turn, sent: bool) -> Result<()> {
        if let Some(s) = core.get_mut(&t.chat).filter(|s| s.version == t.version) {
            s.pending = false;
            s.hint = Hint::Open;
            if t.trigger == "pause" || sent {
                s.pause_done = true;
            }
            db.mark_handled(&t.chat, &t.id, s.pause_done)?;
        }
        Ok(())
    }
    fn context(&self, db: &Store, t: &Turn, payload: &mut Value) -> Result<()> {
        let a = &self.config.agent;
        let sender = text(&t.last, "sender");
        let context = if a.learning.enabled {
            LayeredMemory::new(db).context(&t.chat, sender, t.now, &a.memory, &t.query)?
        } else {
            vec![]
        };
        payload["chatStyle"] = json!(context.iter().map(|scope| json!({"subject":scope["subject"],"traits":array(&scope["traits"]).iter().map(|m|json!({"key":m["slot"],"text":m["text"],"sources":m["sources"]})).collect::<Vec<_>>()})).collect::<Vec<_>>());
        payload["memoryContext"] = json!(context);
        payload["memories"] = json!(db.retrieve_scoped(
            &t.chat,
            sender,
            &t.query,
            t.now,
            &a.memory,
            &ScopedOptions {
                enabled: Some(a.learning.enabled),
                exclude_ids: t.history.iter().map(|m| text(m, "id").into()).collect(),
                limit: Some(a.learning.retrieval_limit as usize)
            }
        )?);
        payload["expressions"] = json!(if a.learning.enabled {
            ExpressionMemory::new(db).context(
                &t.chat,
                sender,
                &t.query,
                t.now,
                &a.expression,
                &a.memory,
            )?
        } else {
            vec![]
        });
        Ok(())
    }
    fn reservoir(&self, db: &Store, t: &Turn) -> Result<Vec<Value>> {
        let a = &self.config.agent;
        db.reservoir(
            &t.chat,
            t.now,
            a.thought_ttl_seconds,
            a.thought_limit as i64,
            Some(text(&t.last, "sender")),
        )
    }
    async fn cycle(&self, chat: &str, trigger: &str, start: CycleStart) -> Result<()> {
        let a = &self.config.agent;
        let CycleStart { version, id, now } = start;
        // 1–2：入口守卫；版本取自 tick 准入时的快照，等价 JS 首个 await 之前。
        {
            let core = self.core();
            if core.get(chat).is_none() || !policy::allowed(chat, a) || !self.available(now)? {
                return Ok(());
            }
        }
        // 3：观察闸门失败只推迟 5 秒；等待过程中允许 ingest。
        let mut signal = self.aborted.subscribe();
        if *signal.borrow() {
            return Ok(());
        }
        let oriented = tokio::select! { biased;
            _ = signal.changed() => return Ok(()),
            result = self.orientation.before_speak(chat) => result?,
        };
        let mut t = {
            let mut core = self.core();
            if !oriented {
                if let Some(s) = core.get_mut(chat) {
                    s.due = self.now() + 5.;
                }
                return Ok(());
            }
            // 4–10：捕获五项失效凭证；assessment 防止已处理消息重入。
            if *self.aborted.borrow()
                || core.get(chat).is_none_or(|s| s.version != version)
                || !self.available(self.now())?
            {
                return Ok(());
            }
            let orientation_profile = self.orientation.profile(chat)?;
            let db = self.db()?;
            let mut t = Turn {
                chat: chat.into(),
                trigger: trigger.into(),
                version,
                id,
                now,
                activity_started: self.snapshot(&db, self.now())?.started,
                orientation_epoch: db.orientation_state(chat)?.map(|r| r.epoch),
                profile: Value::Null,
                hint: Hint::Open,
                history: vec![],
                last: Value::Null,
                counts: Value::Null,
                query: String::new(),
                learn_now: false,
                payload: json!({}),
            };
            if a.sending.enabled && db.assessment(chat, &t.id)?.is_some() {
                self.finish(&mut core, &db, &t, true)?;
                return Ok(());
            }
            t.hint = if trigger == "pause" {
                Hint::Open
            } else {
                core.get(chat).unwrap().hint
            };
            core.get_mut(chat).unwrap().last_think = now;
            core.last_cycle = now;
            t.profile = db.learning_state(chat)?;
            // 11–16：历史、最后人类消息、小时配额、学习门控、检索 query。
            t.history = db.history(
                chat,
                Some(if a.learning.enabled {
                    a.history_limit.max(a.learning.min_messages)
                } else {
                    a.history_limit
                } as i64),
            )?;
            let humans: Vec<_> = t.history.iter().filter(|m| !truthy(&m["self"])).collect();
            let Some(last) = humans.last() else {
                return Ok(());
            };
            t.last = (*last).clone();
            t.counts = db.counts(chat, now)?;
            if num(&t.counts, "total") >= a.max_messages_per_hour
                || (t.hint != Hint::SelfChat
                    && (num(&t.counts, "proactive") >= a.max_proactive_per_hour
                        || now - num(&t.counts, "last") < a.proactive_cooldown_seconds))
            {
                self.finish(&mut core, &db, &t, false)?;
                return Ok(());
            }
            let new_start = humans
                .iter()
                .rposition(|m| m["id"] == t.profile["last_id"])
                .map_or(0, |i| i + 1);
            t.learn_now = a.learning.enabled
                && t.last["id"] != t.profile["last_id"]
                && (humans.len() - new_start) as f64 >= a.learning.min_messages
                && now - num(&t.profile, "updated") >= a.learning.interval_seconds;
            t.query = humans[humans.len().saturating_sub(3)..]
                .iter()
                .map(|m| text(m, "text"))
                .collect::<Vec<_>>()
                .join(" ");
            // 17–18：复用 memory/ranking/expression/store 的上下文实现。
            let mut payload = json!({"personality":personality_context(a,|| (self.options.expression_random)()),"persona":a.persona.text,"name":a.name.text,"trigger":trigger,"addressedHint":t.hint,"groupOrientation":orientation_profile,
                "history":t.history.iter().map(|m|json!({"id":m["id"],"sender":m["sender"],"self":truthy(&m["self"]),"timestamp":m["ts"],"speaker":if truthy(&m["self"]) {a.name.text.as_str()} else {text(m,"name")},"text":m["text"]})).collect::<Vec<_>>(),
                "retainedIdeas":self.reservoir(&db,&t)?,"priorExpectation":db.expectation(chat,now)?});
            self.context(&db, &t, &mut payload)?;
            payload["learning"] = json!({"requested":t.learn_now,"subjects":array(&payload["memoryContext"]).iter().map(|s|s["subject"].clone()).collect::<Vec<_>>(),"currentSpeaker":t.last["sender"],"learnExpressions":a.expression.learn});
            t.payload = payload;
            t
        };
        // 19–20：形成候选；先校验再检查过期，保持 JS 错误/副作用顺序。
        let formed = self.model(prompts::FORMATION, t.payload.clone()).await?;
        ensure!(
            formed["candidates"].is_array()
                && Allocation::parse(text(&formed, "allocation")).is_some(),
            "invalid_formation"
        );
        let candidates = {
            let mut core = self.core();
            let db = self.db()?;
            if !self.fresh(&core, &db, &t)? {
                return Ok(());
            }
            // 21：可选学习失败只记录，不中止候选流程。
            if t.learn_now && formed.get("learning").is_some() {
                let learned = (|| -> Result<Option<usize>> {
                    ensure!(!formed["learning"].is_null(), "invalid_learning");
                    let learning = &formed["learning"];
                    let empty = json!([]);
                    let updates = parse_memory_updates(
                        if learning["layers"].is_null() {
                            &empty
                        } else {
                            &learning["layers"]
                        },
                        &t.history,
                        chat,
                        text(&t.last, "sender"),
                        &a.memory,
                    )?;
                    let expressions = if a.expression.learn {
                        parse_expressions(
                            if learning["expressions"].is_null() {
                                &empty
                            } else {
                                &learning["expressions"]
                            },
                            &t.history,
                            chat,
                            text(&t.last, "sender"),
                        )?
                    } else {
                        vec![]
                    };
                    Ok(db
                        .learn(
                            chat,
                            &json!({"style":null,"memories":[],"forgetIds":[]}),
                            now,
                            text(&t.last, "id"),
                            &a.learning,
                            t.profile["epoch"].as_i64().unwrap_or(0),
                            Some(LayeredUpdate {
                                updates: &updates,
                                settings: &a.memory,
                                expressions: Some(&expressions),
                                expression_settings: &a.expression,
                            }),
                        )?
                        .then_some(updates.len()))
                })();
                match learned {
                    Ok(Some(count)) => {
                        let mut payload = t.payload.clone();
                        self.context(&db, &t, &mut payload)?;
                        t.payload = payload;
                        (self.options.log)(
                            "chat_learning_updated",
                            json!({"chat":chat,"updates":count}),
                        );
                    }
                    Err(_) => (self.options.log)("chat_learning_rejected", json!({"chat":chat})),
                    _ => {}
                }
            }
            // 22–23：候选去重/保留；与 normalize 一样，非 BMP 截断按 Unicode 字符。
            for candidate in array(&formed["candidates"]).iter().take(3) {
                if !candidate["text"].is_string()
                    || policy::js_trim(text(candidate, "text")).is_empty()
                    || CandidateKind::parse(text(candidate, "kind")).is_none()
                {
                    continue;
                }
                let content = policy::clip_chars(policy::js_trim(text(candidate, "text")), 300);
                if !self
                    .reservoir(&db, &t)?
                    .iter()
                    .any(|c| c["text"] == content)
                {
                    db.add_thought(chat,&json!({"text":content,"kind":candidate["kind"],"subject":t.last["sender"]}),now)?;
                }
            }
            if !self.fresh(&core, &db, &t)? {
                return Ok(());
            }
            let candidates = self.reservoir(&db, &t)?;
            if candidates.is_empty() {
                self.finish(&mut core, &db, &t, false)?;
                return Ok(());
            }
            candidates
        };
        // 24–26：评价、严格评分过滤、写回评分（JS 在过期检查前回写）。
        let mut payload = t.payload.clone();
        payload.as_object_mut().unwrap().remove("retainedIdeas");
        payload["candidates"] = json!(candidates);
        payload["recentAgentMessages"] = t.counts["total"].clone();
        let result = self.model(prompts::EVALUATION, payload).await?;
        let (selected, proactive, timing) = {
            let mut core = self.core();
            let db = self.db()?;
            let rated = ratings(&result, &candidates)?;
            for r in &rated {
                db.score(&r.id, r.motivation)?;
            }
            if !self.fresh(&core, &db, &t)? {
                return Ok(());
            }
            // 27–30：显式点名优先；模型判 self 不能绕过主动配额或静默。
            let allocation = match t.hint {
                Hint::SelfChat => Allocation::SelfChat,
                Hint::Other => Allocation::Other,
                Hint::Open => Allocation::parse(text(&formed, "allocation")).unwrap(),
            };
            let start = t
                .history
                .iter()
                .rposition(|m| truthy(&m["self"]))
                .map_or(0, |i| i + 1);
            let silent = t.history[start..]
                .iter()
                .filter(|m| !truthy(&m["self"]))
                .count();
            let selected = policy::select(&rated, allocation, a, silent as f64, || {
                (self.options.selection_random)()
            });
            let proactive = t.hint != Hint::SelfChat;
            if selected.is_none()
                || (proactive && (!a.proactive || policy::quiet(now, a.quiet_hours.as_ref())))
            {
                self.record_decision(
                    &db,
                    chat,
                    "withhold",
                    selected.as_ref().map_or(0., |s| s.adjusted),
                    &json!([]),
                    now,
                )?;
                self.finish(&mut core, &db, &t, false)?;
                return Ok(());
            }
            let selected = selected.unwrap();
            // 31：仅开启 sending 时读取发送时序。
            let timing = if a.sending.enabled {
                let time = self.now();
                let timing = db.sending_timing(chat, time, a.sending.recovery_seconds)?;
                Some(Timing {
                    proactive,
                    age: (time - core.get(chat).unwrap().last_human).max(0.),
                    gap: num(&timing, "gap"),
                    recent_humans: num(&timing, "recentHumans"),
                    score: selected.adjusted,
                })
            } else {
                None
            };
            (selected, proactive, timing)
        };
        let prediction = if let Some(timing) = timing {
            // 32–35：预测、抽签、持久化 assessment 后才准入生成。
            let mut payload = t.payload.clone();
            payload.as_object_mut().unwrap().remove("retainedIdeas");
            payload["selectedIdea"] = json!(selected.candidate.text);
            payload["timing"] = json!(timing);
            let prediction = forecast_result(&self.model(prompts::FORECAST, payload).await?)?;
            let mut core = self.core();
            let db = self.db()?;
            if !self.fresh(&core, &db, &t)? {
                return Ok(());
            }
            let settings = SendingSettings {
                proactive_probability: a.sending.proactive_probability,
                addressed_probability: a.sending.addressed_probability,
                settle_seconds: a.sending.settle_seconds,
                recovery_seconds: a.sending.recovery_seconds,
                burst_scale: a.sending.burst_scale,
                max_negative_probability: a.sending.max_negative_probability,
            };
            let gate = sending_probability(&settings, &timing, &prediction);
            let draw = (self.options.random)();
            let admitted = gate.veto.is_none() && draw < gate.probability;
            let mut details = serde_json::to_value(gate)?;
            details["draw"] = json!(draw);
            details["timing"] = json!(timing);
            details["prediction"] = json!(prediction);
            db.assess(
                chat,
                &t.id,
                self.now(),
                if admitted { "admitted" } else { "withheld" },
                &details,
            )?;
            (self.options.log)(
                "send_assessment",
                json!({"chat":chat,"probability":gate.probability,"admitted":admitted}),
            );
            if !admitted {
                let veto = serde_json::to_value(gate.veto)?;
                self.record_decision(
                    &db,
                    chat,
                    veto.as_str().unwrap_or("probability_withhold"),
                    selected.adjusted,
                    &json!([]),
                    self.now(),
                )?;
                self.finish(&mut core, &db, &t, true)?;
                return Ok(());
            }
            Some(prediction)
        } else {
            None
        };
        // 36–37：P6c parity：与 JS 一样先装饰抽样，再复用已有长度抽样，绝不加开关。
        let decorations = {
            let _core = self.core();
            decoration_choices(&*self.db()?, chat, self.now(), &a.emoji, || {
                (self.options.expression_random)()
            })?
        };
        let length_target = policy::pick_length_target(
            if t.hint == Hint::SelfChat {
                "self"
            } else {
                "open"
            },
            || (self.options.expression_random)(),
        );
        let face_only_allowed = a.emoji.face_only
            && length_target != "long"
            && crate::humanize::face_only_allowed(
                &*self.db()?,
                chat,
                t.hint,
                selected.candidate.motivation,
                &t.history,
                self.now(),
                a.emoji.cooldown_seconds,
            )?;
        let mut payload = json!({"lengthTarget":length_target,"decorations":decorations,"persona":a.persona.text,"name":a.name.text,"selectedIdea":selected.candidate.text,"responsePlan":prediction,"assertiveTone":a.proactive_tone,"maxCharacters":a.max_output_chars});
        for key in [
            "personality",
            "expressions",
            "history",
            "groupOrientation",
            "chatStyle",
            "memories",
            "memoryContext",
            "priorExpectation",
        ] {
            payload[key] = t.payload[key].clone();
        }
        // 门控运行时片段进入 user JSON；绝不修改生成产物 prompts.rs 或它的 parity 断言。
        if a.emoji.face_only {
            payload["runtimeInstructions"] = json!(crate::humanize::FACE_ONLY_INSTRUCTIONS);
            payload["faceOnlyAllowed"] = json!(face_only_allowed);
        }
        let system = prompts::articulation_for(&a.reply_language).map_err(anyhow::Error::msg)?;
        let response = match self.model(&system, payload).await {
            Ok(value) => value,
            Err(error) => {
                self.db()?
                    .assessment_status(chat, &t.id, "generation_failed")?;
                return Err(error);
            }
        };
        let (delivery_id, decorated) = {
            let mut core = self.core();
            let db = self.db()?;
            // 38–43：正文、作废、离线、重复/静默、dry-run，顺序不可换。
            let raw = text(&response, "text");
            let lower = raw.to_ascii_lowercase();
            let face_only = policy::js_trim(raw).is_empty();
            let decorated = decorate(&response, &decorations, a.max_output_chars as usize);
            if !response["text"].is_string()
                || (face_only
                    && !(face_only_allowed
                        && decorated["faceId"].is_string()
                        && (response["emoji"].is_null() || response["emoji"] == "")
                        && text(&decorated, "text").is_empty()))
                || ["<think>", "</think>", "<analysis>", "</analysis>"]
                    .iter()
                    .any(|tag| lower.contains(tag))
            {
                db.assessment_status(chat, &t.id, "generation_failed")?;
                anyhow::bail!("invalid_articulation");
            }
            if self.obsolete(&core, &db, &t)?
                || self.now() - core.get(chat).unwrap().last_human > a.active_window_seconds
                || !self.snapshot(&db, self.now())?.active
            {
                db.assessment_status(chat, &t.id, "cancelled")?;
                return Ok(());
            }
            let transport = self.transport.state();
            if !transport.connected || !transport.online {
                db.assessment_status(chat, &t.id, "cancelled")?;
                anyhow::bail!("qq_offline");
            }
            let own = t
                .history
                .iter()
                .filter(|m| truthy(&m["self"]))
                .map(|m| text(m, "text").to_owned())
                .collect::<Vec<_>>();
            if (proactive && policy::quiet(self.now(), a.quiet_hours.as_ref()))
                || (!face_only && policy::repeated(text(&decorated, "text"), &own))
            {
                db.assessment_status(chat, &t.id, "cancelled")?;
                db.r#use(&selected.candidate.id)?;
                self.finish(&mut core, &db, &t, false)?;
                return Ok(());
            }
            if a.dry_run {
                db.assessment_status(chat, &t.id, "dry_run")?;
                let tags: Vec<_> = selected
                    .candidate
                    .for_tags
                    .iter()
                    .chain(&selected.candidate.against_tags)
                    .collect();
                self.record_decision(&db, chat, "dry_run", selected.adjusted, &json!(tags), now)?;
                db.r#use(&selected.candidate.id)?;
                self.finish(&mut core, &db, &t, false)?;
                (self.options.log)("dry_run", json!({"chat":chat,"score":selected.adjusted}));
                return Ok(());
            }
            // 44–45：先落库再发送；崩溃/超时留下 pending/uncertain，绝不重放。
            let delivery_id = db.delivery(chat, proactive, self.now())?;
            if a.emoji.face_only && face_only {
                db.execute(
                    "INSERT OR REPLACE INTO humanize_reply_state VALUES(?,1)",
                    [chat],
                )?;
            }
            db.r#use(&selected.candidate.id)?;
            self.finish(&mut core, &db, &t, true)?;
            (delivery_id, decorated)
        };
        // 46：与 JS 一样，发送不受模型取消信号中断；stop 等待实际投递结果。
        let content = text(&decorated, "text");
        let sent = self
            .transport
            .send(chat, content, decorated["faceId"].as_str())
            .await;
        let mut core = self.core();
        let db = self.db()?;
        match sent {
            Ok(sent) => {
                // 47：成功后记表达使用、预期与自身消息。
                if a.emoji.face_only && !content.is_empty() {
                    db.execute(
                        "INSERT OR REPLACE INTO humanize_reply_state VALUES(?,0)",
                        [chat],
                    )?;
                }
                db.finish_delivery(&delivery_id, "sent", sent.get("message_id"))?;
                ExpressionMemory::new(&db).used(
                    chat,
                    array(&t.payload["expressions"]),
                    content,
                    self.now(),
                )?;
                if truthy(&decorated["decorated"]) {
                    db.execute(
                        "INSERT OR REPLACE INTO decoration_usage VALUES(?,?)",
                        rusqlite::params![chat, self.now()],
                    )?;
                }
                db.assessment_status(chat, &t.id, "sent")?;
                if let Some(prediction) = prediction {
                    db.expect(
                        chat,
                        self.now(),
                        a.sending.expectation_seconds,
                        &json!(prediction),
                    )?;
                }
                let id = if sent["message_id"].is_null() {
                    delivery_id
                } else {
                    js_string(&sent["message_id"])
                };
                db.message(&json!({"chat":chat,"id":id,"sender":self.transport.self_id(),"name":a.name.text,"text":content,"ts":self.now(),"self":true}))?;
                self.record_decision(
                    &db,
                    chat,
                    "sent",
                    selected.adjusted,
                    &json!(selected.candidate.for_tags),
                    self.now(),
                )?;
                core.last_error = None;
                (self.options.log)(
                    "message_sent",
                    json!({"chat":chat,"proactive":proactive,"lengthTarget":length_target}),
                );
            }
            Err(error) => {
                // 48：确定拒绝和不确定送达都只记账，不设置自动重试。
                if a.emoji.face_only && content.is_empty() && !error.uncertain {
                    db.execute("DELETE FROM humanize_reply_state WHERE chat=?", [chat])?;
                }
                let status = if error.uncertain {
                    "uncertain"
                } else {
                    "failed"
                };
                db.finish_delivery(&delivery_id, status, None)?;
                db.assessment_status(chat, &t.id, status)?;
                self.record_decision(
                    &db,
                    chat,
                    if error.uncertain {
                        "delivery_uncertain"
                    } else {
                        "delivery_failed"
                    },
                    selected.adjusted,
                    &json!([]),
                    self.now(),
                )?;
                core.last_error = Some(error.code.clone());
                (self.options.log)("delivery_error", json!({"chat":chat,"code":error.code}));
            }
        }
        Ok(())
    }
}
fn ratings(result: &Value, candidates: &[Value]) -> Result<Vec<Candidate>> {
    ensure!(result["ratings"].is_array(), "invalid_evaluation");
    let mut seen = HashSet::new();
    let mut rated = Vec::new();
    for r in array(&result["ratings"]) {
        let Some(c) = candidates.iter().find(|c| c["id"] == r["id"]) else {
            continue;
        };
        if seen.contains(text(r, "id"))
            || !["motivation", "relevance", "originality"].iter().all(|k| {
                r[k].as_f64()
                    .is_some_and(|n| n.is_finite() && (1.0..=5.0).contains(&n))
            })
        {
            continue;
        }
        seen.insert(text(r, "id").to_owned());
        let tags = |key: &str| {
            array(&r[key])
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| {
                    [
                        "relevance",
                        "information_gap",
                        "expected_impact",
                        "urgency",
                        "coherence",
                        "originality",
                        "balance",
                        "dynamics",
                    ]
                    .contains(s)
                })
                .take(2)
                .map(str::to_owned)
                .collect()
        };
        rated.push(Candidate {
            id: text(c, "id").into(),
            kind: CandidateKind::parse(text(c, "kind")).unwrap_or(CandidateKind::System2),
            text: text(c, "text").into(),
            motivation: num(r, "motivation"),
            relevance: num(r, "relevance"),
            originality: num(r, "originality"),
            for_tags: tags("for"),
            against_tags: tags("against"),
        });
    }
    ensure!(!rated.is_empty(), "invalid_ratings");
    Ok(rated)
}
