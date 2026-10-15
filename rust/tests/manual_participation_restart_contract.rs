//! AA2 acceptance tests. Every `reboot` opens the same SQLite file through a
//! new Store, Engine, and control Backend; it does not use hot reload.
//! For corruption injection and migration inspection, these independent tests
//! assume a `manual_participation(id=1, mode)` row. Its SQL spelling is a
//! test-author choice; adapt only those probes if the final schema differs.

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    control::{Backend, Handler, Remote},
    engine::{orientation::{OrientationProvider, OrientationTransport}, Engine, EngineTransport, Options},
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::{Path, PathBuf}, process::Command, sync::{Arc, Mutex}};

const TARGET_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Directory(PathBuf);
impl Directory {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("aa2-restart-{}", rand::random::<u64>()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
    fn database(&self) -> PathBuf { self.0.join("agent.sqlite") }
}
impl Drop for Directory {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

struct Peer { state: Mutex<State>, sends: Mutex<usize> }
impl Peer {
    fn new() -> Self {
        Self { state: Mutex::new(State { connected: true, online: true, self_id: "99".into(), ..State::default() }), sends: Mutex::new(0) }
    }
    fn degraded(&self, value: bool) { self.state.lock().unwrap().degraded = value; }
}
impl OrientationProvider for Peer {
    fn json<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected decision while participation is disabled") })
    }
}
impl OrientationTransport for Peer {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected network request") })
    }
}
impl EngineTransport for Peer {
    fn state(&self) -> State { self.state.lock().unwrap().clone() }
    fn send<'a>(
        &'a self, _: &'a str, _: &'a str, _: Option<&'a str>, _: Option<&'a str>, _: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            *self.sends.lock().unwrap() += 1;
            Ok(json!({"message_id":"unexpected"}))
        })
    }
}
impl Remote for Peer {
    fn state(&self) -> State { self.state.lock().unwrap().clone() }
    fn request<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected remote control request") })
    }
}

struct Runtime {
    engine: Arc<Engine>,
    backend: Backend,
    store: Arc<Mutex<Store>>,
    peer: Arc<Peer>,
}

fn start(dir: &Directory) -> Result<Runtime> {
    let config = Config::from_value(&merge(&defaults(), &json!({
        "apiKey":"test-only","onebotToken":"","dataDir":dir.0,
        "provider":{"model":"fixture"},
        "agent":{"allowedGroups":["10"],"quietHours":null,
            "observation":{"enabled":false},"identity":{"enabled":false},
            "learning":{"enabled":false},"emoji":{"learnFrequency":false}}
    })))?;
    let store = Arc::new(Mutex::new(Store::open(dir.database())?));
    let peer = Arc::new(Peer::new());
    let engine = Engine::new(config, store.clone(), peer.clone(), peer.clone(),
        Options { now: Arc::new(|| 10_000.), ..Options::default() })?;
    let backend = Backend::new(dir.0.clone(), store.clone(), peer.clone(), vec![]);
    let (shutdown, _) = tokio::sync::watch::channel(false);
    backend.attach_runtime(engine.clone(), shutdown);
    Ok(Runtime { engine, backend, store, peer })
}

async fn command(runtime: &Runtime, method: &str) -> Result<Value> {
    Handler::request(&runtime.backend, method, json!({})).await
}

fn message(id: &str) -> Value {
    json!({"post_type":"message","message_type":"group","group_id":"10",
        "self_id":"99","user_id":"20","message_id":id,"time":9999,
        "sender":{"nickname":"friend"},"message":"Luma：请回答"})
}

fn message_count(runtime: &Runtime, id: &str) -> Result<i64> {
    Ok(runtime.store.lock().unwrap().rows(
        "SELECT count(*) AS n FROM messages WHERE chat='group:10' AND id=?", [id],
    )?[0]["n"].as_i64().unwrap())
}

#[tokio::test]
async fn pause_stop_and_resume_survive_complete_reconstruction() -> Result<()> {
    for (method, expected) in [
        ("agent.pause", "paused"),
        ("agent.stop_participation", "stopped"),
    ] {
        let dir = Directory::new()?;
        let first = start(&dir)?;
        assert_eq!(command(&first, method).await?["manual"], expected);
        drop(first);
        let second = start(&dir)?;
        assert_eq!(command(&second, "agent.status").await?["manual"], expected,
            "{method} must survive a new Store and Engine");
    }
    let dir = Directory::new()?;
    let first = start(&dir)?;
    command(&first, "agent.stop_participation").await?;
    assert_eq!(command(&first, "agent.resume").await?["manual"], "normal");
    drop(first);
    let second = start(&dir)?;
    assert_eq!(command(&second, "agent.status").await?["manual"], "normal",
        "resume must clear the persisted manual stop");
    Ok(())
}

