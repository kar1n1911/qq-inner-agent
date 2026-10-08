#[path = "golden/mod.rs"]
mod golden;
use qq_inner_core::{config::*, settings::*};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, process::Command};

// 只在 rust/target 下创建 fixture，不访问真实 data/；析构清理自己创建的目录。
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "config-fixture-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, v: &Value) {
        atomic_json(&self.0.join(name), v).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
const ENV: [&str; 5] = [
    "LLM_API_KEY",
    "DEEPSEEK_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ONEBOT_TOKEN",
];
// 固化金标准保留原始文件字节（含非法 UTF-8）和环境覆盖；路径只替换临时根目录。
fn expected(f: &Fixture, env: &[(&str, &str)], mode: &str) -> Value {
    let input = json!({"mode":mode,"env":env,"config":fs::read(f.0.join("config.json")).ok(),"secrets":fs::read(f.0.join("secrets.json")).ok()});
    let value = golden::expected(include_str!("golden/config.json"), &input);
    serde_json::from_str(&value.to_string().replace("<ROOT>", &f.0.to_string_lossy())).unwrap()
}

// Rust 新增的可选扩展单独测试，旧 JS 金标准继续逐字段覆盖原有协议。
fn legacy_config(mut value: Value) -> Value {
    value["agent"].as_object_mut().unwrap().remove("ocr");
    value["agent"].as_object_mut().unwrap().remove("backfill");
    value["agent"]["observation"].as_object_mut().unwrap().remove("backlogDigest");
    value
}

