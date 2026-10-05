use qq_inner_core::{
    config::{self, Config, Memory},
    memory::{apply_learning_review, learning_review_input, parse_memory_updates, LayeredMemory},
    store::Store,
};
use serde_json::{json, Value};

fn settings() -> Memory {
    serde_json::from_value(config::defaults()["agent"]["memory"].clone()).unwrap()
}
fn history() -> Vec<Value> {
    (0..8).map(|n| json!({"chat":"group:10","id":format!("m{n}"),"sender":"20","ts":100+n,"text":format!("我喜欢园艺 {n}")})).collect()
}
fn candidate(key: &str, id: usize) -> Value {
    json!({"subject":"person:20","layer":"traits","key":key,"operation":"upsert","text":"喜欢园艺","importance":0.8,"confidence":0.9,"keywords":["园艺"],"sourceIds":[format!("m{id}")]})
}
fn parse(v: Value, cfg: &Memory) -> Vec<Value> {
    parse_memory_updates(&v, &history(), "group:10", "20", cfg).unwrap()
}
fn rows(db: &Store) -> Vec<Value> {
    LayeredMemory::new(db)
        .rows("group:10", "person:20", "traits", 200.)
        .unwrap()
}

#[test]
fn default_learn_skip_and_invalid_verdict_are_atomic() {
    let cfg = settings();
    let db = Store::in_memory().unwrap();
    let v = candidate("garden", 0);
    let updates = parse(json!([v, {"verdict":"skip","reason":"敏感位置"}]), &cfg);
    assert!(updates[0].get("verdict").is_none()); // exact legacy parsed shape
    LayeredMemory::new(&db)
        .apply("group:10", &updates, 200., &cfg)
        .unwrap();
    assert_eq!(rows(&db).len(), 1);
    assert_eq!(rows(&db)[0]["confidence"], 0.9);
    let logs = db.rows("SELECT * FROM decisions", []).unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0]["action"], "skipped");
    let tags: Value = serde_json::from_str(logs[0]["tags"].as_str().unwrap()).unwrap();
    assert_eq!(tags["reason"], "敏感位置");
    for invalid in [json!(null), json!("LEARN"), json!(5), json!(false)] {
        let mut bad = candidate("bad", 1);
        bad["verdict"] = invalid;
        assert_eq!(
            parse_memory_updates(&json!([v, bad]), &history(), "group:10", "20", &cfg)
                .unwrap_err()
                .to_string(),
            "invalid_memory_verdict"
        );
    }
    assert!(parse_memory_updates(
        &json!([{"verdict":"skip"}]),
        &history(),
        "group:10",
        "20",
        &cfg
    )
    .is_err());
}

#[test]
fn partial_requires_configured_distinct_new_evidence_even_when_model_says_learn() {
    for required in [1, 2, 3] {
        let mut cfg = settings();
        cfg.partial_evidence = required;
        let db = Store::in_memory().unwrap();
        let mut v = candidate("garden", 0);
        v["verdict"] = json!("partial");
        let initial = parse(json!([v]), &cfg);
        LayeredMemory::new(&db)
            .apply("group:10", &initial, 200., &cfg)
            .unwrap();
        assert_eq!(rows(&db)[0]["pending"], true);
        assert!(rows(&db)[0]["confidence"].as_f64().unwrap() <= 0.5);
        for n in 0..=required {
            let mut v = candidate("garden", n);
            v["verdict"] = json!("learn");
            let updates = parse(json!([v]), &cfg);
            // Recreating LayeredMemory simulates losing all transient state; only DB state counts.
            for _ in 0..3 {
                LayeredMemory::new(&db)
                    .apply("group:10", &updates, 201. + n as f64, &cfg)
                    .unwrap();
            }
            if n < required {
                assert_eq!(rows(&db)[0]["pending"], true);
                assert!(rows(&db)[0]["confidence"].as_f64().unwrap() <= 0.5);
            } else {
                assert!(rows(&db)[0].get("pending").is_none());
                assert!(rows(&db)[0]["confidence"].as_f64().unwrap() > 0.5);
            }
        }
    }
    assert_eq!(settings().partial_evidence, 2);
}