#[tokio::test]
async fn restored_stop_blocks_ingest_and_decisions_while_pause_ingests_without_sending() -> Result<()> {
    for (method, expected, id) in [
        ("agent.stop_participation", "stopped", "stopped-message"),
        ("agent.pause", "paused", "paused-message"),
    ] {
        let dir = Directory::new()?;
        let first = start(&dir)?;
        command(&first, method).await?;
        drop(first);
        let restored = start(&dir)?;
        assert_eq!(command(&restored, "agent.status").await?["manual"], expected);
        restored.engine.ingest(&message(id))?;
        assert_eq!(message_count(&restored, id)?, i64::from(expected == "paused"),
            "paused observes messages; stopped does not ingest");
        restored.engine.tick()?;
        restored.engine.wait_idle().await;
        assert_eq!(*restored.peer.sends.lock().unwrap(), 0, "neither disabled mode may send");
        assert_eq!(restored.store.lock().unwrap().rows("SELECT count(*) AS n FROM decisions", [])?[0]["n"], 0,
            "disabled participation must not decide to speak");
    }
    Ok(())
}

#[tokio::test]
async fn onebot_recovery_changes_only_automatic_component_not_manual_stop() -> Result<()> {
    let dir = Directory::new()?;
    let first = start(&dir)?;
    command(&first, "agent.stop_participation").await?;
    first.peer.degraded(true);
    first.engine.sync_connection();
    let outage = command(&first, "agent.status").await?;
    assert_eq!(outage["manual"], "stopped");
    assert_eq!(outage["lostOnebot"], true);
    first.peer.degraded(false);
    first.engine.sync_connection();
    let recovered = command(&first, "agent.status").await?;
    assert_eq!(recovered["manual"], "stopped");
    assert_eq!(recovered["lostOnebot"], false);
    assert_eq!(recovered["effective"], "stopped");
    drop(first);
    let restarted = start(&dir)?;
    assert_eq!(command(&restarted, "agent.status").await?["manual"], "stopped");
    Ok(())
}

#[tokio::test]
async fn drain_exit_does_not_create_a_persisted_manual_stop() -> Result<()> {
    let dir = Directory::new()?;
    let first = start(&dir)?;
    assert_eq!(command(&first, "agent.exit").await?["draining"], true);
    assert_eq!(command(&first, "agent.status").await?["effective"], "stopped");
    drop(first);
    let restarted = start(&dir)?;
    assert_eq!(command(&restarted, "agent.status").await?["manual"], "normal",
        "a temporary drain must not become a persistent stop");
    Ok(())
}

#[tokio::test]
async fn failed_persistence_is_reported_instead_of_acknowledging_a_durable_stop() -> Result<()> {
    let dir = Directory::new()?;
    let runtime = start(&dir)?;
    runtime.store.lock().unwrap().execute("PRAGMA query_only=ON", [])?;
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o500))?;
    let attempt = command(&runtime, "agent.stop_participation").await;
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o700))?;
    let error = attempt.expect_err("write failure must not be reported as durable stop");
    assert!(!error.to_string().is_empty());
    drop(runtime);
    let restarted = start(&dir)?;
    assert_ne!(command(&restarted, "agent.status").await?["manual"], "stopped");
    Ok(())
}

#[tokio::test]
async fn unreadable_existing_state_never_silently_becomes_normal() -> Result<()> {
    let dir = Directory::new()?;
    let first = start(&dir)?;
    command(&first, "agent.stop_participation").await?;
    drop(first);
    // Corrupt the existing database, rather than simulating a fresh install.
    fs::write(dir.database(), b"not a sqlite database")?;
    let error = start(&dir).err().context("unreadable state must fail startup")?;
    assert!(!error.to_string().is_empty());
    Ok(())
}

#[tokio::test]
async fn missing_and_illegal_stored_modes_fail_loudly_on_restart() -> Result<()> {
    for sql in [
        "DELETE FROM manual_participation WHERE id=1",
        "UPDATE manual_participation SET mode='not-a-mode' WHERE id=1",
    ] {
        let dir = Directory::new()?;
        let first = start(&dir)?;
        command(&first, "agent.stop_participation").await?;
        drop(first);
        let changed = rusqlite::Connection::open(dir.database())?.execute(sql, [])
            .context("persisted manual state must exist before fault injection")?;
        anyhow::ensure!(changed == 1, "fault injection must alter the existing state");
        let error = start(&dir).err().context("missing/illegal state must not become Normal")?;
        assert!(!error.to_string().is_empty());
    }
    Ok(())
}