#[test]
fn load_and_revision_golden() {
    let f = Fixture::new();
    assert_eq!(legacy_config(defaults()), expected(&f, &[], "defaults"));
    let mut fixtures = vec![
        json!({}),
        json!({"provider":{"model":"测试模型","baseUrl":"https://api.deepseek.com/v1"},"agent":{"allowedGroups":[123,"456"],"quietHours":null},"unknown":{"kept":true}}),
        json!({"provider":{"kind":"anthropic","model":"claude","baseUrl":"https://example.org/v1","thinking":"disabled"},"storage":{"directory":"scratch/../other"},"onebot":{"selfId":123},"agent":{"allowedUsers":[[456]],"schedule":{"timezone":"europe/stockholm"}}}),
        json!({"provider":{"baseUrl":"http://127.1:80/v1?","retries":1.5},"agent":{"quietHours":false,"threshold":1.5}}),
        json!({"storage":{"directory":"/tmp/unused-config-path"},"agent":{"persona":LEGACY_PERSONAS[0]}}),
        json!({"agent":{"persona":LEGACY_PERSONAS[1],"quietHours":{"timezone":"UTC"},"emoji":{"symbols":["🙂".repeat(12)]}}}),
    ];
    fixtures.push(json!({"provider":{"model":false,"workspaceId":null,"baseUrl":["https://api.openai.com/v1"]},"agent":{"name":null,"persona":42,"quietHours":0},"onebot":{"url":["ws://localhost:3001/"]}}));
    fixtures.push(json!({"agent":{"aliases":{"length":0}}}));
    let envs = vec![
        vec![],
        vec![
            ("LLM_API_KEY", "global"),
            ("DEEPSEEK_API_KEY", "deep"),
            ("OPENAI_API_KEY", "open"),
            ("ANTHROPIC_API_KEY", "anthropic"),
            ("ONEBOT_TOKEN", "bot"),
        ],
        vec![
            ("LLM_API_KEY", ""),
            ("DEEPSEEK_API_KEY", "deep"),
            ("OPENAI_API_KEY", "open"),
            ("ANTHROPIC_API_KEY", "anthropic"),
        ],
        vec![
            ("LLM_API_KEY", ""),
            ("OPENAI_API_KEY", ""),
            ("ONEBOT_TOKEN", ""),
        ],
    ];
    // 两个文件都缺失也必须与 JS 一致。
    let n = expected(&f, &[], "load");
    let r = load_with_env(&f.0, |_| None).unwrap();
    assert_eq!(legacy_config(r.raw), n["value"]);
    assert_eq!(revision(&f.0).unwrap(), n["revision"]);
    for fixture in fixtures {
        f.write("config.json", &fixture);
        f.write(
            "secrets.json",
            &json!({"apiKey":"saved-secret","onebotToken":"saved-token"}),
        );
        for env in &envs {
            let n = expected(&f, env, "load");
            let r = load_with_env(&f.0, |key| {
                env.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap();
            assert_eq!(n["ok"], true, "{n}");
            assert_eq!(legacy_config(r.raw), n["value"]);
            assert_eq!(revision(&f.0).unwrap(), n["revision"]);
            assert_eq!(json!(readiness(&r.config)), n["missing"]);
            assert!(!r.config.data_dir.exists());
        }
    }
}

#[test]
fn revision_utf8_missing_and_io_errors() {
    let f = Fixture::new();
    assert_eq!(read_json(&f.0.join("missing")).unwrap(), None);
    {
        for bytes in [
            vec![],
            b"\xef\xbb\xbf{}\r\n".to_vec(),
            vec![0xff, 0xed, 0xa0, 0x80, 0xe2, 0x82],
            vec![b'x'; 128],
        ] {
            fs::write(f.0.join("config.json"), bytes).unwrap();
            assert_eq!(
                json!(revision(&f.0).unwrap()),
                expected(&f, &[], "revision")
            );
        }
    }
    fs::write(f.0.join("config.json"), b"invalid").unwrap();
    assert!(read_json(&f.0.join("config.json")).is_err());
    fs::create_dir(f.0.join("secrets.json")).unwrap();
    assert!(revision(&f.0).is_err());
}

#[test]
fn atomic_permissions_and_replacement() {
    let f = Fixture::new();
    let p = f.0.join("secrets.json");
    f.write("secrets.json", &json!({"key":"old"}));
    fs::write(f.0.join("secrets.json.tmp"), b"stale").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            f.0.join("secrets.json.tmp"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    let v = json!({"key":"new 中文🙂"});
    atomic_json(&p, &v).unwrap();
    assert_eq!(read_json(&p).unwrap(), Some(v));
    assert!(!f.0.join("secrets.json.tmp").exists());
    assert!(fs::read(&p).unwrap().ends_with(b"\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn migration_and_merge() {
    for old in LEGACY_PERSONAS {
        let c = merge(&defaults(), &json!({"agent":{"persona":old}}));
        validate(&c).unwrap();
        assert_eq!(c["agent"]["persona"], old);
        assert_eq!(
            normalize(&c).unwrap()["agent"]["persona"],
            defaults()["agent"]["persona"]
        );
        let c = merge(&defaults(), &json!({"agent":{"persona":format!("{old} ")}}));
        assert_eq!(
            normalize(&c).unwrap()["agent"]["persona"],
            format!("{old} ")
        );
    }
    let b = json!({"list":[1,2],"nested":{"a":1}});
    let e = json!({"list":[3],"nested":{"b":2,"__proto__":{"x":1},"constructor":5,"prototype":6},"__proto__":3});
    assert_eq!(merge(&b, &e), json!({"list":[3],"nested":{"a":1,"b":2}}));
    assert_eq!(b, json!({"list":[1,2],"nested":{"a":1}}));
    assert_eq!(
        merge(&json!([1, 2]), &json!({"1":3,"ignored":4})),
        json!([1, 3])
    );
    // JS 只跳过实际遍历的键；整个赋值的新对象内部不递归过滤。
    assert_eq!(
        merge(&json!({}), &json!({"new":{"constructor":7}})),
        json!({"new":{"constructor":7}})
    );
}

#[test]
fn invalid_values_match_golden() {
    let f = Fixture::new();
    let cases = vec![
        json!({"ui":{"language":"fr"}}),
        json!({"agent":{"replyLanguage":"fr"}}),
        json!({"provider":{"kind":"other"}}),
        json!({"provider":{"tokenParameter":"tokens"}}),
        json!({"provider":{"anthropicAuth":"none"}}),
        json!({"provider":{"thinking":true}}),
        json!({"agent":{"rhythm":{"centerProbability":0.8,"edgeProbability":0.7}}}),
        json!({"agent":{"rhythm":{"activeMinSeconds":1201}}}),
        json!({"agent":{"rhythm":{"restMinSeconds":2401}}}),
        json!({"agent":{"schedule":{"activeStart":"23:00"}}}),
        json!({"agent":{"schedule":{"activeStart":"24:00"}}}),
        json!({"agent":{"schedule":{"activeStart":"08:00\n"}}}),
        json!({"agent":{"schedule":{"timezone":"Not/AZone"}}}),
        json!({"agent":{"observation":{"thresholdMode":"all"}}}),
        json!({"agent":{"emoji":{"faceIds":["１２"]}}}),
        json!({"agent":{"emoji":{"symbols":["🙂".repeat(13)]}}}),
        json!({"agent":{"aliases":["\u{feff}"]}}),
        json!({"agent":{"allowedUsers":["01"]}}),
        json!({"onebot":{"selfId":0}}),
        json!({"agent":{"dryRun":1}}),
        json!({"agent":{"quietHours":{"start":1.5}}}),
        json!({"provider":{"baseUrl":"http://example.com"}}),
        json!({"provider":{"baseUrl":"https://user:pass@example.com"}}),
        json!({"provider":{"baseUrl":"https://example.com/?key=secret"}}),
        json!({"onebot":{"url":"wss://example.com/#fragment"}}),
        json!({"agent":{"learning":{"minMessages":1.5}}}),
        json!({"agent":{"personality":{"behavior":"x".repeat(2001)}}}),
        json!({"agent":{"emoji":{"faceIds":["1\n"]}}}),
    ];
    for extra in cases {
        let r = validate(&merge(&defaults(), &extra));
        assert!(r.is_err(), "{extra}");
        {
            f.write("config.json", &extra);
            let n = expected(&f, &[], "validate");
            assert_eq!(n["ok"], false, "{extra}");
            if !n["error"].as_str().unwrap().contains("time zone") {
                assert_eq!(r.unwrap_err().to_string(), n["error"]);
            }
        }
    }
}

#[test]
fn cli_defaults_are_complete_editable_and_secret_free() {
    let f = Fixture::new();
    f.write(
        "secrets.json",
        &json!({"apiKey":"file-secret","onebotToken":"file-token"}),
    );
    f.write("config.json", &json!({"agent":{"name":"local-name"}}));
    let out = Command::new(env!("CARGO_BIN_EXE_qq-inner-core"))
        .arg("--root")
        .arg(&f.0)
        .arg("config-defaults")
        .env("LLM_API_KEY", "env-secret")
        .env("ONEBOT_TOKEN", "env-token")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    for secret in [
        "file-secret",
        "file-token",
        "env-secret",
        "env-token",
        "local-name",
    ] {
        assert!(!text.contains(secret));
    }
    let schema: Value = serde_json::from_str(&text).unwrap();
    for key in ["apiKey", "onebotToken", "dataDir"] {
        assert!(schema.get(key).is_none());
    }
    for pointer in [
        "/onebot/forwardEnabled",
        "/agent/affect",
        "/agent/identity",
        "/agent/relay",
        "/agent/topicSource",
        "/agent/ocr",
        "/agent/backstory",
        "/agent/multiBubble",
        "/agent/memoryRecall",
        "/agent/threeLayerDecision",
        "/agent/ownerTeaching",
        "/agent/observation/backlogDigest",
        "/agent/emoji/learnFrequency",
        "/agent/emoji/faceOnly",
        "/agent/memory/partialEvidence",
    ] {
        assert!(schema.pointer(pointer).is_some(), "missing {pointer}");
    }
    for pointer in [
        "/agent/name",
        "/agent/persona",
        "/provider/model",
        "/provider/workspaceId",
        "/onebot/selfId",
    ] {
        assert_eq!(schema.pointer(pointer), defaults().pointer(pointer));
    }
    f.write("config.json", &schema);
    let loaded = load_with_env(&f.0, |_| None).unwrap();
    assert_eq!(loaded.config.agent.name.text, defaults()["agent"]["name"]);
    assert_eq!(loaded.config.agent.memory.partial_evidence, 2);
}

#[test]
fn cli_redaction_and_selftest() {
    let f = Fixture::new();
    f.write(
        "secrets.json",
        &json!({"apiKey":"secret-api-123","onebotToken":"secret-bot-456"}),
    );
    for command in ["config", "selftest"] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_qq-inner-core"));
        for k in ENV {
            cmd.env_remove(k);
        }
        let out = cmd.arg("--root").arg(&f.0).arg(command).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(!text.contains("secret-api-123"));
        assert!(!text.contains("secret-bot-456"));
    }
    assert!(!f.0.join("data").exists());
}

#[test]
fn backfill_defaults_and_validation() {
    let original = defaults();
    assert_eq!(original["agent"]["backfill"], json!({"enabled":true,"intervalSeconds":60,"count":50}));
    for patch in [json!({}), json!({"enabled":false}), json!({"intervalSeconds":1.5,"count":1})] {
        let value = merge(&original, &json!({"agent":{"backfill":patch}}));
        validate(&value).unwrap();
    }
    for patch in [json!(null), json!(false), json!({"enabled":"true"}), json!({"count":0}), json!({"count":-1}), json!({"count":1.5}), json!({"intervalSeconds":0}), json!({"intervalSeconds":-1}), json!({"intervalSeconds":1e100})] {
        let value = merge(&original, &json!({"agent":{"backfill":patch}}));
        assert!(validate(&value).is_err(), "{value}");
    }
    let mut legacy = original;
    legacy["agent"].as_object_mut().unwrap().remove("backfill");
    legacy["apiKey"] = json!("");
    legacy["onebotToken"] = json!("");
    legacy["dataDir"] = json!("unused");
    let settings = Config::from_value(&legacy).unwrap().agent.backfill;
    assert!(settings.enabled);
    assert_eq!(settings.interval_seconds, 60.);
    assert_eq!(settings.count, 50);
}
