//! X10 manual order and instance parameters reach the trusted loader, not status output.

use qq_inner_core::engine::io::plugin_list::resolve_with;
use serde_json::json;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn manual_order_and_params_reach_loader_without_echoing_secrets() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "qq-inner-plugin-params-{}-{nonce}",
        std::process::id()
    ));
    for name in ["first", "second"] {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            json!({
                "name":name,"version":"0.1.0","kind":"Model","transport":"InProcess",
                "core_api_version":{"major":1,"min_minor":0,"max_minor":null},
                "host_permissions":[],
                "dependencies": if name == "second" { json!([{"role":"ModelPort","needed_for":"completion","required":true}]) } else { json!([]) },
                "config_schema": if name == "second" { json!([
                    {"key":"temperature","type":"number","required":false,"secret":false},
                    {"key":"api_key","type":"string","required":false,"secret":true}
                ]) } else { json!([]) },
                "capabilities":[]
            })
            .to_string(),
        )
        .unwrap();
    }
    let list = json!({"schema_version":1,"mode":"manual","plugins":[
        {"id":"first","enabled":true},
        {"id":"second","enabled":true,"params":{"temperature":0.4},
         "bindings":{"ModelPort":"first"},"secret_refs":{"api_key":"secrets.model"}}
    ]});
    let mut observed = Vec::new();
    let status = resolve_with(&root, &list, &mut |request| {
        observed.push((
            request.id.clone(),
            request.params.clone(),
            request.bindings.clone(),
            request.secret_refs.clone(),
        ));
        Ok(json!({"capabilities":[]}))
    });
    assert_eq!(
        observed
            .iter()
            .map(|item| item.0.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert_eq!(observed[1].1["temperature"], 0.4);
    assert_eq!(observed[1].2["ModelPort"], "first");
    assert_eq!(observed[1].3["api_key"], "secrets.model");
    assert_eq!(status["registered"], json!(["first", "second"]));
    assert!(!status.to_string().contains("secrets.model"));
    fs::remove_dir_all(root).unwrap();
}
