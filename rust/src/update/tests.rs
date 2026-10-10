use super::*;
use crate::control::{Events, Server};
use std::{
    os::unix::fs::PermissionsExt,
    process::Child,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    remote: PathBuf,
    old: String,
    build: PathBuf,
}
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let base = PathBuf::from("/tmp").join(format!("qq-update-{}", rand::random::<u64>()));
        let remote = base.join("remote");
        let root = base.join("service");
        fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-b", "main"]);
        git(&remote, &["config", "user.email", "test@example.invalid"]);
        git(&remote, &["config", "user.name", "Update Test"]);
        fs::create_dir(remote.join("rust")).unwrap();
        fs::write(remote.join("rust/placeholder"), "fixture").unwrap();
        fs::write(remote.join(".gitignore"), ".runtime/\nrust/target/\n").unwrap();
        fs::write(remote.join("artifact"), Self::artifact("old")).unwrap();
        git(&remote, &["add", "."]);
        git(&remote, &["commit", "-m", "old"]);
        let old = git(&remote, &["rev-parse", "HEAD"]);
        git(
            &base,
            &["clone", remote.to_str().unwrap(), root.to_str().unwrap()],
        );
        fs::create_dir_all(root.join("rust/target/release")).unwrap();
        executable(
            &root.join("rust/target/release/qq-inner-core"),
            &Self::artifact("old").replace("REVISION_PLACEHOLDER", &old),
        );
        let build = base.join("fake-cargo");
        executable(&build, "#!/bin/sh\nset -eu\nprintf build >> ../.runtime/build-count\nmkdir -p \"$6/release\"\nsed \"s/REVISION_PLACEHOLDER/$QQ_BUILD_REVISION/g\" ../artifact > \"$6/release/qq-inner-core\"\nchmod 755 \"$6/release/qq-inner-core\"\n");
        Self {
            base,
            root,
            remote,
            old,
            build,
        }
    }
    fn artifact(label: &str) -> String {
        format!("#!/bin/sh\n# {label} build artifact\nif [ \"${{1:-}}\" = build-revision ]; then echo REVISION_PLACEHOLDER; exit; fi\nexport QQ_FIXTURE_REV=REVISION_PLACEHOLDER\nexec \"$QQ_FIXTURE_TEST_EXE\" --exact update::tests::service_child --nocapture\n")
    }
    fn publish(&self) -> String {
        fs::write(self.remote.join("artifact"), Self::artifact("new")).unwrap();
        git(&self.remote, &["add", "."]);
        git(&self.remote, &["commit", "-m", "new"]);
        git(&self.remote, &["rev-parse", "HEAD"])
    }
    fn updater(&self) -> (Arc<Updater>, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        let mut updater = Updater::new(
            self.root.clone(),
            self.root.join("rust/target/release/qq-inner-core"),
            self.old.clone(),
            tx,
        )
        .unwrap();
        Arc::get_mut(&mut updater).unwrap().build = self.build.clone();
        (updater, rx)
    }
    fn binary(&self) -> Vec<u8> {
        fs::read(self.root.join("rust/target/release/qq-inner-core")).unwrap()
    }
    fn spawn(&self) -> ChildGuard {
        ChildGuard(
            Command::new(self.root.join("rust/target/release/qq-inner-core"))
                .env("QQ_FIXTURE_ROOT", &self.root)
                .env("QQ_FIXTURE_BUILD", &self.build)
                .env("QQ_FIXTURE_TEST_EXE", std::env::current_exe().unwrap())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct NoBackend;
impl Handler for NoBackend {
    fn request<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unknown_method") })
    }
}
async fn done(updater: &Updater) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let s = updater.status();
            if s["busy"] == false || s["result"] == "restart_pending" {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn clean_no_change_never_builds_and_dirty_is_rejected() {
    let f = Fixture::new();
    let (u, restart) = f.updater();
    u.start("update.apply").unwrap();
    assert_eq!(done(&u).await["result"], "already_latest");
    assert!(!f.root.join(".runtime/build-count").exists());
    fs::write(f.root.join("untracked"), "dirty").unwrap();
    u.start("update.apply").unwrap();
    assert_eq!(done(&u).await["error"], "dirty_worktree");
    assert!(!*restart.borrow());
}
#[tokio::test]
async fn build_failure_preserves_binary_and_removes_partial_output() {
    let f = Fixture::new();
    f.publish();
    let old = f.binary();
    executable(&f.build, "#!/bin/sh\nmkdir -p \"$6/release\"\necho partial > \"$6/release/qq-inner-core\"\necho compiler-error >&2\nexit 9\n");
    let (u, restart) = f.updater();
    u.start("update.apply").unwrap();
    let status = done(&u).await;
    assert_eq!(status["result"], "failed");
    assert!(status["error"].as_str().unwrap().contains("build_failed"));
    assert_eq!(f.binary(), old);
    assert!(!f.root.join(".runtime/update/build").exists());
    assert!(fs::read_to_string(f.root.join(".runtime/update/build.log"))
        .unwrap()
        .contains("compiler-error"));
    assert!(!*restart.borrow());
}
#[tokio::test]
async fn success_installs_exact_artifact_and_retains_backup() {
    let f = Fixture::new();
    let revision = f.publish();
    let old = f.binary();
    let mut old_inode = fs::File::open(f.root.join("rust/target/release/qq-inner-core")).unwrap();
    let (u, mut restart) = f.updater();
    u.start("update.apply").unwrap();
    assert!(u.start("update.rollback").is_err());
    let s = done(&u).await;
    restart.changed().await.unwrap();
    assert_eq!(s["installedRevision"], revision);
    assert_eq!(
        f.binary(),
        Fixture::artifact("new")
            .replace("REVISION_PLACEHOLDER", &revision)
            .as_bytes()
    );
    assert_eq!(
        fs::read(s["previousBinary"].as_str().unwrap()).unwrap(),
        old
    );
    let mut retained = Vec::new();
    std::io::Read::read_to_end(&mut old_inode, &mut retained).unwrap();
    assert_eq!(retained, old, "open inode must retain the old executable");
    assert_eq!(s["runningRevision"], f.old);
    assert!(!f.root.join(".runtime/update/build").exists());
}

// A reused test executable is the fixture's service implementation. No nested Cargo
// compilation and no network: both executable versions are fake-build shell launchers.
#[test]
fn service_child() {
    let Ok(root) = std::env::var("QQ_FIXTURE_ROOT") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let root = PathBuf::from(root);
        let (tx, mut rx) = watch::channel(false);
        let revision = std::env::var("QQ_FIXTURE_REV").unwrap();
        let mut updater = Updater::new(
            root.clone(),
            root.join("rust/target/release/qq-inner-core"),
            revision,
            tx,
        )
        .unwrap();
        Arc::get_mut(&mut updater).unwrap().build =
            std::env::var("QQ_FIXTURE_BUILD").unwrap().into();
        let server = Server::bind(
            &root.join(".runtime/update"),
            Arc::new(Admin {
                backend: Arc::new(NoBackend),
                updater,
            }),
            Events::default(),
        )
        .unwrap();
        rx.changed().await.unwrap();
        server.stop().await;
    });
}
async fn call(root: &Path, method: &str) -> Value {
    let mut socket = tokio::net::UnixStream::connect(root.join(".runtime/update/control.sock"))
        .await
        .unwrap();
    socket
        .write_all(format!("{{\"id\":\"test\",\"method\":\"{method}\"}}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}
async fn ready(f: &Fixture, revision: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let socket =
                tokio::net::UnixStream::connect(f.root.join(".runtime/update/control.sock")).await;
            if socket.is_ok() {
                let status = call(&f.root, "update.status").await;
                assert_eq!(status["result"]["runningRevision"], revision);
                return status["result"].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn exited(child: &mut ChildGuard) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(
            Instant::now() < deadline,
            "service did not exit for restart"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
// Goal design: docs/DEVELOPMENT.md:349 (UPDATE-1). Exercise the actual
// control bus, disk install, process exit, supervisor relaunch and rollback.
#[tokio::test]
async fn control_entry_restarts_new_artifact_and_rollback_restarts_old_artifact() {
    let f = Fixture::new();
    let old = f.binary();
    let revision = f.publish();
    let mut process = f.spawn();
    let initial = ready(&f, &f.old).await;
    assert_eq!(
        call(&f.root, "update.apply").await["result"]["accepted"],
        true
    );
    exited(&mut process).await;
    assert_eq!(
        f.binary(),
        Fixture::artifact("new")
            .replace("REVISION_PLACEHOLDER", &revision)
            .as_bytes()
    );
    // Emulate Restart=always: same ExecStart path, fresh process after old process exits.
    let mut process = f.spawn();
    let updated = ready(&f, &revision).await;
    assert_ne!(updated["pid"], initial["pid"]);
    assert_eq!(updated["result"], "completed");
    assert_eq!(
        fs::read(updated["previousBinary"].as_str().unwrap()).unwrap(),
        old
    );
    assert_eq!(
        call(&f.root, "update.rollback").await["result"]["accepted"],
        true
    );
    exited(&mut process).await;
    assert_eq!(f.binary(), old);
    let _process = f.spawn();
    let rolled_back = ready(&f, &f.old).await;
    assert_ne!(rolled_back["pid"], updated["pid"]);
    assert_eq!(rolled_back["previousRevision"], revision);
    assert_eq!(rolled_back["result"], "completed");
}

#[tokio::test]
async fn diverged_branch_does_not_build_or_change_binary() {
    let f = Fixture::new();
    f.publish();
    git(&f.root, &["config", "user.email", "test@example.invalid"]);
    git(&f.root, &["config", "user.name", "Test"]);
    fs::write(f.root.join("local"), "local commit").unwrap();
    git(&f.root, &["add", "local"]);
    git(&f.root, &["commit", "-m", "diverge"]);
    let before = git(&f.root, &["rev-parse", "HEAD"]);
    let old = f.binary();
    let (u, restart) = f.updater();
    u.start("update.apply").unwrap();
    assert_eq!(done(&u).await["result"], "failed");
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), before);
    assert_eq!(f.binary(), old);
    assert!(!f.root.join(".runtime/build-count").exists());
    assert!(!*restart.borrow());
}

#[tokio::test]
async fn invalid_candidate_and_missing_backup_fail_without_restart() {
    let f = Fixture::new();
    f.publish();
    let old = f.binary();
    executable(&f.build, "#!/bin/sh\nset -eu\nmkdir -p \"$6/release\"\ncp ../rust/target/release/qq-inner-core \"$6/release/qq-inner-core\"\n");
    let (u, restart) = f.updater();
    u.start("update.apply").unwrap();
    assert_eq!(done(&u).await["error"], "candidate_revision_mismatch");
    assert_eq!(f.binary(), old);
    assert!(!f
        .root
        .join("rust/target/release/qq-inner-core.update.tmp")
        .exists());
    u.start("update.rollback").unwrap();
    assert_eq!(done(&u).await["error"], "no_previous_binary");
    assert!(!*restart.borrow());
    let (tx, _) = watch::channel(false);
    let reloaded = Updater::new(f.root.clone(), u.binary.clone(), f.old.clone(), tx).unwrap();
    assert_eq!(reloaded.status()["error"], "no_previous_binary");
}

#[tokio::test]
async fn management_socket_routes_requests_and_preserves_existing_backend() {
    let f = Fixture::new();
    let (u, _) = f.updater();
    let server = Server::bind(
        &u.dir,
        Arc::new(Admin {
            backend: Arc::new(NoBackend),
            updater: u.clone(),
        }),
        Events::default(),
    )
    .unwrap();
    assert_eq!(
        call(&f.root, "update.status").await["result"]["runningRevision"],
        f.old
    );
    assert_eq!(
        call(&f.root, "existing.backend.method").await["error"]["code"],
        "unknown_method"
    );
    assert_eq!(
        call(&f.root, "update.apply").await["result"]["accepted"],
        true
    );
    assert_eq!(done(&u).await["result"], "already_latest");
    assert_eq!(
        call(&f.root, "update.rollback").await["result"]["accepted"],
        true
    );
    assert_eq!(done(&u).await["error"], "no_previous_binary");
    server.stop().await;
}
