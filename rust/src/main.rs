//! src/main.mjs runtime port plus the existing diagnostic commands.
//! Status preserves all 19 JS fields (including raw model types, nulls and seconds).
//! JSON object order/number spelling and atomic_json's trailing newline are not byte-identical.
//! Known runtime difference: Provider blocking HTTP cannot be aborted; shutdown drains its
//! remaining Store owners before closing SQLite. Reload also rechecks the marker/revision
//! around loading and defers an unstable snapshot to the next watcher tick (JS checks only before).
//! P6a differences remain documented in ENGINE.md.
//! 控制套接字与运行时一起启动、重载资源引用并在停止时清理。
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use qq_inner_core::config;
use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "qq-inner-core",
    version,
    about = "Rust core for qq-inner-agent"
)]
struct Cli {
    /// Repository root. Defaults to $QQ_INNER_ROOT, then walks up from the
    /// current directory looking for `config.example.json`.
    #[arg(long, global = true, env = "QQ_INNER_ROOT")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Probe the repository root, configuration, and SQLite, then exit.
    Selftest,
    /// Run the agent until SIGINT, SIGTERM or SIGHUP.
    Start,
    /// Print actual SQLite schema (in memory unless --database is supplied).
    DbSchema {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Print a normalized summary with credentials redacted.
    Config,
    /// Authenticate with the OneBot bridge and report online status.
    Check,
}

fn resolve_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit {
        return Ok(root);
    }
    let cwd = std::env::current_dir().context("read current directory")?;
    for dir in cwd.ancestors() {
        if dir.join("config.example.json").is_file() {
            return Ok(dir.to_path_buf());
        }
    }
    Ok(cwd)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = resolve_root(cli.root)?;

    match cli.command {
        Command::Start => {
            if root.join(".settings-write").exists() || config::load_config(&root).is_err() {
                eprintln!("Invalid configuration. Run ./agent setup to correct it.");
                std::process::exit(2);
            }
            run(root).await?;
        }
        Command::DbSchema { database } => {
            let store = match database {
                Some(path) => qq_inner_core::store::Store::open(path)?,
                None => qq_inner_core::store::Store::in_memory()?,
            };
            println!("{}", serde_json::to_string_pretty(&store.schema()?)?);
        }
        Command::Check => {
            let c = config::load_config(&root)?.config;
            let token = if c.onebot_token.is_truthy {
                c.onebot_token.text
            } else {
                String::new()
            };
            let (bot, _notices) = qq_inner_core::onebot::OneBot::new(c.onebot, token);
            match bot.check().await {
                Ok(state) => {
                    println!(
                        "authentication = ok\nselfId = {}\nonline = {}",
                        state.self_id, state.online
                    );
                    anyhow::ensure!(state.online, "qq_offline");
                }
                Err(e) => {
                    println!("authentication = failed\nonline = false");
                    return Err(e.into());
                }
            }
        }
        Command::Config => {
            let loaded = config::load_config(&root)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&config::summary(&loaded.config))?
            );
        }
        Command::Selftest => {
            // 使用同一配置加载流程；SQLite 仍仅在内存中自检。
            let config = config::load_config(&root)?.raw;
            let conn = Connection::open_in_memory().context("open in-memory sqlite")?;
            let sqlite: String = conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;

            let field = |pointer: &str| {
                config
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .unwrap_or("(unset)")
                    .to_string()
            };

            println!("core version   = {}", env!("CARGO_PKG_VERSION"));
            println!("root           = {}", root.display());
            println!("config.json    = ok");
            println!("sqlite         = {sqlite}");
            println!("provider.kind  = {}", field("/provider/kind"));
            println!("provider.model = {}", field("/provider/model"));
            println!("selftest ok");
        }
    }

    Ok(())
}