#[test]
fn stale_evidence_and_baseline_replays_do_not_promote() {
    let cfg = settings();
    let db = Store::in_memory().unwrap();
    let mut v = candidate("garden", 3);
    v["verdict"] = json!("partial");
    LayeredMemory::new(&db)
        .apply("group:10", &parse(json!([v]), &cfg), 200., &cfg)
        .unwrap();
    for n in [0, 1, 2, 3] {
        LayeredMemory::new(&db)
            .apply(
                "group:10",
                &parse(json!([candidate("garden", n)]), &cfg),
                201.,
                &cfg,
            )
            .unwrap();
    }
    assert_eq!(rows(&db)[0]["pending"], true);
    let mut replay = parse(json!([candidate("garden", 3)]), &cfg);
    replay[0]["sources"][0]["ts"] = json!(1000);
    LayeredMemory::new(&db)
        .apply("group:10", &replay, 202., &cfg)
        .unwrap();
    assert_eq!(rows(&db)[0]["pending"], true);
}

#[test]
fn keep_drop_rewrite_and_scoped_sources_are_checked_before_writes() {
    let cfg = settings();
    let db = Store::in_memory().unwrap();
    let updates = parse(
        json!([candidate("a",0),candidate("b",1),candidate("c",2),{"verdict":"skip","reason":"敏感身份"}]),
        &cfg,
    );
    let mut history = history();
    history.push(json!({"id":"m0","sender":"20","self":true,"text":"模型不能作证"}));
    let input = learning_review_input(&updates, &history, &[]);
    assert_eq!(input["sources"].as_array().unwrap().len(), 3);
    assert_eq!(input["candidates"].as_array().unwrap().len(), 3);
    assert_eq!(input["sources"][2]["id"], "m2");
    let response = json!({"reviews":[{"index":0,"action":"keep","reason":"稳定偏好"},{"index":1,"action":"drop","reason":"一次性情绪"},{"index":2,"action":"rewrite","reason":"去掉推断","text":"提过园艺兴趣"}]});
    let reviewed = apply_learning_review(&updates, &response, &cfg).unwrap();
    LayeredMemory::new(&db)
        .apply("group:10", &reviewed, 200., &cfg)
        .unwrap();
    let rows = rows(&db);
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|r| r["slot"] == "a" && r["text"] == "喜欢园艺"));
    assert!(rows
        .iter()
        .any(|r| r["slot"] == "c" && r["text"] == "提过园艺兴趣"));
    let logs = db
        .rows("SELECT action,tags FROM decisions ORDER BY rowid", [])
        .unwrap();
    assert_eq!(
        logs.iter().map(|r| r["action"].clone()).collect::<Vec<_>>(),
        vec![json!("drop"), json!("rewrite"), json!("skipped")]
    );
    for log in logs {
        let tags: Value = serde_json::from_str(log["tags"].as_str().unwrap()).unwrap();
        assert!(tags["reason"].is_string());
    }
    for field in ["index", "action", "reason", "text"] {
        let mut bad = response.clone();
        bad["reviews"][2][field] = Value::Null;
        assert!(apply_learning_review(&updates, &bad, &cfg).is_err());
    }
    let mut duplicate = response.clone();
    duplicate["reviews"][2]["index"] = json!(0);
    assert!(apply_learning_review(&updates, &duplicate, &cfg).is_err());
    assert!(apply_learning_review(&updates, &json!({"reviews":[]}), &cfg).is_err());
}

#[test]
fn evidence_config_is_optional_bounded_and_integer() {
    let base = config::merge(
        &config::defaults(),
        &json!({"apiKey":"","onebotToken":"","dataDir":"unused"}),
    );
    for value in [json!(0), json!(101), json!(1.5), json!(null)] {
        let mut input = base.clone();
        input["agent"]["memory"]["partialEvidence"] = value;
        assert!(Config::from_value(&input).is_err());
    }
    let mut input = base;
    input["agent"]["memory"]["partialEvidence"] = json!(3);
    assert_eq!(
        Config::from_value(&input)
            .unwrap()
            .agent
            .memory
            .partial_evidence,
        3
    );
}
