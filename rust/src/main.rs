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

async fn running_revision(data_dir: &std::path::Path) -> Option<String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut socket = tokio::net::UnixStream::connect(data_dir.join("control.sock"))
            .await
            .ok()?;
        socket
            .write_all(b"{\"id\":\"full-update\",\"method\":\"state.get\"}\n")
            .await
            .ok()?;
        let mut reader = BufReader::new(socket);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.ok()? == 0 {
                return None;
            }
            let reply: Value = serde_json::from_str(&line).ok()?;
            if reply["id"] != "full-update" {
                continue;
            }
            if reply["ok"] != true {
                return None;
            }
            return reply["result"]["runningRevision"]
                .as_str()
                .map(str::to_owned);
        }
    })
    .await
    .ok()
    .flatten()
}

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
    /// Print the revision embedded at compile time.
    BuildRevision,
    /// Human-triggered binary deployment via the local control socket.
    Update {
        #[arg(value_parser = ["apply", "status", "rollback", "full"])]
        action: String,
    },
    /// Run the agent until SIGINT, SIGTERM or SIGHUP.
    Start,
    /// Print actual SQLite schema (in memory unless --database is supplied).
    DbSchema {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    /// Print a normalized summary with credentials redacted.
    Config,
    /// Print the full default configuration schema (dashboard's dynamic whitelist).
    ConfigDefaults,
    /// Authenticate with the OneBot bridge and report online status.
    Check,
    /// Inspect and reconcile the configured plugin list.
    Plugins {
        #[arg(value_parser = ["reconcile"])]
        action: String,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        json: bool,
    },
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

fn plugin_status(root: &std::path::Path, list: &Value) -> Value {
    let resolved = qq_inner_core::engine::io::plugin_list::preflight(root, list);
    project_plugin_status(&resolved, list)
}

fn project_plugin_status(resolved: &Value, list: &Value) -> Value {
    let entries: Vec<Value> = resolved["plugins"]
        .as_object()
        .into_iter()
        .flat_map(|items| items.iter())
        .map(|(id, item)| {
            json!({"id": id, "directory": item["directory"],
                "requested_enabled": list["plugins"].as_array().and_then(|entries| entries.iter().find(|entry| entry["id"].as_str() == Some(id))).is_some_and(|entry| entry["enabled"] == true),
                "effective_state": if item["reason"] == "disabled_by_user" { "disabled" } else if item["reason"] == "degraded" { "degraded" } else if item["effective"] == true { "active" } else { "inactive" },
                "reason": item["reason"], "installed": item["installed"], "missing":item["missing"],
                "host_permissions":item["host_permissions"], "capabilities":item["capabilities"],
                "declared_capabilities":item["declared_capabilities"]})
        })
        .collect();
    let discovery: Vec<Value> = entries.iter().filter(|entry| entry["id"] == entry["directory"]).map(|entry| {
        json!({"directory": entry["directory"],
            "state": if entry["reason"] == "missing_manifest" { "unrecognized" } else if entry["installed"] == true { "discovered" } else { "unrecognized" },
            "reason": if entry["reason"] == "missing_manifest" { "manifest_missing" } else { entry["reason"].as_str().unwrap_or("unknown") }})
    }).collect();
    json!({"schema_version":1,"mode":resolved["mode"],"generation":0,
        "plugins":entries,"discovery":discovery,"registered":resolved["registered"],
        "activation_order":resolved["registered"], "reason":resolved["reason"]})
}

fn plugin_factory_status(root: &Path, list: &Value, log: Option<Logger>, bot: Option<Arc<OneBot>>, model_hourly_limit: u32)
    -> (Value, Option<qq_inner_core::engine::io::factory::PublishedPorts>) {
    use qq_inner_core::engine::io::factory::{self, HostServices};
    let plugins = if root.join("plugins").is_dir() { root.join("plugins") } else { root.to_path_buf() };
    let secrets = plugins.parent().and_then(|parent| fs::read(parent.join("secrets.json")).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).unwrap_or(Value::Null);
    let secret = Arc::new(move |reference: &str| {
        let key = reference.strip_prefix("secrets.")?;
        secrets.get(key)?.as_str().map(str::to_owned)
    });
    let mut host = HostServices::without_runtime(secret);
    host.model_hourly_limit = model_hourly_limit;
    if let Some(bot) = bot {
        use qq_inner_core::engine::io::PortReply;
        let action_bot = bot.clone();
        host.onebot_action = Some(Arc::new(move |action, params| {
            let bot = action_bot.clone();
            Box::pin(async move {
                match bot.call(&action, params).await {
                    Ok(data) => PortReply::Ok(data),
                    Err(error) => PortReply::Failed { code: error.code, uncertain: error.uncertain },
                }
            })
        }));
        host.onebot_connection = Some(Arc::new(move || {
            let state = bot.state();
            (state.connected, state.online, (!state.self_id.is_empty()).then(|| state.self_id.clone()))
        }));
    }
    if let Some(log) = log {
        host.audit = Some(Arc::new(move |plugin, target| {
            log("plugin_http_call", json!({"plugin":plugin,"target":target}));
        }));
    }
    let mut outcome = factory::prepare(&plugins, list, &host);
    for (id, state) in outcome.status["plugins"].as_object_mut().into_iter().flat_map(|m| m.iter_mut()) {
        let directory = state["directory"].as_str().unwrap_or(id);
        let manifest = plugins.join(directory).join("manifest.json");
        if let Ok(bytes) = fs::read(manifest) {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                state["host_permissions"] = value["host_permissions"].clone();
                state["declared_capabilities"] = value["capabilities"].clone();
            }
        }
    }
    let status = project_plugin_status(&outcome.status, list);
    (status, outcome.published)
}

fn load_plugin_batch(root: &Path, log: Logger, bot: Option<Arc<OneBot>>, model_hourly_limit: u32)
    -> Result<Option<qq_inner_core::engine::io::factory::PublishedPorts>> {
    let path = root.join("plugins/plugins.json");
    if !path.is_file() { return Ok(None); }
    let list: Value = serde_json::from_slice(&fs::read(path)?)?;
    let (status, published) = plugin_factory_status(root, &list, Some(log), bot, model_hourly_limit);
    if let Some(error) = plugin_configuration_error(&status, &list) {
        anyhow::bail!("plugin batch rejected: {error}");
    }
    Ok(published)
}

