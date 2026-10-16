//! X10 plugin discovery and atomic activation planning. The loader is supplied by the host;
//! reading a manifest never executes plugin code or grants a permission.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Clone)]
struct Package {
    directory: String,
    manifest: Option<Value>,
    reason: Option<&'static str>,
}

/// A validated instance request. References to secrets are passed through; secret values are
/// never read from the deployment list or included in status output.
#[derive(Clone, Debug)]
pub struct ActivationRequest {
    pub id: String,
    pub directory: String,
    pub manifest: Value,
    pub params: Value,
    pub bindings: Value,
    pub secret_refs: Value,
}

fn scan(root: &Path) -> BTreeMap<String, Package> {
    let mut packages = BTreeMap::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() || kind.is_symlink() {
                continue;
            }
            let directory = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path().join("manifest.json");
            let (manifest, reason) = if !path.is_file() {
                (None, Some("missing_manifest"))
            } else {
                match fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                {
                    Some(value) if valid_manifest(&directory, &value) => (Some(value), None),
                    _ => (None, Some("invalid_manifest")),
                }
            };
            packages.insert(
                directory.clone(),
                Package {
                    directory,
                    manifest,
                    reason,
                },
            );
        }
    }
    packages
}

fn valid_manifest(directory: &str, value: &Value) -> bool {
    let object = match value.as_object() {
        Some(object) => object,
        None => return false,
    };
    let nonempty = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s != "TBD")
    };
    nonempty("name")
        && value["name"] == directory
        && nonempty("version")
        && matches!(
            value["kind"].as_str(),
            Some("Chat" | "Model" | "Db" | "Memory" | "External" | "Persona")
        )
        && matches!(
            value["transport"].as_str(),
            Some("InProcess" | "Sidecar" | "Remote")
        )
        && value["core_api_version"]["major"].as_u64() == Some(1)
        && value["core_api_version"]["min_minor"].as_u64().is_some()
        && value.get("host_permissions").is_some_and(Value::is_array)
        && value.get("dependencies").is_some_and(Value::is_array)
        && value
            .get("config_schema")
            .is_some_and(|v| v.is_array() || v.is_object())
        && value
            .get("capabilities")
            .is_some_and(|v| v.is_array() || v.is_object())
        && value["status"] != "placeholder"
        && !contains_tbd(value)
}

fn contains_tbd(value: &Value) -> bool {
    match value {
        Value::String(text) => text == "TBD",
        Value::Array(items) => items.iter().any(contains_tbd),
        Value::Object(fields) => fields.values().any(contains_tbd),
        _ => false,
    }
}