use qq_inner_core::{
    activity::ActivityRhythm,
    control::{Backend, Events, LiveRemote, Server},
    engine::{Clock, Engine, Logger, Options},
    expression::ExpressionMemory,
    memory::LayeredMemory,
    onebot::{Notification, OneBot},
    provider::Provider,
    settings,
    store::Store,
};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{interval_at, Instant, Interval},
};

fn iso(now: f64) -> String {
    chrono::DateTime::from_timestamp_millis((now * 1000.).floor() as i64)
        .expect("valid clock")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn logger(dir: PathBuf, now: Clock) -> Logger {
    let lock = Mutex::new(());
    Arc::new(move |event, data| {
        let _guard = lock.lock().unwrap();
        let mut line = json!({"time": iso(now()), "event": event});
        if let Some(fields) = data.as_object() {
            line.as_object_mut().unwrap().extend(fields.clone());
        }
        let line = line.to_string();
        let _ = writeln!(std::io::stdout(), "{line}");
        let file = dir.join("agent.log");
        if fs::metadata(&file).is_ok_and(|m| m.len() > 1_048_576) {
            let _ = fs::rename(&file, dir.join("agent.log.1"));
        }
        let mut options = fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let Ok(mut f) = options.open(file) {
            let _ = writeln!(f, "{line}");
        }
    })
}
fn mode(c: &config::Config) -> &'static str {
    if !config::readiness(c).is_empty() {
        "waiting_for_setup"
    } else if c.agent.dry_run {
        "dry_run"
    } else {
        "active"
    }
}
fn changed(root: &Path, applied: &str) -> Result<Option<String>> {
    if root.join(".settings-write").exists() {
        return Ok(None);
    }
    let next = settings::revision(root)?;
    Ok((next != applied).then_some(next))
}
struct Runtime {
    config: config::Config,
    raw: Value,
    store: Arc<Mutex<Store>>,
    provider: Arc<Provider>,
    bot: Arc<OneBot>,
    notices: mpsc::UnboundedReceiver<Notification>,
    engine: Arc<Engine>,
    connection: Option<JoinHandle<()>>,
    abort: watch::Sender<bool>,
    applied: String,
    reloading: bool,
    reload_error: Option<&'static str>,
    now: Clock,
    log: Logger,
    control: Option<Arc<Backend>>,
}
impl Runtime {
    fn new(
        loaded: config::Loaded,
        store: Arc<Mutex<Store>>,
        applied: String,
        now: Clock,
        log: Logger,
        existing_bot: Option<Arc<OneBot>>,
    ) -> Result<Self> {
        let c = loaded.config;
        let provider = Arc::new(Provider::new(
            c.provider.clone(),
            c.api_key.text.clone(),
            store.clone(),
        ));
        let (bot, notices) = OneBot::new(c.onebot.clone(), c.onebot_token.text.clone());
        let bot = existing_bot.unwrap_or_else(|| Arc::new(bot));
        let engine = Engine::new_with_media(
            c.clone(),
            store.clone(),
            provider.clone(),
            bot.clone(),
            Options {
                now: now.clone(),
                log: log.clone(),
                ..Options::default()
            },
            serde_json::from_value(
                loaded.raw["agent"]
                    .get("mediaSelect")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )?,
            serde_json::from_value(
                loaded.raw["agent"]
                    .get("media")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )?,
        )?;
        let (abort, _) = watch::channel(false);
        Ok(Self {
            control: None,
            config: c,
            raw: loaded.raw,
            store,
            provider,
            bot,
            notices,
            engine,
            connection: None,
            abort,
            applied,
            reloading: false,
            reload_error: None,
            now,
            log,
        })
    }
    fn connect(&mut self) {
        let bot = self.bot.clone();
        let signal = self.abort.subscribe();
        self.connection = Some(tokio::spawn(async move { bot.run(signal).await }));
    }
    async fn notice(&self, notice: Notification) {
        match notice {
            Notification::Status(state) => (self.log)("onebot", json!({"state":state})),
            Notification::Event(event) => {
                if let Some(control) = &self.control {
                    control.observe(&event);
                }
                let event = qq_inner_core::policy::resolve_forwards(
                    &event,
                    &self.bot,
                    &self.config.agent,
                    (self.now)(),
                )
                .await;
                // 入站采集含同步文件/下载 I/O；单个顺序阻塞任务，不阻塞 Tokio worker。
                let engine = self.engine.clone();
                if !matches!(
                    tokio::task::spawn_blocking(move || engine.ingest(&event)).await,
                    Ok(Ok(()))
                ) {
                    (self.log)("event_rejected", json!({}));
                }
            }
        }
    }
    fn status(&self, now: f64, pid: u32) -> Result<Value> {
        let c = &self.config;
        let active = self.engine.available(now)?;
        let db = self.store.lock().unwrap();
        let activity =
            ActivityRhythm::new(&db, &c.agent.schedule, &c.agent.rhythm, rand::random::<f64>)
                .snapshot(now)?;
        drop(db);
        let bot = self.bot.state();
        // Exactly src/main.mjs status(): nulls are retained and cycle timestamps are seconds.
        Ok(json!({"updatedAt":iso(now), "pid":pid, "mode":mode(c),
            "appliedRevision":self.applied, "reloading":self.reloading, "reloadError":self.reload_error,
            "scheduleActive":active, "activityRhythm":activity, "missing":config::readiness(c),
            "onebotConnected":bot.connected, "qqOnline":bot.online, "selfId":bot.self_id,
            "reconnects":bot.reconnects, "activeChats":self.engine.chats().len(),
            "model":self.raw["provider"]["model"], "provider":c.provider.kind, "apiCallsThisRun":self.provider.calls(),
            "lastCycleAt":self.engine.last_cycle(), "lastError":self.engine.last_error()}))
    }
    fn report(&self) -> Result<()> {
        settings::atomic_json(
            &self.config.data_dir.join("status.json"),
            &self.status((self.now)(), std::process::id())?,
        )
    }
    fn maintain(&self, prune: bool) -> Result<()> {
        let db = self.store.lock().unwrap();
        let now = (self.now)();
        if prune {
            db.prune(
                now,
                self.config.storage.retention_days,
                self.config.storage.max_messages_per_chat as i64,
            )?;
        }
        LayeredMemory::new(&db).configure(now, &self.config.agent.memory)?;
        ExpressionMemory::new(&db).prune(now, &self.config.agent.expression)?;
        Ok(())
    }
    async fn disconnect(&mut self) {
        self.abort.send_replace(true);
        if let Some(connection) = self.connection.take() {
            let _ = connection.await;
        }
        while let Ok(notice) = self.notices.try_recv() {
            self.notice(notice).await;
        }
    }
    async fn reload(
        &mut self,
        root: &Path,
        revision: String,
        shutdown: &mut watch::Receiver<bool>,
        report: &mut Interval,
    ) -> Result<()> {
        // Recheck immediately before loading; the watcher may have observed an older revision.
        if root.join(".settings-write").exists() {
            return Ok(());
        }
        let next = config::load_config(root)?;
        if root.join(".settings-write").exists() || settings::revision(root)? != revision {
            return Ok(());
        }
        anyhow::ensure!(
            next.config.data_dir == self.config.data_dir,
            "data_directory_change"
        );
        let reconnect = next.raw["onebot"] != self.raw["onebot"]
            || next.raw["onebotToken"] != self.raw["onebotToken"];
        let engine = self.engine.clone();
        let stop = engine.stop();
        tokio::pin!(stop);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => { self.abort.send_replace(true); stop.await; return Ok(()); }
                _ = &mut stop => break,
                _ = report.tick() => self.report()?,
                Some(notice) = self.notices.recv() => self.notice(notice).await,
            }
        }
        if *shutdown.borrow() {
            return Ok(());
        }
        if reconnect {
            self.disconnect().await;
            if *shutdown.borrow() {
                return Ok(());
            }
        }
        let mut replacement = Self::new(
            next,
            self.store.clone(),
            revision,
            self.now.clone(),
            self.log.clone(),
            (!reconnect).then(|| self.bot.clone()),
        )?;
        replacement.engine.inherit_chats(self.engine.chats());
        replacement.maintain(false)?;
        if reconnect {
            replacement.connect();
        } else {
            std::mem::swap(&mut replacement.notices, &mut self.notices);
            replacement.connection = self.connection.take();
            replacement.abort = self.abort.clone();
        }
        replacement.control = self.control.take();
        if let Some(control) = &replacement.control {
            control.replace(
                Arc::new(LiveRemote {
                    bot: replacement.bot.clone(),
                    provider: replacement.provider.clone(),
                    config: replacement.config.clone(),
                }),
                vec![
                    replacement.config.api_key.text.clone(),
                    replacement.config.onebot_token.text.clone(),
                ],
            );
        }
        *self = replacement;
        (self.log)("config_applied", json!({"revision": &self.applied[..12]}));
        Ok(())
    }
    async fn shutdown(mut self) -> Result<()> {
        self.abort.send_replace(true);
        self.engine.stop().await;
        self.disconnect().await;
        let result = self.report();
        let store = self.store.clone();
        let log = self.log.clone();
        drop(self);
        // P6a/Provider boundary: cancelled spawn_blocking HTTP keeps an Arc<Store>.
        // Unlike JS abort, drain those bounded requests before dropping SQLite.
        while Arc::strong_count(&store) > 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(store);
        log("stopped", json!({}));
        result
    }
}
fn timer(seconds: u64) -> Interval {
    let duration = Duration::from_secs(seconds);
    let mut timer = interval_at(Instant::now() + duration, duration);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    timer
}
async fn run(root: PathBuf) -> Result<()> {
    // Install all signal handlers before opening resources/connecting.
    let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let (stop, mut shutdown) = watch::channel(false);
    let signals = tokio::spawn(async move {
        tokio::select! { _ = int.recv() => {}, _ = term.recv() => {}, _ = hup.recv() => {} }
        stop.send_replace(true);
    });
    anyhow::ensure!(
        !root.join(".settings-write").exists(),
        "Pending settings recovery"
    );
    let loaded = config::load_config(&root)?;
    let c = &loaded.config;
    // No new libc dependency: restrict the directory and precreate SQLite at 0600.
    // SQLite creates WAL/SHM with the database's mode.
    fs::create_dir_all(&c.data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        fs::set_permissions(&c.data_dir, fs::Permissions::from_mode(0o700))?;
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(c.data_dir.join("agent.sqlite"))?;
    }
    let store = Arc::new(Mutex::new(Store::open(c.data_dir.join("agent.sqlite"))?));
    store.lock().unwrap().recover_deliveries()?;
    let now = Options::default().now;
    let events = Events::default();
    let file_log = logger(c.data_dir.clone(), now.clone());
    let event_log = events.clone();
    let log: Logger = Arc::new(move |event, data| {
        event_log.publish(event, data.clone());
        file_log(event, data);
    });
    let mut rt = Runtime::new(loaded, store, settings::revision(&root)?, now, log, None)?;
    rt.engine.restore()?;
    (rt.log)(
        "started",
        json!({"mode":mode(&rt.config), "missing":config::readiness(&rt.config), "provider":rt.config.provider.kind, "model":rt.raw["provider"]["model"], "selectedChats":rt.config.agent.allowed_groups.len()+rt.config.agent.allowed_users.len()}),
    );
    rt.maintain(true)?;
    rt.report()?;
    let backend = Arc::new(Backend::new(
        rt.config.data_dir.clone(),
        rt.store.clone(),
        Arc::new(LiveRemote {
            bot: rt.bot.clone(),
            provider: rt.provider.clone(),
            config: rt.config.clone(),
        }),
        vec![
            rt.config.api_key.text.clone(),
            rt.config.onebot_token.text.clone(),
        ],
    ));
    let control = Server::bind(&rt.config.data_dir, backend.clone(), events)?;
    rt.control = Some(backend);
    rt.connect();
    let mut tick = timer(1);
    let mut report = timer(5);
    let mut prune = timer(3600);
    let mut watcher = timer(1);
    let result: Result<()> = async {
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                _ = tick.tick() => rt.engine.tick()?,
                _ = report.tick() => rt.report()?,
                _ = prune.tick() => rt.maintain(true)?,
                _ = watcher.tick() => {
                    if let Some(revision) = changed(&root, &rt.applied)? {
                        rt.reloading = true;
                        if rt.reload(&root, revision, &mut shutdown, &mut report).await.is_err() {
                            rt.reload_error = Some("Invalid configuration; previous settings remain active.");
                            (rt.log)("config_reload_rejected", json!({}));
                        }
                        rt.reloading = false;
                        rt.report()?;
                        if *shutdown.borrow() { break; }
                    }
                }
                Some(notice) = rt.notices.recv() => rt.notice(notice).await,
                _ = async { if let Some(connection) = &mut rt.connection { let _ = connection.await; } } => {
                    rt.connection = None;
                    (rt.log)("connection_loop_failed", json!({}));
                    break;
                }
            }
        }
        Ok(())
    }.await;
    control.stop().await;
    let stopped = rt.shutdown().await;
    signals.abort();
    result.and(stopped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command as Process, Stdio};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "qr-{}-{}",
                std::process::id(),
                rand::random::<u32>()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn config(&self, extra: Value) {
            let c = config::merge(
                &json!({"storage":{"directory":"state"}, "agent":{"observation":{"enabled":false},"rhythm":{"enabled":false}}}),
                &extra,
            );
            settings::atomic_json(&self.0.join("config.json"), &c).unwrap();
        }
        fn runtime(&self) -> Runtime {
            let loaded = config::load_with_env(&self.0, |_| None).unwrap();
            fs::create_dir_all(&loaded.config.data_dir).unwrap();
            Runtime::new(
                loaded,
                Arc::new(Mutex::new(Store::in_memory().unwrap())),
                settings::revision(&self.0).unwrap(),
                Arc::new(|| 1_700_000_000.125),
                Arc::new(|_, _| {}),
                None,
            )
            .unwrap()
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn revision_and_write_barrier() {
        let dir = Temp::new();
        dir.config(json!({}));
        let applied = settings::revision(&dir.0).unwrap();
        assert!(changed(&dir.0, &applied).unwrap().is_none());
        fs::write(dir.0.join(".settings-write"), "journal").unwrap();
        // An unreadable config proves the marker is checked BEFORE reading/hash/loading.
        fs::remove_file(dir.0.join("config.json")).unwrap();
        fs::create_dir(dir.0.join("config.json")).unwrap();
        assert!(changed(&dir.0, &applied).unwrap().is_none());
        fs::remove_dir(dir.0.join("config.json")).unwrap();
        dir.config(json!({"agent":{"dryRun":true}}));
        fs::remove_file(dir.0.join(".settings-write")).unwrap();
        assert!(changed(&dir.0, &applied).unwrap().is_some());
        let applied = settings::revision(&dir.0).unwrap();
        fs::write(dir.0.join("secrets.json"), "{\"apiKey\":\"test\"}").unwrap();
        assert!(changed(&dir.0, &applied).unwrap().is_some());
    }

    #[tokio::test]
    async fn reload_preserves_store_and_allowed_chats_and_rejects_directory() {
        let dir = Temp::new();
        dir.config(json!({"agent":{"allowedUsers":["1","2"]}}));
        let mut rt = dir.runtime();
        let store = rt.store.clone();
        let old_engine = rt.engine.clone();
        let old_bot = rt.bot.clone();
        let mut state = qq_inner_core::engine::ChatState {
            busy: true,
            last_think: 99.,
            pending: true,
            version: 7,
            ..Default::default()
        };
        rt.engine
            .inherit_chats(vec![("private:1".into(), state.clone())]);
        state.version = 8;
        rt.engine.inherit_chats(vec![("private:2".into(), state)]);
        dir.config(json!({"agent":{"allowedUsers":["1"],"dryRun":true}}));
        let (_stop, mut shutdown) = watch::channel(false);
        let revision = settings::revision(&dir.0).unwrap();
        rt.reload(&dir.0, revision.clone(), &mut shutdown, &mut timer(5))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&store, &rt.engine.store));
        assert!(!Arc::ptr_eq(&old_engine, &rt.engine));
        assert!(Arc::ptr_eq(&old_bot, &rt.bot));
        assert_eq!(rt.applied, revision);
        let chats = rt.engine.chats();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].1.version, 7);
        assert!(chats[0].1.pending);
        assert!(!chats[0].1.busy);
        assert_eq!(chats[0].1.last_think, 0.);
        fs::write(dir.0.join(".settings-write"), "pending").unwrap();
        fs::write(dir.0.join("config.json"), "{").unwrap();
        rt.reload(&dir.0, "ignored".into(), &mut shutdown, &mut timer(5))
            .await
            .unwrap();
        assert_eq!(rt.applied, revision);
        fs::remove_file(dir.0.join(".settings-write")).unwrap();
        dir.config(json!({"storage":{"directory":"other"}}));
        let current = rt.engine.clone();
        assert!(rt
            .reload(
                &dir.0,
                settings::revision(&dir.0).unwrap(),
                &mut shutdown,
                &mut timer(5)
            )
            .await
            .is_err());
        assert!(Arc::ptr_eq(&current, &rt.engine));
        assert_eq!(rt.applied, revision);
        fs::write(dir.0.join("config.json"), "{").unwrap();
        assert!(rt
            .reload(
                &dir.0,
                settings::revision(&dir.0).unwrap(),
                &mut shutdown,
                &mut timer(5)
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn reconnect_replaces_bot_but_shutdown_does_not_apply_reload() {
        let dir = Temp::new();
        dir.config(json!({}));
        let mut rt = dir.runtime();
        let bot = rt.bot.clone();
        let store = Arc::downgrade(&rt.store);
        let mut abort = rt.abort.subscribe();
        rt.connection = Some(tokio::spawn(async move {
            abort.changed().await.unwrap();
        }));
        // Current-thread runtime: the newly spawned connection is cancelled before its first poll.
        dir.config(json!({"onebot":{"selfId":"123"}}));
        let (stop, mut shutdown) = watch::channel(false);
        rt.reload(
            &dir.0,
            settings::revision(&dir.0).unwrap(),
            &mut shutdown,
            &mut timer(5),
        )
        .await
        .unwrap();
        assert!(!Arc::ptr_eq(&bot, &rt.bot));
        assert_eq!(rt.bot.state().self_id, "123");
        assert!(Arc::ptr_eq(&store.upgrade().unwrap(), &rt.store));
        rt.disconnect().await;
        let applied = rt.applied.clone();
        dir.config(json!({"agent":{"dryRun":true}}));
        stop.send_replace(true);
        rt.reload(
            &dir.0,
            settings::revision(&dir.0).unwrap(),
            &mut shutdown,
            &mut timer(5),
        )
        .await
        .unwrap();
        assert_eq!(rt.applied, applied);
        rt.shutdown().await.unwrap();
        assert!(store.upgrade().is_none());
    }

    #[test]
    fn status_matches_actual_js_function() {
        if Process::new("node").arg("--version").output().is_err() {
            eprintln!("SKIP JS status oracle: Node unavailable");
            return;
        }
        let dir = Temp::new();
        for extra in [
            json!({}),
            json!({"provider":{"model":"test"},"agent":{"allowedUsers":["1"],"dryRun":true}}),
            json!({"provider":{"model":"test"},"agent":{"allowedUsers":["1"],"dryRun":false,"rhythm":{"enabled":true}}}),
            json!({"provider":{"model":123}}),
        ] {
            dir.config(extra);
            fs::write(dir.0.join("secrets.json"), "{\"apiKey\":\"fake\"}").unwrap();
            let mut rt = dir.runtime();
            // Persist a deterministic, exactly representable draw: JSON's JS/Rust
            // float parsers can differ by one ULP on arbitrary random decimals.
            ActivityRhythm::new(
                &rt.store.lock().unwrap(),
                &rt.config.agent.schedule,
                &rt.config.agent.rhythm,
                || 0.25,
            )
            .snapshot((rt.now)())
            .unwrap();
            rt.reloading = true;
            rt.reload_error = Some("Invalid configuration; previous settings remain active.");
            let actual = rt.status((rt.now)(), 42).unwrap();
            let input = json!({"config":rt.raw,"status":actual});
            // Extract and execute the real status() body; no import of main.mjs (would start networking).
            // ActivityRhythm already has its own JS parity suite; here inject its persisted snapshot.
            let script = r#"
import fs from 'node:fs'; import path from 'node:path'; import vm from 'node:vm';
import { readiness } from './src/config.mjs';
const {config,status:s} = JSON.parse(fs.readFileSync(0,'utf8'));
const source = fs.readFileSync('src/main.mjs','utf8');
const body = source.slice(source.indexOf('function status() {'),source.indexOf('\nconst tick ='));
let result;
class Clock extends Date { constructor(){ super(s.updatedAt); } static now(){return new Date(s.updatedAt).getTime();} }
vm.runInNewContext(body+';status()', {Date:Clock, process:{pid:42}, path, config, readiness,
 appliedRevision:s.appliedRevision,reloading:true,reloadError:s.reloadError,
 engine:{available:()=>s.scheduleActive,activity:{snapshot:()=>s.activityRhythm},chats:new Map(),lastCycle:0,lastError:null},
 bot:{connected:false,online:false,selfId:'',reconnects:0},provider:{calls:0},
 fs:{writeFileSync:(_,text)=>result=JSON.parse(text),renameSync:()=>{}}});
console.log(JSON.stringify(result));
"#;
            let mut child = Process::new("node")
                .args(["--input-type=module", "-e", script])
                .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.to_string().as_bytes())
                .unwrap();
            let result = child.wait_with_output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let expected: Value = serde_json::from_slice(&result.stdout).unwrap();
            fn numeric(v: &mut Value) {
                match v {
                    Value::Number(n) => *v = json!(n.as_f64().unwrap()),
                    Value::Array(a) => a.iter_mut().for_each(numeric),
                    Value::Object(o) => o.values_mut().for_each(numeric),
                    _ => {}
                }
            }
            let mut comparable = actual.clone();
            let mut expected = expected;
            numeric(&mut comparable);
            numeric(&mut expected);
            assert_eq!(comparable, expected);
            assert_eq!(actual.as_object().unwrap().len(), 19);
        }
    }

    #[test]
    fn status_atomic_and_private() {
        let dir = Temp::new();
        dir.config(json!({}));
        let rt = dir.runtime();
        rt.report().unwrap();
        let file = rt.config.data_dir.join("status.json");
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let reader_running = running.clone();
        let reader_file = file.clone();
        let (ready, started) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reads = 0;
            while reader_running.load(std::sync::atomic::Ordering::Relaxed) {
                let v: Value = serde_json::from_slice(&fs::read(&reader_file).unwrap()).unwrap();
                assert_eq!(v.as_object().unwrap().len(), 19);
                reads += 1;
                if reads == 1 {
                    ready.send(()).unwrap();
                }
            }
            reads
        });
        started.recv().unwrap();
        for _ in 0..200 {
            rt.report().unwrap();
        }
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(reader.join().unwrap() > 0);
        assert!(!file.with_extension("json.tmp").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn shutdown_drains_connection_and_blocking_store_owners() {
        let dir = Temp::new();
        dir.config(json!({}));
        let mut rt = dir.runtime();
        let weak = Arc::downgrade(&rt.store);
        let mut signal = rt.abort.subscribe();
        rt.connection = Some(tokio::spawn(async move {
            signal.changed().await.unwrap();
            assert!(*signal.borrow());
        }));
        // Explicit known JS difference: uncancellable HTTP workers may own Store after Engine::stop.
        let store = rt.store.clone();
        let (release, waiting) = tokio::sync::oneshot::channel::<()>();
        let worker = tokio::spawn(async move {
            waiting.await.unwrap();
            drop(store);
        });
        let shutdown = tokio::spawn(rt.shutdown());
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        release.send(()).unwrap();
        worker.await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn provider_counter_is_per_generation_and_only_counts_admitted_attempts() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = Temp::new();
        dir.config(json!({}));
        let mut rt = dir.runtime();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let body = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let mut c = rt.config.provider.clone();
        c.base_url = format!("http://{address}");
        c.requests_per_hour = 1.;
        c.retries = 0.;
        rt.provider = Arc::new(Provider::new(c.clone(), "fake", rt.store.clone()));
        assert_eq!(rt.provider.complete("test", "test").await.unwrap(), "ok");
        server.await.unwrap();
        assert_eq!(rt.status((rt.now)(), 42).unwrap()["apiCallsThisRun"], 1);
        assert!(rt.provider.complete("test", "test").await.is_err());
        assert_eq!(rt.provider.calls(), 1);
        let next = Provider::new(c, "fake", rt.store.clone());
        assert_eq!(next.calls(), 0);
        assert!(next.complete("test", "test").await.is_err());
        assert_eq!(next.calls(), 0); // Store budget survives provider replacement.
    }

    #[test]
    fn signal_child() {
        if let Some(root) = std::env::var_os("QIA_RUNTIME_SIGNAL_TEST") {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(run(PathBuf::from(root)))
                .unwrap();
        }
    }

    #[test]
    fn all_signals_close_socket_and_sqlite() {
        use std::io::Read;
        for signal in ["-INT", "-TERM", "-HUP"] {
            let dir = Temp::new();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            dir.config(
                json!({"onebot":{"url":format!("ws://{}/",listener.local_addr().unwrap())}}),
            );
            let mut child = Process::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::signal_child", "--nocapture"])
                .env("QIA_RUNTIME_SIGNAL_TEST", &dir.0)
                .env_remove("ONEBOT_TOKEN")
                .env_remove("LLM_API_KEY")
                .env_remove("OPENAI_API_KEY")
                .env_remove("DEEPSEEK_API_KEY")
                .env_remove("ANTHROPIC_API_KEY")
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                if let Ok((socket, _)) = listener.accept() {
                    break socket;
                }
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("child did not connect to mock");
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(Process::new("kill")
                .args([signal, &child.id().to_string()])
                .status()
                .unwrap()
                .success());
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("signal did not drain runtime");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut bytes = Vec::new();
            socket.read_to_end(&mut bytes).unwrap();
            let status: Value =
                serde_json::from_slice(&fs::read(dir.0.join("state/status.json")).unwrap())
                    .unwrap();
            assert_eq!(status["onebotConnected"], false);
            assert!(!dir.0.join("state/control.sock").exists());
            assert!(!dir.0.join("state/agent.sqlite-wal").exists());
            let db = rusqlite::Connection::open(dir.0.join("state/agent.sqlite")).unwrap();
            db.execute_batch("BEGIN EXCLUSIVE; ROLLBACK;").unwrap();
            assert!(fs::read_to_string(dir.0.join("state/agent.log"))
                .unwrap()
                .contains("\"event\":\"stopped\""));
        }
    }
}
