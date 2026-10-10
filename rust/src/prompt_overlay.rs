//! Immutable prompt snapshot, replaced only with the configuration/Engine generation.
use crate::{prompts, settings};
use serde_json::{json, Value};
use std::{collections::HashMap, fs, path::Path};

pub const FILE: &str = ".runtime/prompts.json";

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    values: HashMap<String, String>,
    revision: Option<String>,
    errors: Vec<String>,
}

// Read failures are part of the watched state, never a fatal configuration error.
fn read(root: &Path) -> Result<Vec<u8>, String> {
    fs::read(root.join(FILE)).map_err(|e| format!("{FILE}: {:?}", e.kind()))
}
pub fn revision(root: &Path) -> String {
    match read(root) {
        Ok(bytes) => settings::sha256(&bytes),
        Err(error) => error,
    }
}
impl Snapshot {
    pub fn load(root: &Path) -> Self {
        let mut snapshot = Self::default();
        let bytes = match read(root) {
            Ok(bytes) => bytes,
            Err(error) => {
                snapshot.errors.push(error);
                return snapshot;
            }
        };
        snapshot.revision = Some(settings::sha256(&bytes));
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(Value::Object(object)) => Value::Object(object),
            _ => {
                snapshot
                    .errors
                    .push("prompts.json must be a JSON object".into());
                return snapshot;
            }
        };
        for (key, _) in prompts::DEFAULT_ENTRIES {
            if let Some(text) = value[key].as_str() {
                snapshot.values.insert((*key).into(), text.into());
            } else {
                snapshot
                    .errors
                    .push(format!("{key}: missing or non-string; using builtin"));
            }
        }
        snapshot
    }
    pub fn watch_revision(&self) -> &str {
        self.revision
            .as_deref()
            .unwrap_or_else(|| self.errors.first().map_or("builtin", String::as_str))
    }
    pub fn status(&self) -> Value {
        json!({"source":if self.values.is_empty() {"builtin"} else {"overlay"},
            "revision":self.revision, "errors":self.errors,
            "overriddenKeys":self.values.keys().collect::<std::collections::BTreeSet<_>>()})
    }
    pub fn get<'a>(&'a self, default: &'a str) -> &'a str {
        prompts::DEFAULT_ENTRIES
            .iter()
            .find(|(_, text)| *text == default)
            .and_then(|(key, _)| self.values.get(*key))
            .map_or(default, String::as_str)
    }
    pub fn compose_prompt(&self, contract: &str, disabled_rules: &[&str]) -> String {
        let names = prompts::TASK_RULES
            .iter()
            .find(|(text, _)| *text == contract)
            .map_or(
                &["BOUNDARY", "RESPONSIBILITY", "ATTRIBUTION"][..],
                |(_, names)| *names,
            );
        let mut parts = vec![
            self.get(prompts::IDENTITY),
            self.get(prompts::OUTPUT_CONTRACT),
            self.get(contract),
        ];
        for key in names {
            let default = prompts::DEFAULT_ENTRIES
                .iter()
                .find(|(k, _)| k == key)
                .expect("generated key")
                .1;
            let name = prompts::RULES
                .iter()
                .find(|(_, text)| *text == default)
                .expect("generated rule")
                .0;
            if !disabled_rules.contains(&name) {
                parts.push(self.get(default));
            }
        }
        parts.join("\n")
    }
    pub fn articulation_for(&self, language: &str) -> Result<String, &'static str> {
        let instruction = match prompts::ReplyLanguage::parse(language) {
            Some(prompts::ReplyLanguage::Auto) => prompts::INSTRUCTION_AUTO,
            Some(prompts::ReplyLanguage::ZhCn) => prompts::INSTRUCTION_ZH_CN,
            Some(prompts::ReplyLanguage::En) => prompts::INSTRUCTION_EN,
            None => return Err("Invalid reply language"),
        };
        Ok(format!(
            "{}\n{}",
            self.compose_prompt(prompts::ARTICULATION, &[]),
            self.get(instruction)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_falls_back_per_key_and_never_rejects_bad_files() {
        let root = std::env::temp_dir().join(format!("prompts-{}", crate::store::uuid()));
        fs::create_dir_all(root.join(".runtime")).unwrap();
        let missing = Snapshot::load(&root);
        assert_eq!(missing.get(prompts::IDENTITY), prompts::IDENTITY);
        assert_eq!(missing.status()["source"], "builtin");
        assert!(!missing.status()["errors"].as_array().unwrap().is_empty());
        fs::write(
            root.join(FILE),
            r#"{"IDENTITY":"new identity","FORMATION":false}"#,
        )
        .unwrap();
        let partial = Snapshot::load(&root);
        assert_eq!(partial.get(prompts::IDENTITY), "new identity");
        assert_eq!(partial.get(prompts::FORMATION), prompts::FORMATION);
        assert_eq!(partial.get(prompts::ARTICULATION), prompts::ARTICULATION);
        assert_eq!(partial.status()["source"], "overlay");
        assert!(partial.status()["errors"].as_array().unwrap().len() > 1);
        for invalid in ["{", "[]", "null"] {
            fs::write(root.join(FILE), invalid).unwrap();
            let bad = Snapshot::load(&root);
            assert_eq!(
                bad.compose_prompt(prompts::FORMATION, &[]),
                prompts::compose_prompt(prompts::FORMATION, &[])
            );
            assert_eq!(bad.status()["source"], "builtin");
            assert_ne!(bad.status()["revision"], partial.status()["revision"]);
            assert!(!bad.status()["errors"].as_array().unwrap().is_empty());
        }
        fs::remove_file(root.join(FILE)).unwrap();
        fs::create_dir(root.join(FILE)).unwrap();
        assert_eq!(Snapshot::load(&root).status()["source"], "builtin");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn every_generated_key_is_overridable_and_defaults_compose_identically() {
        let root = std::env::temp_dir().join(format!("prompts-{}", crate::store::uuid()));
        fs::create_dir_all(root.join(".runtime")).unwrap();
        let defaults: serde_json::Map<String, Value> = prompts::DEFAULT_ENTRIES
            .iter()
            .map(|(k, v)| ((*k).into(), json!(v)))
            .collect();
        settings::atomic_json(&root.join(FILE), &json!(defaults)).unwrap();
        let snapshot = Snapshot::load(&root);
        assert_eq!(snapshot.status()["errors"], json!([]));
        for (contract, _) in prompts::TASK_RULES {
            assert_eq!(
                snapshot.compose_prompt(contract, &[]),
                prompts::compose_prompt(contract, &[])
            );
        }
        for language in ["auto", "zh-CN", "en"] {
            assert_eq!(
                snapshot.articulation_for(language),
                prompts::articulation_for(language)
            );
        }
        let overrides: serde_json::Map<String, Value> = prompts::DEFAULT_ENTRIES
            .iter()
            .map(|(k, _)| ((*k).into(), json!(format!("edited {k}"))))
            .collect();
        settings::atomic_json(&root.join(FILE), &json!(overrides)).unwrap();
        let edited = Snapshot::load(&root);
        for (key, default) in prompts::DEFAULT_ENTRIES {
            assert_eq!(edited.get(default), format!("edited {key}"));
        }
        assert!(edited
            .articulation_for("en")
            .unwrap()
            .contains("edited INSTRUCTION_EN"));
        fs::remove_dir_all(root).unwrap();
    }
}
