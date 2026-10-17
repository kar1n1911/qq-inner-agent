use qq_inner_core::engine::io::plugin_list::{preflight, resolve};
use serde_json::{json, Value};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("plugins")
}

#[test]
fn repository_packages_are_visible_and_skeletons_cannot_activate() {
    let root = root();
    let list: Value = serde_json::from_slice(&std::fs::read(root.join("plugins.json")).unwrap()).unwrap();
    assert_eq!(list["mode"], "auto");
    assert!(list["plugins"].as_array().unwrap().iter().all(|entry| entry["enabled"] == false));
    let mut loaded = Vec::new();
    let discovered = resolve(&root, &list, &mut |id| {
        loaded.push(id.to_owned());
        Ok(json!({"capabilities":[]}))
    });
    assert!(loaded.is_empty());
    for name in ["onebot", "a_memorix", "openai", "claude", "sillytavern"] {
        assert_eq!(discovered["plugins"][name]["installed"], true, "{name}");
        assert_eq!(discovered["plugins"][name]["reason"], "disabled_by_user", "{name}");
    }
    for name in ["ollama", "null"] {
        assert_eq!(discovered["plugins"][name]["status"], "skeleton", "{name}");
        assert_eq!(discovered["plugins"][name]["reason"], "plugin_is_skeleton", "{name}");
        let mut enabled = list.clone();
        if let Some(entry) = enabled["plugins"].as_array_mut().unwrap().iter_mut()
            .find(|entry| entry["id"] == name) {
            entry["enabled"] = json!(true);
        } else {
            enabled["plugins"].as_array_mut().unwrap().push(json!({"id":name,"directory":name,"enabled":true}));
        }
        let checked = preflight(&root, &enabled);
        assert_eq!(checked["reason"], "preflight_failed", "{name}");
        assert_eq!(checked["plugins"][name]["reason"], "plugin_is_skeleton", "{name}");
        let resolved = resolve(&root, &enabled, &mut |id| {
            loaded.push(id.to_owned()); Ok(json!({"capabilities":[]}))
        });
        assert_eq!(resolved["plugins"][name]["reason"], "plugin_is_skeleton", "{name}");
        assert!(loaded.is_empty(), "skeleton must never reach loader");
    }
}

#[test]
fn available_packages_pass_manifest_preflight_with_required_configuration() {
    let root = root();
    let mut list: Value = serde_json::from_slice(&std::fs::read(root.join("plugins.json")).unwrap()).unwrap();
    for entry in list["plugins"].as_array_mut().unwrap() {
        let id = entry["id"].as_str().unwrap().to_owned();
        if id == "ollama" { continue; }
        entry["enabled"] = json!(true);
        match id.as_str() {
            "a_memorix" => entry["params"] = json!({"endpoint":"http://127.0.0.1:50051"}),
            "openai" | "claude" => {
                entry["params"] = json!({"base_url":"https://example.invalid","model":"example",
                    "context_window_tokens":4096});
                entry["secret_refs"] = json!({"api_key":"secrets.test_key"});
            }
            "sillytavern" => entry["params"] = json!({"card_root":"/tmp/cards","chat_card_bindings":{}}),
            _ => {}
        }
    }
    let checked = preflight(&root, &list);
    assert!(checked.get("reason").is_none(), "{checked}");
    for name in ["onebot", "a_memorix", "openai", "claude", "sillytavern"] {
        assert_eq!(checked["plugins"][name]["reason"], "preflight_pending", "{name}");
    }
    assert_eq!(checked["plugins"]["ollama"]["reason"], "plugin_is_skeleton");
    assert_eq!(checked["plugins"]["null"]["reason"], "plugin_is_skeleton");
}
