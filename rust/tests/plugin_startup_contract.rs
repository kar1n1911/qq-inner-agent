//! X10 startup assertion: the real `start` path must reject a bad manual list
//! before launching the engine, using the same preflight reason as reconciliation.

use serde_json::{json, Value};
use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn missing_manual_plugin_rejects_real_start_before_runtime() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "qq-inner-plugin-start-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("plugins")).unwrap();
    fs::write(
        root.join("plugins/plugins.json"),
        json!({
            "schema_version":1,"mode":"manual",
            "plugins":[{"id":"gone","directory":"gone","enabled":true}]
        })
        .to_string(),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_qq-inner-core"))
        .args(["--root", root.to_str().unwrap(), "start"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
        panic!("bad plugin list did not stop real startup before the engine ran");
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "plugin_directory_missing");
    assert_eq!(error["id"], "gone");
    assert!(
        !root.join("data").exists(),
        "preflight must precede runtime setup"
    );
    fs::remove_dir_all(root).unwrap();
}
