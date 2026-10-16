use qq_inner_core::engine::io::plugin_list::preflight;
use serde_json::json;
use std::fs;

#[test]
fn disabled_instance_defers_required_configuration_but_keeps_field_checks() {
    let root = std::env::temp_dir().join(format!(
        "ab3-disabled-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let package = root.join("example");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("manifest.json"),
        json!({
            "name":"example","version":"1.0.0","kind":"Model","transport":"InProcess",
            "core_api_version":{"major":1,"min_minor":0},"host_permissions":[],
            "dependencies":[{"role":"ModelPort","required":true}],
            "config_schema":[{"key":"model","type":"string","required":true,"secret":false},
                {"key":"api_key","type":"string","required":false,"secret":true}],
            "capabilities":[],"hot_reload":false
        })
        .to_string(),
    )
    .unwrap();
    let mut list = json!({"schema_version":1,"mode":"manual","plugins":[
        {"id":"example","enabled":false,"params":{},"bindings":{},"secret_refs":{}}
    ]});
    assert_eq!(preflight(&root, &list)["plugins"]["example"]["reason"], "disabled_by_user");
    list["plugins"][0]["enabled"] = json!(true);
    assert_eq!(preflight(&root, &list)["plugins"]["example"]["reason"], "invalid_config");
    list["plugins"][0]["enabled"] = json!(false);
    list["plugins"][0]["params"]["api_key"] = json!("raw-key");
    assert_eq!(preflight(&root, &list)["plugins"]["example"]["reason"], "invalid_config");
    fs::remove_dir_all(root).unwrap();
}