fn plugin_configuration_error(status: &Value, list: &Value) -> Option<Value> {
    if !matches!(list.get("schema_version").or_else(|| list.get("version")).and_then(Value::as_u64), Some(1))
        || !matches!(list["mode"].as_str(), Some("auto" | "manual"))
        || !list["plugins"].is_array() || status["reason"] == "invalid_plugin_list" {
        return Some(json!({"code":"invalid_plugin_list"}));
    }
    status["plugins"].as_array()?.iter().find_map(|entry| {
        let selected = list["plugins"].as_array().is_some_and(|items| items.iter().any(|item| {
            item["id"] == entry["id"]
        }));
        if !selected { return None; }
        if entry["reason"] == "preflight_pending" || entry["effective_state"] == "active" || entry["effective_state"] == "degraded"
            || entry["effective_state"] == "disabled" { return None; }
        let code = match entry["reason"].as_str()? {
            "missing_directory" => "plugin_directory_missing",
            "missing_manifest" => "plugin_manifest_missing",
            "invalid_manifest" => "plugin_manifest_invalid",
            "load_failed" => "plugin_load_failed",
            "invalid_entry" => "plugin_entry_invalid",
            "invalid_config" => "plugin_config_invalid",
            "invalid_order" => "plugin_order_invalid",
            "preflight_failed" => "plugin_preflight_failed",
            _ => "plugin_activation_failed",
        };
        Some(json!({"code":code,"id":entry["id"],"directory":entry["directory"]}))
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = resolve_root(cli.root)?;

    match cli.command {
        Command::Plugins { action: _, config, json: _ } => {
            let list: Value = serde_json::from_slice(&fs::read(&config)?)?;
            let (mut status, _prepared) = plugin_factory_status(&root, &list, None, None, u32::MAX);
            if let Some(error) = plugin_configuration_error(&status, &list) {
                if let Some(fields) = error.as_object() {
                    status.as_object_mut().expect("status object").extend(fields.clone());
                }
                eprintln!("{status}");
                std::process::exit(2);
            }
            println!("{status}");
        }
        Command::BuildRevision => println!("{}", qq_inner_core::update::REVISION),
        Command::Update { action } => {
            if action == "full" {
                let loaded = config::load_config(&root)?;
                let running = running_revision(&loaded.config.data_dir).await;
                let result = qq_inner_core::update::full_update(
                    &fs::canonicalize(&root)?,
                    &std::env::current_exe()?,
                    running.as_deref(),
                    &loaded.config.data_dir.join("agent.sqlite"),
                )?;
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(());
            }
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let dir = config::load_config(&root)?.config.data_dir;
            let mut socket = tokio::net::UnixStream::connect(dir.join("control.sock")).await?;
            socket.write_all(format!("{{\"id\":\"cli\",\"method\":\"update.{action}\"}}\n").as_bytes()).await?;
            let mut reader = BufReader::new(socket);
            loop {
                let mut line = String::new();
                anyhow::ensure!(reader.read_line(&mut line).await? > 0, "control disconnected; query update status");
                let reply: Value = serde_json::from_str(&line)?;
                if reply["id"] == "cli" {
                    println!("{}", serde_json::to_string_pretty(&reply)?);
                    anyhow::ensure!(reply["ok"] == true, "update request rejected");
                    break;
                }
            }
        }
        Command::Start => {
            if root.join(".settings-write").exists() || config::load_config(&root).is_err() {
                eprintln!("Invalid configuration. Run ./agent setup to correct it.");
                std::process::exit(2);
            }
            let plugin_config = root.join("plugins/plugins.json");
            if plugin_config.is_file() {
                let list: Value = serde_json::from_slice(&fs::read(&plugin_config)?)?;
                let status = plugin_status(&root.join("plugins"), &list);
                if let Some(error) = plugin_configuration_error(&status, &list) {
                    eprintln!("{error}");
                    std::process::exit(2);
                }
            }
            let _ = fs::remove_file(root.join(".plugin-restart-pending"));
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
            let (bot, _notices) = qq_inner_core::transport::new(c.onebot, token);
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
        Command::ConfigDefaults => {
            let schema = config::public_defaults()?;
            println!("{}", serde_json::to_string_pretty(&schema)?);
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
    control::{Backend, Events, LiveRemote, Server},
    engine::activity::ActivityRhythm,
    engine::{Clock, Engine, Logger, Options},
    persona::expression::ExpressionMemory,
    memory::LayeredMemory,
    transport::{Notification, OneBot},
    transport::provider::Provider,
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
fn runtime_revision(root: &Path) -> Result<String> {
    let base = format!("{}:{}", settings::revision(root)?, qq_inner_core::prompt_overlay::revision(root));
    let list = root.join("plugins/plugins.json");
    if !list.is_file() { return Ok(base); }
    use sha2::{Digest, Sha256};
    Ok(format!("{base}:{:x}", Sha256::digest(fs::read(list)?)))
}
fn changed(root: &Path, applied: &str) -> Result<Option<String>> {
    if root.join(".settings-write").exists() || root.join(".plugin-restart-pending").exists() {
        return Ok(None);
    }
    let next = runtime_revision(root)?;
    Ok((next != applied).then_some(next))
}
fn event_rejected_context(event: &Value) -> Value {
    let kind = event["message_type"].as_str().unwrap_or("");
    let target = if kind == "group" { &event["group_id"] } else { &event["user_id"] };
    let target_id = |value: &Value| value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
    let chat = if matches!(kind, "group" | "private") && !target.is_null() {
        format!("{kind}:{}", target_id(target))
    } else if !event["group_id"].is_null() {
        format!("group:{}", target_id(&event["group_id"]))
    } else {
        String::from("unknown")
    };
    json!({"chat":chat,"raw_message_id":event["message_id"],"raw_group_id":event["group_id"],
        "raw_user_id":event["user_id"],"post_type":event["post_type"],
        "message_type":event["message_type"],"notice_type":event["notice_type"]})
}
fn event_rejected_payload(mut context: Value, reason: &str, error: &str) -> Value {
    context["reason"] = json!(reason);
    context["error"] = json!(error);
    context
}
struct Runtime {
    config: config::Config,
    raw: Value,
    store: Arc<Mutex<Store>>,
    provider: Arc<Provider>,
    model_port: Option<Arc<qq_inner_core::engine::io::adapters::model_http::HttpModel>>,
    _plugin_ports: Option<qq_inner_core::engine::io::factory::PublishedPorts>,
    bot: Arc<OneBot>,
    onebot_io: Option<Arc<qq_inner_core::engine::io::adapters::onebot::OneBotAdapter>>,
    notices: mpsc::UnboundedReceiver<Notification>,
    engine: Arc<Engine>,
    connection: Option<JoinHandle<()>>,
    backfill: Option<JoinHandle<()>>,
    next_backfill: Instant,
    abort: watch::Sender<bool>,
    applied: String,
    reloading: bool,
    reload_error: Option<&'static str>,
    plugin_reload_failure: Option<Value>,
    last_alerted_plugin_revision: Option<String>,
    now: Clock,
    log: Logger,
    control: Option<Arc<Backend>>,
}
struct RuntimePlugins {
    ports: Option<qq_inner_core::engine::io::factory::PublishedPorts>,
    initial_bot: Option<(Arc<OneBot>, mpsc::UnboundedReceiver<Notification>)>,
}
fn use_onebot_io(value: Option<&str>) -> bool {
    value == Some("1")
}
struct SelectedModelPort {
    port: Arc<qq_inner_core::engine::io::adapters::model_http::HttpModel>,
    model: String,
    max_tokens: u32,
}
impl qq_inner_core::engine::orientation::OrientationProvider for SelectedModelPort {
    fn text<'a>(&'a self, system: &'a str, payload: Value) -> futures_util::future::BoxFuture<'a, Result<String>> {
        Box::pin(async move { Ok(self.port.complete_engine(&self.model, system, payload, self.max_tokens).await?) })
    }
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> futures_util::future::BoxFuture<'a, Result<Value>> {
        Box::pin(async move { Ok(self.port.complete_engine_json(&self.model, system, payload, self.max_tokens).await?) })
    }
}
fn live_config(c: &config::Config, model: Option<&Arc<qq_inner_core::engine::io::adapters::model_http::HttpModel>>) -> config::Config {
    let mut selected = c.clone();
    if model.is_some() && selected.provider.io_port == "legacy" {
        selected.provider.io_port = "openai".into();
    }
    selected
}
impl Runtime {
    #[cfg(test)]
    fn new(
        loaded: config::Loaded,
        store: Arc<Mutex<Store>>,
        applied: String,
        now: Clock,
        log: Logger,
        existing_bot: Option<Arc<OneBot>>,
    ) -> Result<Self> {
        Self::new_with_plugins(loaded, store, applied, now, log, existing_bot,
            RuntimePlugins { ports: None, initial_bot: None })
    }
    fn new_with_plugins(
        loaded: config::Loaded,
        store: Arc<Mutex<Store>>,
        applied: String,
        now: Clock,
        log: Logger,
        existing_bot: Option<Arc<OneBot>>,
        plugins: RuntimePlugins,
    ) -> Result<Self> {
        let RuntimePlugins { ports: plugin_ports, initial_bot } = plugins;
        // Record the bytes actually loaded, including startup edits during resource setup.
        let suffix = if plugin_ports.is_some() {
            applied.rsplit_once(':').map(|(_, hash)| format!(":{hash}")).unwrap_or_default()
        } else { String::new() };
        let applied = format!("{}:{}{}", applied.split(':').next().unwrap_or(&applied), loaded.prompts.watch_revision(), suffix);
        let mut c = loaded.config;
        if let Some(instance) = plugin_ports.as_ref().and_then(|ports| ports.model_instance.as_ref()) {
            c.provider.model = config::RuntimeText { text: instance.clone(), is_truthy: !instance.is_empty() };
            c.provider.io_port = plugin_ports.as_ref().and_then(|ports| ports.model_kind.clone())
                .unwrap_or_else(|| "openai".into());
        }
        let provider = Arc::new(Provider::new(
            c.provider.clone(),
            c.api_key.text.clone(),
            store.clone(),
        ));
        let model_port = if let Some(ports) = &plugin_ports { ports.model.clone() }
            else if c.provider.io_port == "legacy" { None } else {
            Some(Arc::new(qq_inner_core::engine::io::adapters::model_http::HttpModel::configured(
                &c.provider.io_port, &c.provider.base_url, Some(c.api_key.text.clone()),
                (c.provider.context_window_tokens > 0).then_some(c.provider.context_window_tokens),
                c.provider.requests_per_hour.round() as u32,
            )?))
        };
        let engine_provider: Arc<dyn qq_inner_core::engine::orientation::OrientationProvider> =
            if let Some(port) = &model_port {
                Arc::new(SelectedModelPort {
                    port: port.clone(),
                    model: c.provider.model.text.clone(),
                    max_tokens: c.provider.max_tokens.round() as u32,
                })
            } else { provider.clone() };
        let (bot, notices) = if let Some((bot, notices)) = initial_bot {
            (bot, notices)
        } else {
            let (bot, notices) = qq_inner_core::transport::new(c.onebot.clone(), c.onebot_token.text.clone());
            (existing_bot.unwrap_or_else(|| Arc::new(bot)), notices)
        };
        let onebot_io = if let Some(plugin) = plugin_ports.as_ref().and_then(|ports| ports.onebot.clone()) {
            Some(plugin)
        } else if use_onebot_io(std::env::var("QQ_INNER_ONEBOT_IO").ok().as_deref()) {
            use qq_inner_core::engine::io::{adapters::onebot::OneBotAdapter, PortReply};
            let action_bot = bot.clone();
            let state_bot = bot.clone();
            Some(Arc::new(OneBotAdapter::new(
                "onebot",
                Arc::new(move |action, params| {
                    let bot = action_bot.clone();
                    Box::pin(async move {
                        match bot.call(&action, params).await {
                            Ok(data) => PortReply::Ok(data),
                            Err(error) => PortReply::Failed { code: error.code, uncertain: error.uncertain },
                        }
                    })
                }),
                Arc::new(move || {
                    let state = state_bot.state();
                    (state.connected, state.online, (!state.self_id.is_empty()).then(|| state.self_id.clone()))
                }),
            )?))
        } else { None };
        let engine = Engine::new_with_media(
            c.clone(),
            store.clone(),
            engine_provider,
            bot.clone(),
            Options {
                prompts: loaded.prompts.clone(),
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
        if let Some(port) = &onebot_io {
            engine.set_chat_port(port.clone());
            engine.bind_configured_chats()?;
        }
        if let Some(ports) = &plugin_ports {
            for (address, port) in &ports.persona_bindings {
                let chat = format!("{}:{}", address.kind.as_str(), address.id);
                engine.bind_persona_port(chat, address.clone(), port.clone());
            }
        }
        let mut raw = loaded.raw;
        if let Some(instance) = plugin_ports.as_ref().and_then(|ports| ports.model_instance.as_ref()) {
            raw["provider"]["model"] = json!(instance);
            raw["provider"]["ioPort"] = json!(c.provider.io_port);
        }
        let (abort, _) = watch::channel(false);
        Ok(Self {
            control: None,
            config: c,
            raw,
            store,
            provider,
            model_port,
            _plugin_ports: plugin_ports,
            bot,
            onebot_io,
            notices,
            engine,
            connection: None,
            backfill: None,
            next_backfill: Instant::now(),
            abort,
            applied,
            reloading: false,
            reload_error: None,
            plugin_reload_failure: None,
            last_alerted_plugin_revision: None,
            now,
            log,
        })
    }
    fn connect(&mut self) {
        let bot = self.bot.clone();
        let signal = self.abort.subscribe();
        self.connection = Some(tokio::spawn(async move { bot.run(signal).await }));
    }
    fn schedule_backfill(&mut self, connected: bool) {
        if connected {
            self.next_backfill = Instant::now();
        }
        if !self.config.agent.backfill.enabled
            || !self.bot.state().connected
            || *self.abort.borrow()
            || self.backfill.as_ref().is_some_and(|task| !task.is_finished())
            || Instant::now() < self.next_backfill
        {
            return;
        }
        self.next_backfill = Instant::now()
            + Duration::from_secs_f64(self.config.agent.backfill.interval_seconds);
        let engine = self.engine.clone();
        self.backfill = Some(tokio::spawn(async move { engine.backfill_once().await }));
    }
    async fn drain_backfill(&mut self) {
        if let Some(task) = self.backfill.take() {
            let _ = task.await;
        }
    }
    async fn notice(&mut self, notice: Notification) {
        match notice {
            Notification::Status(state) => {
                self.engine.sync_connection();
                (self.log)("onebot", json!({"state":state}));
                // Both statuses are emitted by a successful handshake, including reconnects.
                if self.engine.participates() && matches!(state.as_str(), "connected" | "qq_offline") {
                    self.schedule_backfill(true);
                }
            }
            Notification::Event(event) => {
                self.engine.sync_connection();
                if let Some(control) = &self.control {
                    control.observe(&event);
                }
                if !self.engine.participates() { return; }
                if let Some(adapter) = &self.onebot_io {
                    use qq_inner_core::engine::io::PortEvent;
                    let context = event_rejected_context(&event);
                    let port_event = match adapter.map_event(&event, (self.now)()) {
                        Ok(Some(port_event)) => port_event,
                        Ok(None) => return,
                        Err(error) => {
                            (self.log)("event_rejected", event_rejected_payload(context, "map_incoming_failed", &error.to_string()));
                            return;
                        }
                    };
                    let engine = self.engine.clone();
                    let adapter = adapter.clone();
                    let outcome = tokio::task::spawn_blocking(move || match port_event {
                        PortEvent::Incoming(incoming) => engine.ingest_incoming(&incoming, adapter),
                        PortEvent::Notice(notice) => engine.ingest_notice(&notice),
                    }).await;
                    match outcome {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => (self.log)("event_rejected", event_rejected_payload(context, "ingest_incoming_failed", &error.to_string())),
                        Err(error) => (self.log)("event_rejected", event_rejected_payload(context, "ingest_task_failed", &error.to_string())),
                    }
                    return;
                }
                let event = qq_inner_core::engine::policy::resolve_forwards(
                    &event,
                    &self.bot,
                    &self.config.agent,
                    (self.now)(),
                )
                .await;
                // 入站采集含同步文件/下载 I/O；单个顺序阻塞任务，不阻塞 Tokio worker。
                let engine = self.engine.clone();
                let context = event_rejected_context(&event);
                let outcome = tokio::task::spawn_blocking(move || engine.ingest(&event)).await;
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => (self.log)("event_rejected", event_rejected_payload(context, "ingest_failed", &error.to_string())),
                    Err(error) => (self.log)("event_rejected", event_rejected_payload(context, "ingest_task_failed", &error.to_string())),
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
        // JS status fields retain their types and seconds; runningRevision reports this live process.
        let mut status = json!({"updatedAt":iso(now), "pid":pid, "mode":mode(c),
            "runningRevision":qq_inner_core::update::REVISION,
            "prompts":self.engine.prompt_status(),
            "appliedRevision":self.applied.split(':').next(),
            "reloading":self.reloading, "reloadError":self.reload_error,
            "scheduleActive":active, "activityRhythm":activity, "missing":config::readiness(c),
            "onebotConnected":bot.connected, "qqOnline":bot.online, "selfId":bot.self_id,
            "participation":self.engine.participation_state(),
            "reconnects":bot.reconnects, "activeChats":self.engine.chats().len(),
            "model":self.raw["provider"]["model"], "provider":if c.provider.io_port == "legacy" { &c.provider.kind } else { &c.provider.io_port }, "apiCallsThisRun":self.model_port.as_ref().map_or_else(|| self.provider.calls(), |p| p.calls()),
            "lastCycleAt":self.engine.last_cycle(), "lastError":self.engine.last_error()});
        if let Some(ports) = &self._plugin_ports {
            let revision = self.applied.rsplit(':').next().unwrap_or_default();
            status["pluginAppliedRevision"] = json!(revision);
            status["plugins"] = ports.status_snapshot(revision, &iso(now));
        }
        if let Some(failure) = &self.plugin_reload_failure {
            status["pluginReloadFailure"] = failure.clone();
        }
        Ok(status)
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
        if root.join(".settings-write").exists()
            || runtime_revision(root)? != revision
            || qq_inner_core::prompt_overlay::revision(root) != next.prompts.watch_revision()
        {
            return Ok(());
        }
        anyhow::ensure!(
            next.config.data_dir == self.config.data_dir,
            "data_directory_change"
        );
        let reconnect = next.raw["onebot"] != self.raw["onebot"]
            || next.raw["onebotToken"] != self.raw["onebotToken"];
        let plugin_ports = load_plugin_batch(root, self.log.clone(), Some(self.bot.clone()),
            next.config.provider.requests_per_hour.round() as u32)?;
        let engine = self.engine.clone();
        let stop = engine.stop();
        tokio::pin!(stop);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => { stop.await; self.abort.send_replace(true); return Ok(()); }
                _ = &mut stop => break,
                _ = report.tick() => self.report()?,
                Some(notice) = self.notices.recv() => self.notice(notice).await,
            }
        }
        self.drain_backfill().await;
        if *shutdown.borrow() {
            return Ok(());
        }
        if reconnect {
            self.disconnect().await;
            if *shutdown.borrow() {
                return Ok(());
            }
        }
        let mut replacement = Self::new_with_plugins(
            next,
            self.store.clone(),
            revision,
            self.now.clone(),
            self.log.clone(),
            (!reconnect).then(|| self.bot.clone()),
            RuntimePlugins { ports: plugin_ports, initial_bot: None },
        )?;
        if self.engine.manual_mode() != qq_inner_core::engine::ManualMode::Normal {
            replacement.engine.set_manual_mode(self.engine.manual_mode());
        }
        replacement.engine.sync_connection();
        replacement.engine.inherit_chats(self.engine.chats());
        if let Some(port) = &replacement.onebot_io {
            replacement.engine.set_chat_port(port.clone());
            replacement.engine.bind_configured_chats()?;
        }
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
            control.replace_engine(replacement.engine.clone());
            control.replace(
                Arc::new(LiveRemote {
                    bot: replacement.bot.clone(),
                    provider: replacement.provider.clone(),
                    model_port: replacement.model_port.clone(),
                    config: live_config(&replacement.config, replacement.model_port.as_ref()),
                }),
                vec![
                    replacement.config.api_key.text.clone(),
                    replacement.config.onebot_token.text.clone(),
                ],
            );
        }
        *self = replacement;
        (self.log)("config_applied", json!({"revision": &self.applied[..12], "prompts": self.engine.prompt_status()}));
        Ok(())
    }
    async fn shutdown(mut self) -> Result<()> {
        self.engine.stop().await;
        self.abort.send_replace(true);
        self.drain_backfill().await;
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
    let signal_stop = stop.clone();
    let signals = tokio::spawn(async move {
        tokio::select! { _ = int.recv() => {}, _ = term.recv() => {}, _ = hup.recv() => {} }
        signal_stop.send_replace(true);
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
    let now = Options::default().now;
    let events = Events::default();
    let file_log = logger(c.data_dir.clone(), now.clone());
    let event_log = events.clone();
    let log: Logger = Arc::new(move |event, data| {
        event_log.publish(event, data.clone());
        file_log(event, data);
    });
    let store = Arc::new(Mutex::new(Store::open(c.data_dir.join("agent.sqlite")).inspect_err(|error| {
        let event = if error.to_string().contains("manual_participation") {
            "manual_participation_restore_failed"
        } else {
            "store_open_failed"
        };
        log(event, json!({"error":error.to_string()}));
    })?));
    store.lock().unwrap().recover_deliveries()?;
    let initial_bot = if root.join("plugins/plugins.json").is_file() {
        let (bot, notices) = qq_inner_core::transport::new(c.onebot.clone(), c.onebot_token.text.clone());
        Some((Arc::new(bot), notices))
    } else { None };
    let plugin_ports = load_plugin_batch(&root, log.clone(), initial_bot.as_ref().map(|(bot, _)| bot.clone()),
        c.provider.requests_per_hour.round() as u32)?;
    let mut rt = Runtime::new_with_plugins(loaded, store, runtime_revision(&root)?, now, log, None,
        RuntimePlugins { ports: plugin_ports, initial_bot })?;
    rt.engine.restore()?;
    (rt.log)(
        "started",
        json!({"mode":mode(&rt.config), "missing":config::readiness(&rt.config), "provider":if rt.config.provider.io_port == "legacy" { &rt.config.provider.kind } else { &rt.config.provider.io_port }, "model":rt.raw["provider"]["model"], "selectedChats":rt.config.agent.allowed_groups.len()+rt.config.agent.allowed_users.len()}),
    );
    rt.maintain(true)?;
    rt.report()?;
    let backend = Arc::new(Backend::new(
        rt.config.data_dir.clone(),
        rt.store.clone(),
        Arc::new(LiveRemote {
            bot: rt.bot.clone(),
            provider: rt.provider.clone(),
            model_port: rt.model_port.clone(),
            config: live_config(&rt.config, rt.model_port.as_ref()),
        }),
        vec![
            rt.config.api_key.text.clone(),
            rt.config.onebot_token.text.clone(),
        ],
    ));
    backend.attach_runtime(rt.engine.clone(), stop.clone());
    let updater = qq_inner_core::update::Updater::new(
        fs::canonicalize(&root)?, std::env::current_exe()?,
        qq_inner_core::update::REVISION.into(), stop,
    )?;
    let control = Server::bind(&rt.config.data_dir, Arc::new(qq_inner_core::update::Admin {
        backend: backend.clone(), updater,
    }), events)?;
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
                _ = tick.tick() => {
                    rt.engine.sync_connection();
                    if rt.engine.participates() { rt.schedule_backfill(false); }
                    rt.engine.tick()?;
                },
                _ = report.tick() => rt.report()?,
                _ = prune.tick() => rt.maintain(true)?,
                _ = watcher.tick() => {
                    if let Some(revision) = changed(&root, &rt.applied)? {
                        rt.reloading = true;
                        let attempted_revision = revision.clone();
                        if let Err(error) = rt.reload(&root, revision, &mut shutdown, &mut report).await {
                            rt.reload_error = Some("Invalid configuration; previous settings remain active.");
                            let details = error.to_string();
                            let failure = details.strip_prefix("plugin batch rejected: ")
                                .and_then(|body| serde_json::from_str::<Value>(body).ok());
                            rt.plugin_reload_failure = failure.as_ref().and_then(|value| {
                                Some(json!({"pluginId":value["id"].as_str()?, "code":value["code"].as_str()?}))
                            });
                            if let Some(failure) = &rt.plugin_reload_failure {
                                if rt.last_alerted_plugin_revision.as_deref() != Some(attempted_revision.as_str()) {
                                    (rt.log)("plugin_hot_reload_failed", failure.clone());
                                    rt.last_alerted_plugin_revision = Some(attempted_revision);
                                }
                            } else {
                                (rt.log)("config_reload_rejected", json!({}));
                            }
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
    #[test]
    fn onebot_io_path_is_explicit_and_defaults_to_legacy() {
        assert!(!use_onebot_io(None));
        assert!(!use_onebot_io(Some("0")));
        assert!(use_onebot_io(Some("1")));
    }
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
                runtime_revision(&self.0).unwrap(),
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
    fn model_port_is_opt_in_and_legacy_remains_default() {
        let dir = Temp::new();
        dir.config(json!({}));
        assert!(dir.runtime().model_port.is_none());
        dir.config(json!({"provider":{"ioPort":"ollama","baseUrl":"http://127.0.0.1:11434","model":"test"}}));
        let runtime = dir.runtime();
        assert_eq!(qq_inner_core::engine::io::ModelPort::manifest(runtime.model_port.as_ref().unwrap().as_ref()).name, "ollama");
    }

    #[tokio::test]
    async fn ported_group_join_notice_reaches_engine_and_unknown_preserves_state() {
        use qq_inner_core::engine::{io::{adapters::onebot::OneBotAdapter, ChatAddress, Notice, NoticeKind}, ChatState};
        let dir = Temp::new();
        dir.config(json!({"onebot":{"selfId":"99"},"agent":{"allowedGroups":["42"],
            "observation":{"enabled":true}}}));
        let mut rt = dir.runtime();
        let action = Arc::new(|_, _| Box::pin(async { qq_inner_core::engine::io::PortReply::Ok(Value::Null) }) as futures_util::future::BoxFuture<'static, qq_inner_core::engine::io::PortReply<Value>>);
        rt.onebot_io = Some(Arc::new(OneBotAdapter::new("onebot", action,
            Arc::new(|| (true, true, Some("99".into())))).unwrap()));
        let state = ChatState { pending: true, pause_done: false, ..Default::default() };
        rt.engine.inherit_chats(vec![("group:42".into(), state)]);
        rt.store.lock().unwrap().ensure_orientation("group:42", 1_700_000_000.).unwrap();
        let before = rt.store.lock().unwrap().orientation_state("group:42").unwrap().unwrap().epoch;

        rt.notice(Notification::Event(json!({"post_type":"notice","notice_type":"group_increase",
            "group_id":42,"user_id":99,"self_id":99,"time":1_700_000_001}))).await;
        let joined = rt.store.lock().unwrap().orientation_state("group:42").unwrap().unwrap().epoch;
        assert!(joined > before, "ported notice must advance the orientation epoch");
        assert!(!rt.engine.state("group:42").unwrap().pending, "ported notice must clear pending");

        let unknown = Notice { kind: NoticeKind::Unknown("group_recall".into()),
            chat: Some(ChatAddress::new(qq_inner_core::engine::io::ChatKind::Group, "onebot", "42").unwrap()),
            actor: Some("99".into()), self_id: Some("99".into()), at: 1_700_000_002. };
        rt.engine.ingest_notice(&unknown).unwrap();
        assert_eq!(rt.store.lock().unwrap().orientation_state("group:42").unwrap().unwrap().epoch, joined);
        assert!(!rt.engine.state("group:42").unwrap().pending);
    }

    #[tokio::test]
    async fn configured_chat_binding_rejects_inbound_rebind_and_logs_it() {
        use qq_inner_core::engine::io::{adapters::onebot::OneBotAdapter, ChatPort, PortEvent, PortReply};
        let dir = Temp::new();
        dir.config(json!({"onebot":{"selfId":"99"},"agent":{"allowedGroups":["42"]}}));
        let loaded = config::load_with_env(&dir.0, |_| None).unwrap();
        fs::create_dir_all(&loaded.config.data_dir).unwrap();
        let logs = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let recorded = logs.clone();
        let log: Logger = Arc::new(move |event, data| recorded.lock().unwrap().push((event.into(), data)));
        let rt = Runtime::new(loaded, Arc::new(Mutex::new(Store::in_memory().unwrap())),
            runtime_revision(&dir.0).unwrap(), Arc::new(|| 1_700_000_000.125), log, None).unwrap();
        let make_port = |name| -> Arc<OneBotAdapter> {
            let action = Arc::new(|_, _| Box::pin(async { PortReply::Ok(Value::Null) })
                as futures_util::future::BoxFuture<'static, PortReply<Value>>);
            Arc::new(OneBotAdapter::new(name, action,
                Arc::new(|| (true, true, Some("99".into())))).unwrap())
        };
        let primary: Arc<dyn ChatPort> = make_port("onebot");
        rt.engine.set_chat_port(primary.clone());
        rt.engine.bind_configured_chats().unwrap();
        let other = make_port("other");
        let event = json!({"post_type":"message","message_type":"group","group_id":42,
            "user_id":7,"self_id":99,"message_id":123,"time":1_700_000_000,
            "sender":{"nickname":"群友"},"message":"你好"});
        let Some(PortEvent::Incoming(incoming)) = other.map_event(&event, 1_700_000_000.125).unwrap() else {
            panic!("expected incoming message");
        };
        rt.engine.ingest_incoming(&incoming, other).unwrap();
        assert!(Arc::ptr_eq(&primary, &rt.engine.chat_port_for("group:42").unwrap()));
        assert!(logs.lock().unwrap().iter().any(|(name, value)| name == "chat_port_rebind_ignored"
            && value["chat"] == "group:42" && value["bound"] == "onebot" && value["incoming"] == "other"));
    }

    #[tokio::test]
    async fn rejected_event_keeps_original_log_with_message_attribution() {
        let dir = Temp::new();
        dir.config(json!({"onebot":{"selfId":"99"},"agent":{"allowedUsers":["1"]}}));
        let mut rt = dir.runtime();
        let logs = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let captured = logs.clone();
        rt.log = Arc::new(move |event, data| captured.lock().unwrap().push((event.into(), data)));
        rt.store.lock().unwrap().connection().execute_batch("DROP TABLE messages").unwrap();
        rt.notice(Notification::Event(json!({"post_type":"message","message_type":"private",
            "self_id":99,"user_id":1,"message_id":42,"time":1_700_000_000,
            "message":"hello"}))).await;
        let logs = logs.lock().unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0, "event_rejected");
        assert_eq!(logs[0].1["chat"], "private:1");
        assert_eq!(logs[0].1["raw_message_id"], 42);
        assert_eq!(logs[0].1["reason"], "ingest_failed");
        assert!(logs[0].1["error"].as_str().unwrap().contains("no such table: messages"));
    }

    #[tokio::test]
    async fn pause_keeps_ingesting_but_stopped_participation_does_not() {
        let dir = Temp::new();
        dir.config(json!({"onebot":{"selfId":"99"},"agent":{"allowedUsers":["1"]}}));
        let mut rt = dir.runtime();
        let (stop, _) = watch::channel(false);
        let backend = Backend::new(rt.config.data_dir.clone(), rt.store.clone(), Arc::new(LiveRemote {
            bot: rt.bot.clone(), provider: rt.provider.clone(), model_port: rt.model_port.clone(), config: rt.config.clone(),
        }), vec![]);
        backend.attach_runtime(rt.engine.clone(), stop);
        let event = |id| json!({"post_type":"message","message_type":"private",
            "self_id":99,"user_id":1,"message_id":id,"time":1_700_000_000,"message":"hello"});
        qq_inner_core::control::Handler::request(&backend, "agent.pause", json!({})).await.unwrap();
        assert_eq!(rt.engine.participation_state()["effective"], "paused");
        rt.notice(Notification::Event(event(1))).await;
        assert_eq!(rt.store.lock().unwrap().rows("SELECT id FROM messages", []).unwrap().len(), 1);
        qq_inner_core::control::Handler::request(&backend, "agent.stop_participation", json!({})).await.unwrap();
        rt.notice(Notification::Event(event(2))).await;
        assert_eq!(rt.store.lock().unwrap().rows("SELECT id FROM messages", []).unwrap().len(), 1);
        qq_inner_core::control::Handler::request(&backend, "agent.resume", json!({})).await.unwrap();
        rt.notice(Notification::Event(event(3))).await;
        assert_eq!(rt.store.lock().unwrap().rows("SELECT id FROM messages", []).unwrap().len(), 2);
    }

    #[test]
    fn revision_and_write_barrier() {
        let dir = Temp::new();
        dir.config(json!({}));
        let applied = runtime_revision(&dir.0).unwrap();
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
        let applied = runtime_revision(&dir.0).unwrap();
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
        rt.engine.set_manual_mode(qq_inner_core::engine::ManualMode::Paused);
        dir.config(json!({"agent":{"allowedUsers":["1"],"dryRun":true}}));
        let (_stop, mut shutdown) = watch::channel(false);
        let revision = runtime_revision(&dir.0).unwrap();
        rt.reload(&dir.0, revision.clone(), &mut shutdown, &mut timer(5))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&store, &rt.engine.store));
        assert!(!Arc::ptr_eq(&old_engine, &rt.engine));
        assert!(Arc::ptr_eq(&old_bot, &rt.bot));
        assert_eq!(rt.applied, revision);
        assert_eq!(rt.engine.manual_mode(), qq_inner_core::engine::ManualMode::Paused);
        assert_eq!(rt.store.lock().unwrap().manual_participation_mode().unwrap(), "normal");
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
                runtime_revision(&dir.0).unwrap(),
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
                runtime_revision(&dir.0).unwrap(),
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
            runtime_revision(&dir.0).unwrap(),
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
            runtime_revision(&dir.0).unwrap(),
            &mut shutdown,
            &mut timer(5),
        )
        .await
        .unwrap();
        assert_eq!(rt.applied, applied);
        rt.shutdown().await.unwrap();
        assert!(store.upgrade().is_none());
    }

    // JS status() oracle 已随 JS 实现归档到 js-legacy 分支;status.json 契约由仪表盘与下方测试覆盖。

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
                assert_eq!(v.as_object().unwrap().len(), 22);
                assert_eq!(v["participation"]["effective"], "normal");
                assert_eq!(v["runningRevision"], qq_inner_core::update::REVISION);
                assert_eq!(v["prompts"]["source"], "builtin");
                assert!(v["prompts"]["revision"].is_null());
                assert!(!v["prompts"]["errors"].as_array().unwrap().is_empty());
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

    // Goal design: docs/DEVELOPMENT.md:351 (UPDATE-2). A real engine send is
    // held inside a mock OneBot; shutdown must await its ACK before disconnecting.
    #[tokio::test]
    async fn agent_exit_reuses_update_shutdown_drain_before_disconnect_without_replay() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dir = Temp::new();
        dir.config(json!({
            "provider":{"model":"fixture"},
            "onebot":{"url":format!("ws://{}/", listener.local_addr().unwrap())},
            "agent":{"allowedUsers":["1950202917"],"schedule":{"enabled":false},
                "identity":{"enabled":false},"ownerTeaching":{"enabled":true},
                "backfill":{"enabled":false},"proactive":false}
        }));
        fs::write(dir.0.join("secrets.json"), r#"{"apiKey":"fixture"}"#).unwrap();
        let mut rt = dir.runtime();
        let abort = rt.abort.subscribe();
        let store = rt.store.clone();
        let (sent, mut received) = mpsc::unbounded_channel();
        let (release, mut released) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(socket).await.unwrap();
            let mut count = 0;
            while let Some(Ok(Message::Text(frame))) = ws.next().await {
                let request: Value = serde_json::from_str(&frame).unwrap();
                let data = match request["action"].as_str().unwrap() {
                    "get_login_info" => json!({"user_id":99}),
                    "get_status" => json!({"online":true}),
                    "send_private_msg" => {
                        count += 1;
                        sent.send(()).unwrap();
                        released.recv().await.unwrap();
                        json!({"message_id":1234})
                    }
                    _ => json!({}),
                };
                ws.send(Message::Text(json!({"echo":request["echo"],"status":"ok","retcode":0,"data":data}).to_string())).await.unwrap();
            }
            count
        });
        rt.connect();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !rt.bot.state().online { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        rt.engine.ingest(&json!({"post_type":"message", "message_type":"private",
            "self_id":99,"user_id":1950202917u64,"message_id":1,
            "time":1700000000,"message":"/忘记 临时测试","sender":{"nickname":"Owner"}})).unwrap();
        assert_eq!(
            store.lock().unwrap().rows("SELECT received_at FROM messages WHERE id='1'", []).unwrap()[0]["received_at"],
            1_700_000_000.125
        );
        rt.engine.set_manual_mode(qq_inner_core::engine::ManualMode::Paused);
        rt.engine.tick().unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(50), received.recv()).await.is_err(),
            "paused agent must not send");
        rt.engine.set_manual_mode(qq_inner_core::engine::ManualMode::Normal);
        rt.engine.tick().unwrap();
        tokio::time::timeout(Duration::from_secs(3), received.recv()).await.unwrap().unwrap();
        assert_eq!(store.lock().unwrap().rows("SELECT status FROM deliveries", []).unwrap()[0]["status"], "pending");
        let (stop, mut requested) = watch::channel(false);
        let backend = Backend::new(rt.config.data_dir.clone(), store.clone(), Arc::new(LiveRemote {
            bot: rt.bot.clone(), provider: rt.provider.clone(), model_port: rt.model_port.clone(), config: rt.config.clone(),
        }), vec![]);
        backend.attach_runtime(rt.engine.clone(), stop);
        let result = qq_inner_core::control::Handler::request(&backend, "agent.exit", json!({})).await.unwrap();
        assert_eq!(result["draining"], true);
        tokio::time::timeout(Duration::from_secs(3), requested.changed()).await.unwrap().unwrap();
        assert!(*requested.borrow());
        drop(backend);
        let shutdown = tokio::spawn(rt.shutdown());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!*abort.borrow(), "OneBot disconnected while send was in flight");
        assert!(!shutdown.is_finished());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = store.lock().unwrap().rows("SELECT status FROM deliveries", []).unwrap();
                if rows[0]["status"] == "sent" { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        store.lock().unwrap().recover_deliveries().unwrap();
        assert_eq!(store.lock().unwrap().rows("SELECT status, message_id FROM deliveries", []).unwrap(),
            vec![json!({"status":"sent","message_id":"1234"})]);
        drop(store);
        tokio::time::timeout(Duration::from_secs(3), shutdown).await.unwrap().unwrap().unwrap();
        assert_eq!(server.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn backfill_runs_on_connect_timer_and_reconnect() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dir = Temp::new();
        dir.config(json!({
            "onebot":{"url":format!("ws://{}/", listener.local_addr().unwrap()),"reconnectMaxSeconds":1},
            "agent":{"allowedGroups":["10","11"],"backfill":{"intervalSeconds":1,"count":7}}
        }));
        let (requests, mut received) = mpsc::unbounded_channel();
        let (close, mut closing) = mpsc::unbounded_channel::<()>();
        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = accept_async(socket).await.unwrap();
                loop {
                    let frame = tokio::select! {
                        frame = ws.next() => frame,
                        _ = closing.recv() => { let _ = ws.close(None).await; break; }
                    };
                    let Some(Ok(Message::Text(frame))) = frame else { break; };
                    let req: Value = serde_json::from_str(&frame).unwrap();
                    let (status, code, data) = match req["action"].as_str().unwrap() {
                        "get_login_info" => ("ok", 0, json!({"user_id":99})),
                        "get_status" => ("ok", 0, json!({"online":true})),
                        "get_group_msg_history" => {
                            requests.send(req["params"].clone()).unwrap();
                            if req["params"]["group_id"] == "10" {
                                // One group failing does not starve the next group.
                                ("failed", 1, Value::Null)
                            } else {
                                ("ok", 0, json!({"messages":[null, {
                                    "message_id":123,"user_id":20,"time":1699990000,
                                    "sender":{"nickname":"Human"},"message":"[CQ:at,qq=99]历史消息"
                                }]}))
                            }
                        }
                        other => panic!("unexpected action: {other}"),
                    };
                    if ws.send(Message::Text(json!({"echo":req["echo"],"status":status,"retcode":code,"data":data}).to_string())).await.is_err() {
                        break;
                    }
                }
            }
        });
        async fn pass(rt: &mut Runtime, received: &mut mpsc::UnboundedReceiver<Value>) {
            tokio::time::timeout(Duration::from_secs(5), async {
                for group in ["10", "11"] {
                    loop {
                        tokio::select! {
                            Some(params) = received.recv() => {
                                assert_eq!(params, json!({"group_id":group,"count":7}));
                                break;
                            }
                            Some(notice) = rt.notices.recv() => rt.notice(notice).await,
                            _ = tokio::time::sleep(Duration::from_millis(10)) => rt.schedule_backfill(false),
                        }
                    }
                }
                rt.drain_backfill().await;
            }).await.unwrap();
        }
        let mut rt = dir.runtime();
        rt.connect();
        pass(&mut rt, &mut received).await;
        let first_due = rt.next_backfill;
        pass(&mut rt, &mut received).await;
        assert!(rt.next_backfill > first_due);
        assert_eq!(rt.store.lock().unwrap().history("group:11", None).unwrap().len(), 1);
        assert!(rt.engine.chats().is_empty());
        // Move the timer far away: only a fresh connection may trigger this pass.
        rt.next_backfill = Instant::now() + Duration::from_secs(3600);
        close.send(()).unwrap();
        pass(&mut rt, &mut received).await;
        assert_eq!(rt.store.lock().unwrap().history("group:11", None).unwrap().len(), 1);
        assert!(rt.engine.chats().is_empty());
        rt.config.agent.backfill.enabled = false;
        rt.schedule_backfill(true);
        assert!(rt.backfill.is_none());
        rt.shutdown().await.unwrap();
        server.abort();
        let _ = server.await;
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

    #[tokio::test]
    async fn running_revision_ignores_event_before_matching_response() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let dir = Temp::new();
        let listener = tokio::net::UnixListener::bind(dir.0.join("control.sock")).unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = BufReader::new(socket);
            let mut request = String::new();
            socket.read_line(&mut request).await.unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["id"], "full-update");
            assert_eq!(request["method"], "state.get");
            socket.get_mut().write_all(b"{\"event\":\"onebot\",\"data\":{\"state\":\"websocket_error\"}}\n").await.unwrap();
            socket.get_mut().write_all(b"{\"id\":\"full-update\",\"ok\":true,\"result\":{\"runningRevision\":\"revision-after-event\"}}\n").await.unwrap();
        });
        assert_eq!(running_revision(&dir.0).await.as_deref(), Some("revision-after-event"));
        server.await.unwrap();
    }

    // Usability: prove the production run() wires update methods into its control bus.
    #[test]
    fn runtime_control_bus_exposes_update_management() {
        use std::io::{BufRead, BufReader};
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let dir = Temp::new();
        dir.config(json!({"agent":{"dryRun":true}}));
        let _child = Child(Process::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::signal_child", "--nocapture"])
            .env("QIA_RUNTIME_SIGNAL_TEST", &dir.0)
            .stdout(Stdio::null()).spawn().unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            if let Ok(socket) = std::os::unix::net::UnixStream::connect(dir.0.join("state/control.sock")) {
                break socket;
            }
            assert!(std::time::Instant::now() < deadline, "runtime socket not ready");
            std::thread::sleep(Duration::from_millis(10));
        };
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        writeln!(socket, "{}", json!({"id":"update", "method":"update.status"})).unwrap();
        let mut reader = BufReader::new(socket);
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == "update" {
                assert_eq!(reply["ok"], true);
                assert_eq!(reply["result"]["runningRevision"], qq_inner_core::update::REVISION);
                assert_eq!(reply["result"]["result"], "never_updated");
                break;
            }
        }
        // VM observation: data/status.json lacked runningRevision. The CLI must
        // read the status file through the live service's control socket.
        assert_eq!(tokio::runtime::Runtime::new().unwrap().block_on(running_revision(&dir.0.join("state"))),
            Some(qq_inner_core::update::REVISION.into()));
        let status: Value = serde_json::from_slice(&fs::read(dir.0.join("state/status.json")).unwrap()).unwrap();
        assert_eq!(status["runningRevision"], qq_inner_core::update::REVISION);
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
