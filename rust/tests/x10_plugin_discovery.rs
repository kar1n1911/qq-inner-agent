//! X10 acceptance tests, written before the discovery/reconcile implementation.
//! They exercise the same CLI path planned for startup, rather than a JSON parser.

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("qq-inner-x10-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).expect("create fixture root");
        Self(root)
    }

    fn write_config(&self, value: Value) -> PathBuf {
        let path = self.0.join("plugins.json");
        fs::write(
            &path,
            serde_json::to_vec(&value).expect("serialize fixture"),
        )
        .expect("write fixture config");
        path
    }

    fn reconcile(&self, config: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_qq-inner-core"))
            .args(["plugins", "reconcile", "--root"])
            .arg(&self.0)
            .arg("--config")
            .arg(config)
            .arg("--json")
            .output()
            .expect("run plugin reconcile CLI")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected machine-readable plugin status: {error}; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn auto_reports_directory_without_manifest_as_unrecognized() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.0.join("mystery")).expect("create unrecognized package");
    let config = fixture.write_config(json!({"schema_version": 1, "mode": "auto", "plugins": []}));

    let output = fixture.reconcile(&config);
    assert!(
        output.status.success(),
        "automatic discovery must report unknown directories; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = stdout_json(&output);
    let discovery = status["discovery"]
        .as_array()
        .expect("discovery status array");
    assert!(
        discovery.iter().any(|entry| {
            entry["directory"] == "mystery"
                && entry["state"] == "unrecognized"
                && entry["reason"] == "manifest_missing"
        }),
        "missing manifest must be visible with a reason: {status}"
    );
}

#[test]
fn manual_disable_is_queryable_and_does_not_load_plugin() {
    let fixture = Fixture::new();
    let package = fixture.0.join("muted");
    fs::create_dir(&package).expect("create plugin directory");
    fs::write(
        package.join("manifest.json"),
        serde_json::to_vec(&json!({
            "name": "muted",
            "version": "0.1.0",
            "kind": "Persona",
            "transport": "Sidecar",
            "core_api_version": {"major": 1, "min_minor": 0, "max_minor": null},
            "protocol": {"name": "fixture", "schema_version": "1", "schema": "fixture"},
            "entry": {"executable": "sidecar/run.sh", "args": []},
            "capabilities": {"supported": [], "unsupported": [], "unknown": []},
            "dependencies": [],
            "host_permissions": [],
            "config_schema": []
        }))
        .expect("serialize manifest"),
    )
    .expect("write manifest");
    fs::create_dir(package.join("sidecar")).expect("create sidecar directory");
    let marker = fixture.0.join("sidecar-started");
    fs::write(
        package.join("sidecar/run.sh"),
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .expect("write sidecar fixture");
    fs::set_permissions(
        package.join("sidecar/run.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("make sidecar fixture executable");
    let config = fixture.write_config(json!({
        "schema_version": 1,
        "mode": "manual",
        "plugins": [{"id": "quiet", "directory": "muted", "enabled": false,
                     "params": {}, "bindings": {}, "secret_refs": {}}]
    }));

    let output = fixture.reconcile(&config);
    assert!(
        output.status.success(),
        "disabled plugin must remain inspectable; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status = stdout_json(&output);
    let quiet = status["plugins"]
        .as_array()
        .expect("plugin status array")
        .iter()
        .find(|entry| entry["id"] == "quiet")
        .expect("disabled plugin status");
    assert_eq!(quiet["requested_enabled"], false);
    assert_eq!(quiet["effective_state"], "disabled");
    assert_eq!(quiet["reason"], "disabled_by_user");
    assert!(!status["registered"]
        .as_array()
        .expect("registered IDs")
        .contains(&json!("quiet")));
    assert!(!marker.exists(), "disabled plugin entry must never execute");
}

#[test]
fn manual_missing_directory_fails_with_identified_plugin() {
    let fixture = Fixture::new();
    let config = fixture.write_config(json!({
        "schema_version": 1,
        "mode": "manual",
        "plugins": [{"id": "absent-instance", "directory": "not-installed", "enabled": true,
                     "params": {}, "bindings": {}, "secret_refs": {}}]
    }));

    let output = fixture.reconcile(&config);
    assert!(
        !output.status.success(),
        "missing manual plugin must reject reconciliation"
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|parse_error| {
        panic!(
            "expected structured startup failure identifying the plugin: {parse_error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(error["code"], "plugin_directory_missing");
    assert_eq!(error["id"], "absent-instance");
    assert_eq!(error["directory"], "not-installed");
}