fn capabilities(value: &Value) -> Option<HashSet<String>> {
    let list = if value.is_array() {
        value.as_array()?
    } else {
        value["supported"].as_array()?
    };
    list.iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn safe_directory(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

fn valid_instance(manifest: &Value, entry: &Value, enabled: bool) -> bool {
    if enabled && manifest["transport"] != "InProcess"
        && (manifest["protocol"].is_null() || manifest["entry"].is_null())
    {
        return false;
    }
    let params = entry["params"].as_object();
    let refs = entry["secret_refs"].as_object();
    let bindings = entry["bindings"].as_object();
    let empty = Map::new();
    let params = params.unwrap_or(&empty);
    let refs = refs.unwrap_or(&empty);
    let bindings = bindings.unwrap_or(&empty);
    let schema: BTreeMap<String, Value> = if let Some(fields) = manifest["config_schema"].as_array()
    {
        fields
            .iter()
            .filter_map(|field| Some((field["key"].as_str()?.to_owned(), field.clone())))
            .collect()
    } else {
        manifest["config_schema"]
            .as_object()
            .into_iter()
            .flat_map(|fields| fields.iter())
            .map(|(key, field)| (key.clone(), field.clone()))
            .collect()
    };
    for (key, value) in params {
        let Some(field) = schema.get(key) else {
            return false;
        };
        if field["secret"] == true {
            return false;
        }
        let correct_type = match field["type"].as_str() {
            Some("string") => value.is_string(),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            Some("object") => value.is_object(),
            Some("array") => value.is_array(),
            _ => false,
        };
        if !correct_type {
            return false;
        }
    }
    for (key, value) in refs {
        if schema.get(key).is_none_or(|field| field["secret"] != true)
            || value.as_str().is_none_or(str::is_empty)
        {
            return false;
        }
    }
    for (key, field) in &schema {
        if enabled && field["required"] == true && !params.contains_key(key) && !refs.contains_key(key) {
            return false;
        }
    }
    let dependencies = manifest["dependencies"]
        .as_array()
        .expect("validated manifest");
    for (role, target) in bindings {
        if target.as_str().is_none_or(str::is_empty)
            || !dependencies
                .iter()
                .any(|dependency| dependency["role"] == role.as_str())
        {
            return false;
        }
    }
    dependencies.iter().all(|dependency| {
        !enabled || dependency["required"] != true
            || dependency["role"]
                .as_str()
                .is_some_and(|role| bindings.contains_key(role))
    })
}

/// Resolve discovery and requested activation. All entries are preflighted before the first
/// loader call; the callback must perform real activation and return verified capabilities.
/// The caller must not publish a partially effective generation if any loader fails.
pub fn resolve(
    root: &Path,
    list: &Value,
    load: &mut impl FnMut(&str) -> Result<Value, String>,
) -> Value {
    resolve_with(root, list, &mut |request| load(&request.id))
}

/// Rich host entry: requests carry ordered instance configuration to the trusted factory.
pub fn resolve_with(
    root: &Path,
    list: &Value,
    load: &mut impl FnMut(&ActivationRequest) -> Result<Value, String>,
) -> Value {
    resolve_inner(root, list, load, false)
}

/// Validate the complete deployment request without constructing or reporting
/// any instance as active. Startup uses this before opening runtime resources.
pub fn preflight(root: &Path, list: &Value) -> Value {
    resolve_inner(root, list, &mut |_| unreachable!("preflight never loads"), true)
}

fn resolve_inner(
    root: &Path,
    list: &Value,
    load: &mut impl FnMut(&ActivationRequest) -> Result<Value, String>,
    preflight_only: bool,
) -> Value {
    let packages = scan(root);
    let mut states = Map::new();
    for (name, package) in &packages {
        states.insert(
            name.clone(),
            json!({
                "installed": package.manifest.is_some(), "effective": false,
                "reason": package.reason.unwrap_or("not_enabled"),
                "directory": package.directory
            }),
        );
    }
    let version = list
        .get("schema_version")
        .or_else(|| list.get("version"))
        .and_then(Value::as_u64);
    let mode = list["mode"].as_str();
    let entries = list["plugins"].as_array();
    if version != Some(1) || !matches!(mode, Some("auto" | "manual")) || entries.is_none() {
        return json!({"schema_version": 1, "mode": mode, "plugins": states,
            "registered": [], "reason": "invalid_plugin_list"});
    }
    let mut selected = Vec::new();
    let mut invalid = false;
    let mut seen_ids = HashSet::new();
    for entry in entries.expect("checked above") {
        let Some(id) = entry["id"].as_str() else {
            invalid = true;
            continue;
        };
        let directory = entry["directory"].as_str().unwrap_or(id);
        if !safe_directory(directory)
            || !seen_ids.insert(id.to_owned())
            || !entry["enabled"].is_boolean()
            || !["params", "bindings", "secret_refs"]
                .iter()
                .all(|field| entry.get(*field).is_none_or(Value::is_object))
        {
            invalid = true;
            states.insert(id.to_owned(), json!({"installed":false,"effective":false,"reason":"invalid_entry","directory":directory}));
            continue;
        }
        let Some(package) = packages.get(directory) else {
            states.insert(id.to_owned(), json!({"installed":false,"effective":false,"reason":"missing_directory","directory":directory}));
            invalid = true;
            continue;
        };
        if let Some(reason) = package.reason {
            states.insert(
                id.to_owned(),
                json!({"installed":false,"effective":false,"reason":reason,"directory":directory}),
            );
            invalid = true;
            continue;
        }
        if !valid_instance(
            package.manifest.as_ref().expect("validated package"),
            entry,
            entry["enabled"] == true,
        ) {
            states.insert(id.to_owned(), json!({"installed":true,"effective":false,"reason":"invalid_config","directory":directory}));
            invalid = true;
            continue;
        }
        if entry["enabled"] == false {
            states.insert(id.to_owned(), json!({"installed":true,"effective":false,"reason":"disabled_by_user","directory":directory}));
        } else {
            states.insert(id.to_owned(), json!({"installed":true,"effective":false,"reason":"preflight_pending","directory":directory}));
            selected.push(ActivationRequest {
                id: id.to_owned(),
                directory: directory.to_owned(),
                manifest: package.manifest.clone().expect("validated package"),
                params: entry.get("params").cloned().unwrap_or_else(|| json!({})),
                bindings: entry.get("bindings").cloned().unwrap_or_else(|| json!({})),
                secret_refs: entry
                    .get("secret_refs")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            });
        }
    }
    if mode == Some("manual") {
        for (name, status) in &mut states {
            if !seen_ids.contains(name) && status["reason"] == "not_enabled" {
                status["reason"] = json!("not_selected");
            }
        }
    }
    let selected_order: BTreeMap<&str, usize> = selected
        .iter()
        .enumerate()
        .map(|(index, request)| (request.id.as_str(), index))
        .collect();
    for (index, request) in selected.iter().enumerate() {
        if request.bindings.as_object().is_some_and(|bindings| {
            bindings.values().any(|target| {
                target
                    .as_str()
                    .and_then(|id| selected_order.get(id))
                    .is_none_or(|position| *position >= index)
            })
        }) {
            states[&request.id]["reason"] = json!("invalid_order");
            invalid = true;
        }
    }
    if invalid {
        for request in &selected {
            if states[&request.id]["reason"] == "preflight_pending" {
                states[&request.id]["reason"] = json!("preflight_failed");
            }
        }
        return json!({"schema_version":1,"mode":mode,"plugins":states,"registered":[],"reason":"preflight_failed"});
    }
    if preflight_only {
        return json!({"schema_version":1,"mode":mode,"plugins":states,"registered":[]});
    }
    let mut registered = Vec::new();
    let mut activation_failed = false;
    for request in selected {
        let id = request.id.clone();
        let claimed = capabilities(&request.manifest["capabilities"]);
        let result = load(&request);
        match result {
            Ok(actual) => {
                let actual = capabilities(&actual["capabilities"]);
                if claimed
                    .zip(actual)
                    .is_some_and(|(claimed, actual)| claimed.is_subset(&actual))
                {
                    states[&id]["effective"] = json!(true);
                    states[&id]["reason"] = json!("active");
                    registered.push(id);
                } else {
                    states[&id]["reason"] = json!("capability_mismatch");
                    activation_failed = true;
                }
            }
            Err(_) => {
                states[&id]["reason"] = json!("load_failed");
                activation_failed = true;
            }
        }
    }
    if activation_failed {
        for id in &registered {
            states[id]["effective"] = json!(false);
            states[id]["reason"] = json!("activation_aborted");
        }
        registered.clear();
    }
    json!({"schema_version":1,"mode":mode,"plugins":states,"registered":registered})
}
