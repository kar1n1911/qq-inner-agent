//! X10 black-box contract. Intentionally written before the public resolver exists.
//! Only `resolve` in the adapter below is tied to an API shape; the assertions
//! describe observable discovery, loading, status and configuration behavior.
use serde_json::{json, Value};
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root=std::env::temp_dir().join(format!("x10-contract-{}-{}",
            std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn plugin(&self, id:&str, claimed:&[&str]) {
        let dir=self.0.join(id);
        fs::create_dir_all(&dir).unwrap();
        let manifest=json!({"name":id,"version":"0.1.0","kind":"Model","transport":"InProcess",
                "core_api_version":{"major":1,"min_minor":0,"max_minor":null},
                "host_permissions":[],"dependencies":[],"config_schema":{},"capabilities":claimed});
        fs::write(dir.join("manifest.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
    fn directory_without_manifest(&self, id:&str) {
        fs::create_dir_all(self.0.join(id)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { fs::remove_dir_all(&self.0).unwrap(); }
}

fn listing(mode:&str, entries:Value)->Value {
    json!({"version":1,"mode":mode,"plugins":entries})
}

/// Expected public boundary. The callback stands for real activation and
/// returns capabilities verified from the loaded implementation, not its JSON.
fn resolve(root:&Path, list:&Value, loads:&mut Vec<String>, actual:Value)->Value {
    qq_inner_core::engine::io::plugin_list::resolve(root,list,&mut |id:&str| {
        loads.push(id.to_owned());
        Ok(actual.clone())
    })
}

#[test]
fn auto_discovery_reports_new_plugins_and_manifestless_directories() {
    let fixture=Fixture::new();
    fixture.plugin("first",&[]);
    let config=listing("auto",json!([]));
    let mut loads=vec![];
    let before=resolve(&fixture.0,&config,&mut loads,json!({"capabilities":[]}));
    assert_eq!(before["plugins"]["first"]["installed"],true);
    fixture.plugin("second",&[]);
    fixture.directory_without_manifest("broken");
    let after=resolve(&fixture.0,&config,&mut loads,json!({"capabilities":[]}));
    assert_eq!(after["plugins"]["second"]["installed"],true,
        "a newly added directory must appear on the next scan");
    assert_eq!(after["plugins"]["broken"]["reason"],"missing_manifest",
        "a manifestless directory must be visible as an error");
}

#[test]
fn manual_disabled_plugin_is_installed_but_never_loaded() {
    let fixture=Fixture::new();
    fixture.plugin("parked",&[]);
    let config=listing("manual",json!([{"id":"parked","enabled":false}]));
    let mut loads=vec![];
    let status=resolve(&fixture.0,&config,&mut loads,json!({"capabilities":[]}));
    assert_eq!(status["plugins"]["parked"]["installed"],true);
    assert_eq!(status["plugins"]["parked"]["effective"],false);
    assert_eq!(status["plugins"]["parked"]["reason"],"disabled_by_user");
    assert!(loads.is_empty(),"disabled means no activation call");
}

#[test]
fn missing_manual_directory_reports_error_without_losing_valid_plugin() {
    let fixture=Fixture::new();
    fixture.plugin("good",&[]);
    let config=listing("manual",json!([
        {"id":"good","enabled":true},{"id":"gone","enabled":true}]));
    let mut loads=vec![];
    let status=resolve(&fixture.0,&config,&mut loads,json!({"capabilities":[]}));
    assert_eq!(status["plugins"]["gone"]["reason"],"missing_directory");
        // 语义裁决(外层 2026-10-14):预检失败时**原子地不加载任何插件** ——
        // 一个坏配置不得让系统"半启动";但状态报告仍须逐插件完整可见。
        assert_eq!(status["plugins"]["good"]["installed"],true,
            "a valid entry must still be visible even when preflight fails");
        assert_eq!(status["plugins"]["good"]["effective"],false,
            "preflight failure must not produce a partial start");
        assert_eq!(loads,Vec::<String>::new(),
            "a failed preflight must produce no loading side effects");
}

#[test]
fn auto_detected_but_unlisted_plugin_is_visible_and_off_by_default() {
    let fixture=Fixture::new();
    fixture.plugin("listed",&[]);
    fixture.plugin("unlisted",&[]);
    let config=listing("auto",json!([{"id":"listed","enabled":true}]));
    let mut loads=vec![];
    let status=resolve(&fixture.0,&config,&mut loads,json!({"capabilities":[]}));
    assert_eq!(status["plugins"]["listed"]["effective"],true);
    assert_eq!(status["plugins"]["unlisted"]["installed"],true);
    assert_eq!(status["plugins"]["unlisted"]["effective"],false);
    assert_eq!(status["plugins"]["unlisted"]["reason"],"not_enabled");
    assert_eq!(loads,vec!["listed"]);
}

#[test]
fn claimed_capability_must_be_verified_against_loaded_implementation() {
    let fixture=Fixture::new();
    fixture.plugin("overclaim",&["JsonMode"]);
    let config=listing("manual",json!([{"id":"overclaim","enabled":true}]));
    let mut loads=vec![];
    let status=resolve(&fixture.0,&config,&mut loads,json!({"capabilities":[]}));
    assert_eq!(loads,vec!["overclaim"]);
    assert_eq!(status["plugins"]["overclaim"]["effective"],false);
    assert_eq!(status["plugins"]["overclaim"]["reason"],"capability_mismatch",
        "the manifest's claim cannot turn an absent capability into an active one");
}

#[test]
fn json_round_trip_preserves_effective_choices_and_visible_status() {
    let fixture=Fixture::new();
    fixture.plugin("on",&[]);
    fixture.plugin("off",&[]);
    let config=listing("manual",json!([
        {"id":"on","enabled":true},{"id":"off","enabled":false}]));
    let mut first_loads=vec![];
    let first=resolve(&fixture.0,&config,&mut first_loads,json!({"capabilities":[]}));
    let encoded=serde_json::to_string(&config).unwrap();
    let restored:Value=serde_json::from_str(&encoded).unwrap();
    let mut second_loads=vec![];
    let second=resolve(&fixture.0,&restored,&mut second_loads,json!({"capabilities":[]}));
    assert_eq!(first,second,"round-trip must preserve the resolved configuration");
    assert_eq!(first_loads,second_loads);
    assert_eq!(second_loads,vec!["on"]);
    assert_eq!(second["plugins"]["off"]["reason"],"disabled_by_user");
}
