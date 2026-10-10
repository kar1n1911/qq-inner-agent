//! Human-triggered deployments. Cargo never writes to the executable used by ExecStart.
use crate::control::Handler;
use anyhow::{ensure, Context, Result};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::watch;

pub const REVISION: &str = env!("QQ_BUILD_REVISION");

pub struct Updater {
    root: PathBuf,
    binary: PathBuf,
    dir: PathBuf,
    running: String,
    state: Mutex<Value>,
    busy: AtomicBool,
    restart: watch::Sender<bool>,
    build: PathBuf,
    installed: AtomicBool,
}
impl Updater {
    pub fn new(
        root: PathBuf,
        binary: PathBuf,
        running: String,
        restart: watch::Sender<bool>,
    ) -> Result<Arc<Self>> {
        let dir = root.join(".runtime/update");
        fs::create_dir_all(&dir)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let mut state: Value = match fs::read(dir.join("status.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("invalid update status")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({
                "installedRevision": running, "previousRevision": null,
                "previousBinary": null, "result": "never_updated", "error": null
            }),
            Err(e) => return Err(e.into()),
        };
        ensure!(state.is_object(), "invalid update status object");
        if matches!(
            state["result"].as_str(),
            Some("running" | "installing" | "restart_pending")
        ) {
            state["result"] = json!(if state["targetRevision"] == running {
                "completed"
            } else {
                "interrupted"
            });
        }
        // The embedded revision is authoritative after restart, including recovery
        // from a crash between the executable rename and the final status write.
        state["installedRevision"] = json!(running);
        Ok(Arc::new(Self {
            root,
            binary,
            dir,
            running,
            state: Mutex::new(state),
            busy: AtomicBool::new(false),
            restart,
            build: "cargo".into(),
            installed: AtomicBool::new(false),
        }))
    }
    pub fn status(&self) -> Value {
        let mut status = self.state.lock().unwrap().clone();
        status["runningRevision"] = json!(self.running);
        status["busy"] = json!(self.busy.load(Ordering::SeqCst));
        status["pid"] = json!(std::process::id());
        status
    }
    fn save(&self, state: Value) -> Result<()> {
        let tmp = self.dir.join("status.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&state)?)?;
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(tmp, self.dir.join("status.json"))?;
        fs::File::open(&self.dir)?.sync_all()?;
        *self.state.lock().unwrap() = state;
        Ok(())
    }
    fn git(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()?;
        ensure!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }
    fn clean(&self) -> Result<()> {
        ensure!(
            self.git(&["status", "--porcelain", "--untracked-files=all"])?
                .is_empty(),
            "dirty_worktree"
        );
        Ok(())
    }
    fn install(&self, source: &Path, revision: &str) -> Result<()> {
        // Stage alongside the destination: rename is atomic even if dataDir is on another FS.
        let stage = self.binary.with_extension("update.tmp");
        let result = (|| -> Result<()> {
            let live = Command::new(&self.binary).arg("build-revision").output()?;
            ensure!(
                live.status.success()
                    && String::from_utf8_lossy(&live.stdout).trim() == self.running,
                "installed_binary_changed"
            );
            fs::copy(source, &stage)?;
            fs::File::open(&stage)?.sync_all()?;
            let probe = Command::new(&stage).arg("build-revision").output()?;
            ensure!(
                probe.status.success() && String::from_utf8_lossy(&probe.stdout).trim() == revision,
                "candidate_revision_mismatch"
            );
            let previous = self.dir.join(format!("previous-{}", rand::random::<u64>()));
            fs::copy(&self.binary, &previous)?;
            fs::File::open(&previous)?.sync_all()?;
            let mut state = self.state.lock().unwrap().clone();
            state["previousRevision"] = json!(self.running);
            state["previousBinary"] = json!(previous);
            state["targetRevision"] = json!(revision);
            state["result"] = json!("installing");
            self.save(state.clone())?;
            fs::rename(&stage, &self.binary)?;
            self.installed.store(true, Ordering::SeqCst);
            fs::File::open(self.binary.parent().context("binary parent")?)?.sync_all()?;
            state["installedRevision"] = json!(revision);
            state["result"] = json!("restart_pending");
            state["finishedAt"] = json!(chrono::Utc::now().to_rfc3339());
            self.save(state)?;
            Ok(())
        })();
        if stage.exists() {
            fs::remove_file(stage)?;
        }
        result
    }
    fn apply(&self) -> Result<bool> {
        self.clean()?;
        let before = self.git(&["rev-parse", "HEAD"])?;
        self.git(&["pull", "--ff-only"])?;
        let revision = self.git(&["rev-parse", "HEAD"])?;
        if revision == before {
            return Ok(false);
        }
        self.clean()?;
        let target = self.dir.join("build");
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        let result = (|| -> Result<()> {
            let log = fs::File::create(self.dir.join("build.log"))?;
            let status = Command::new(&self.build)
                .current_dir(self.root.join("rust"))
                .args([
                    "build",
                    "--release",
                    "--bin",
                    "qq-inner-core",
                    "--target-dir",
                ])
                .arg(&target)
                .env("QQ_BUILD_REVISION", &revision)
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .status()?;
            ensure!(
                status.success(),
                "build_failed ({status}); see .runtime/update/build.log"
            );
            self.clean()?;
            ensure!(
                self.git(&["rev-parse", "HEAD"])? == revision,
                "source_changed_during_build"
            );
            self.install(&target.join("release/qq-inner-core"), &revision)?;
            Ok(())
        })();
        if target.exists() {
            fs::remove_dir_all(target)?;
        }
        result?;
        Ok(true)
    }
    fn rollback(&self) -> Result<bool> {
        let state = self.state.lock().unwrap().clone();
        let previous = state["previousBinary"]
            .as_str()
            .context("no_previous_binary")?;
        let revision = state["previousRevision"]
            .as_str()
            .context("no_previous_revision")?;
        self.install(Path::new(previous), revision)?;
        Ok(true)
    }
    pub fn start(self: &Arc<Self>, method: &str) -> Result<Value> {
        ensure!(
            matches!(method, "update.apply" | "update.rollback"),
            "unknown_method"
        );
        ensure!(!self.busy.swap(true, Ordering::SeqCst), "update_busy");
        let updater = self.clone();
        let rollback = method == "update.rollback";
        tokio::task::spawn_blocking(move || {
            let mut state = updater.state.lock().unwrap().clone();
            state["operation"] = json!(if rollback { "rollback" } else { "apply" });
            state["result"] = json!("running");
            state["error"] = Value::Null;
            state["targetRevision"] = Value::Null;
            state["finishedAt"] = Value::Null;
            state["startedAt"] = json!(chrono::Utc::now().to_rfc3339());
            let result = updater.save(state).and_then(|_| {
                if rollback {
                    updater.rollback()
                } else {
                    updater.apply()
                }
            });
            let restart = updater.installed.load(Ordering::SeqCst);
            if !matches!(result, Ok(true)) {
                let mut state = updater.state.lock().unwrap().clone();
                match result {
                    Ok(false) => state["result"] = json!("already_latest"),
                    Err(e) => {
                        state["result"] = json!("failed");
                        state["error"] = json!(format!("{e:#}"));
                    }
                    _ => unreachable!(),
                }
                state["finishedAt"] = json!(chrono::Utc::now().to_rfc3339());
                if let Err(e) = updater.save(state.clone()) {
                    *updater.state.lock().unwrap() = state;
                    eprintln!("update status write failed: {e:#}");
                }
            }
            if restart {
                // Remain busy until process exit, so no second installation can race restart.
                updater.restart.send_replace(true);
            } else {
                updater.busy.store(false, Ordering::SeqCst);
            }
        });
        Ok(json!({"accepted":true,"statusMethod":"update.status"}))
    }
}

pub struct Admin {
    pub backend: Arc<dyn Handler>,
    pub updater: Arc<Updater>,
}
impl Handler for Admin {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            match method {
                "update.status" => Ok(self.updater.status()),
                "update.apply" | "update.rollback" => self.updater.start(method),
                _ => self.backend.request(method, params).await,
            }
        })
    }
}

#[cfg(test)]
mod tests;