fn executable(path: PathBuf, body: &str) -> Result<()> {
    fs::write(&path, body)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn fake_update_commands(dir: &Directory) -> Result<PathBuf> {
    let bin = dir.0.join("fake-bin");
    fs::create_dir_all(&bin)?;
    executable(bin.join("git"), &format!(r#"#!/bin/sh
case "$1" in
  status|pull) exit 0 ;;
  rev-parse) echo '{TARGET_REVISION}'; exit 0 ;;
  diff) if [ "$2" = '--name-status' ]; then printf 'M\trust/src/store/schema.sql\n'; fi; exit 0 ;;
esac
exit 2
"#))?;
    executable(bin.join("systemctl"), "#!/bin/sh\nexit 0\n")?;
    executable(bin.join("cargo"), &format!(r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--target-dir' ]; then shift; target="$1"; break; fi
  shift
done
mkdir -p "$target/release"
printf '#!/bin/sh\necho {TARGET_REVISION}\n' > "$target/release/qq-inner-core"
chmod +x "$target/release/qq-inner-core"
"#))?;
    executable(dir.0.join("installed-agent"), "#!/bin/sh\necho old-revision\n")?;
    fs::create_dir_all(dir.0.join("rust"))?;
    fs::write(dir.0.join("config.json"), br#"{"storage":{"directory":"."}}"#)?;
    Ok(bin)
}

fn update_worker(dir: &Directory, bin: &Path, worker: &str) -> Result<String> {
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", worker, "--nocapture"])
        .env("AA2_UPDATE_ROOT", &dir.0)
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    anyhow::ensure!(output.status.success(), "isolated update failed: {stdout}\n{stderr}");
    Ok(format!("{stdout}\n{stderr}"))
}

// These workers run only as child processes, so replacing PATH never affects
// another Rust test or the actual systemctl/cargo/git binaries.
#[test]
fn full_update_worker() -> Result<()> {
    let Ok(root) = std::env::var("AA2_UPDATE_ROOT") else { return Ok(()); };
    let root = PathBuf::from(root);
    let result = qq_inner_core::update::full_update(
        &root, &root.join("installed-agent"), Some("old-revision"), &root.join("agent.sqlite"),
    )?;
    assert_eq!(result["result"], "completed");
    let again = qq_inner_core::update::full_update(
        &root, &root.join("installed-agent"), Some(TARGET_REVISION), &root.join("agent.sqlite"),
    )?;
    assert!(matches!(again["result"].as_str(), Some("already_latest" | "completed")),
        "repeat full update must succeed safely: {again}");
    Ok(())
}

#[tokio::test]
async fn apply_rejection_worker() -> Result<()> {
    let Ok(root) = std::env::var("AA2_UPDATE_ROOT") else { return Ok(()); };
    let root = PathBuf::from(root);
    let (restart, _) = tokio::sync::watch::channel(false);
    let updater = qq_inner_core::update::Updater::new(
        root.clone(), root.join("installed-agent"), "old-revision".into(), restart,
    )?;
    updater.start("update.apply")?;
    for _ in 0..100 {
        if !updater.status()["busy"].as_bool().unwrap_or(true) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let status = updater.status();
    assert_eq!(status["result"], "failed", "apply must reject schema change");
    assert!(status["error"].as_str().is_some_and(|s| s.contains("full_update")),
        "apply must direct the operator to full update: {status}");
    Ok(())
}

#[test]
fn update_full_preserves_old_data_and_is_idempotent_while_apply_rejects_schema_change() -> Result<()> {
    let dir = Directory::new()?;
    let first = start(&dir)?;
    first.store.lock().unwrap().record_message(&json!({
        "chat":"group:10","id":"legacy-message","sender":"20","name":"friend",
        "text":"preserve me","ts":1000.,"self":false
    }))?;
    drop(first);
    // Strip only the new state from an otherwise valid database to model the
    // pre-AA2 schema. This is a fault-injection seam, not implementation code.
    rusqlite::Connection::open(dir.database())?
        .execute("DROP TABLE IF EXISTS manual_participation", [])?;
    let bin = fake_update_commands(&dir)?;
    update_worker(&dir, &bin, "apply_rejection_worker")?;
    let before_full: i64 = rusqlite::Connection::open(dir.database())?.query_row(
        "SELECT count(*) FROM messages WHERE id='legacy-message'", [], |row| row.get(0),
    )?;
    assert_eq!(before_full, 1, "rejected apply must preserve old data");
    update_worker(&dir, &bin, "full_update_worker")?;
    let migrated = start(&dir)?;
    assert_eq!(message_count(&migrated, "legacy-message")?, 1,
        "full migration must retain existing data");
    assert_eq!(migrated.store.lock().unwrap().rows(
        "SELECT mode FROM manual_participation WHERE id=1", [],
    )?.first().context("migration must create an explicit state row")?["mode"], "normal");
    Ok(())
}
