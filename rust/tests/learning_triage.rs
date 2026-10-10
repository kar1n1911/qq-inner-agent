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

fn affect_state(db: &Store, subject: &str, dimension: &str, value: f64) {
    qq_inner_core::persona::affect::enable(db).unwrap();
    db.execute(
        "INSERT OR REPLACE INTO affect_state VALUES(?,?,?,?,0,1,200,'[]')",
        rusqlite::params!["group:10", subject, dimension, value],
    )
    .unwrap();
}

#[test]
fn low_group_or_person_mood_skips_heated_traits_and_preserves_partials() {
    for subject in ["group", "person:20"] {
        let db = Store::in_memory().unwrap();
        let cfg = settings();
        let mut v = candidate("garden", 0);
        v["verdict"] = json!("partial");
        LayeredMemory::new(&db)
            .apply("group:10", &parse(json!([v]), &cfg), 200., &cfg)
            .unwrap();
        affect_state(&db, subject, "mood", -0.8);
        let mut v = candidate("garden", 1);
        v["sourceIds"] = json!(["m1", "m2", "m3", "m4", "m5", "m6"]);
        v["emotional"] = json!(true);
        let mut heated = candidate("heated", 7);
        heated["emotional"] = json!(true);
        let updates = parse(json!([v, heated]), &cfg);
        LayeredMemory::new(&db)
            .with_affect(true)
            .apply("group:10", &updates, 200., &cfg)
            .unwrap();
        assert_eq!(rows(&db).len(), 1);
        assert_eq!(rows(&db)[0]["pending"], true);
        assert_eq!(rows(&db)[0]["sources"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn affinity_modulates_evidence_but_never_source_validation_or_stability() {
    for affinity in [-1., 0., 1.] {
        let db = Store::in_memory().unwrap();
        let cfg = settings();
        affect_state(&db, "person:20", "affinity", affinity);
        let apply = |v| {
            LayeredMemory::new(&db)
                .with_affect(true)
                .apply("group:10", &parse(json!([v]), &cfg), 200., &cfg)
                .unwrap()
        };
        apply(candidate("direct", 0));
        assert_eq!(
            rows(&db).iter().find(|r| r["slot"] == "direct").unwrap()["pending"] == true,
            affinity < 0.
        );
        let mut v = candidate("pending", 0);
        v["verdict"] = json!("partial");
        apply(v);
        for n in 1..=3 {
            apply(candidate("pending", n));
            let pending = rows(&db)
                .into_iter()
                .find(|r| r["slot"] == "pending")
                .unwrap();
            assert_eq!(
                pending["pending"] == true,
                (n as f64) < 2. * (1. - 0.5 * affinity)
            );
        }
        let mut bad = candidate("bad", 0);
        bad["sourceIds"] = json!(["invented"]);
        assert!(parse_memory_updates(
            &json!([candidate("valid", 0), bad]),
            &history(),
            "group:10",
            "20",
            &cfg
        )
        .is_err());
    }
}

#[test]
fn agreement_ratings_do_not_change_learning() {
    for agreement in [-1., 1.] {
        let db = Store::in_memory().unwrap();
        let cfg = settings();
        qq_inner_core::persona::affect::enable(&db).unwrap();
        db.message(&history()[0]).unwrap();
        qq_inner_core::persona::affect::rate(&db, "group:10", "m0", 0., agreement, 1., 200.)
            .unwrap();
        LayeredMemory::new(&db)
            .with_affect(true)
            .apply(
                "group:10",
                &parse(json!([candidate("garden", 0)]), &cfg),
                200.,
                &cfg,
            )
            .unwrap();
        assert!(rows(&db)[0].get("pending").is_none());
        assert_eq!(rows(&db)[0]["confidence"], 0.9);
    }
}

// Exercise the actual update path at production-sized amplitudes, rather than
// inserting an unreachable-in-one-turn -0.8 or relying on six-source promotion.
fn small_mood(db: &Store, subject: &str, signal: f64) {
    use qq_inner_core::persona::affect::{self, Dimension};
    affect::enable(db).unwrap();
    db.message(&history()[0]).unwrap();
    affect::rate(db, "group:10", "m0", signal, -1., 1., 200.).unwrap();
    affect::update(
        db,
        "group:10",
        subject,
        Dimension::Mood,
        Dimension::Mood,
        signal,
        1.,
        "m0",
        200.,
    )
    .unwrap();
}

#[test]
fn small_low_mood_skips_only_emotional_claims_without_banking_evidence() {
    for subject in ["group", "person:20"] {
        for signal in [-0.013, -0.03, -0.24] {
            let db = Store::in_memory().unwrap();
            let cfg = settings();
            let mut partial = candidate("pending", 0);
            partial["verdict"] = json!("partial");
            LayeredMemory::new(&db)
                .apply("group:10", &parse(json!([partial]), &cfg), 200., &cfg)
                .unwrap();
            let before = rows(&db)[0].clone();
            small_mood(&db, subject, signal);
            let mut heated = candidate("heated", 1);
            heated["emotional"] = json!(true);
            heated["confidence"] = json!(1.);
            let mut promote = candidate("pending", 1);
            promote["emotional"] = json!(true);
            promote["sourceIds"] = json!(["m1", "m2", "m3", "m4", "m5", "m6"]);
            let mut ordinary = candidate("ordinary", 2);
            ordinary["emotional"] = json!(false);
            LayeredMemory::new(&db)
                .with_affect(true)
                .apply(
                    "group:10",
                    &parse(json!([heated, promote, ordinary]), &cfg),
                    200.,
                    &cfg,
                )
                .unwrap();
            let saved = rows(&db);
            assert_eq!(saved.len(), 2);
            assert_eq!(
                saved.iter().find(|r| r["slot"] == "pending").unwrap(),
                &before
            );
            let fact = saved.iter().find(|r| r["slot"] == "ordinary").unwrap();
            assert!(fact.get("pending").is_none());
            assert_eq!(fact["confidence"], 0.9);
            assert_eq!(
                db.rows("SELECT * FROM decisions WHERE action='skipped'", [])
                    .unwrap()
                    .len(),
                2
            );
            // Ordinary independent evidence still promotes at the original threshold.
            for id in [1, 2] {
                LayeredMemory::new(&db)
                    .with_affect(true)
                    .apply(
                        "group:10",
                        &parse(json!([candidate("pending", id)]), &cfg),
                        201.,
                        &cfg,
                    )
                    .unwrap();
            }
            assert!(rows(&db)
                .iter()
                .find(|r| r["slot"] == "pending")
                .unwrap()
                .get("pending")
                .is_none());
        }
    }
}

#[test]
fn normal_and_decayed_mood_allow_single_source_learning() {
    for (signal, elapsed) in [(0., 0.), (0.013, 0.), (-0.013, 12. * 3600.)] {
        let db = Store::in_memory().unwrap();
        let cfg = settings();
        small_mood(&db, "person:20", signal);
        let mut emotional = candidate("emotional", 1);
        emotional["emotional"] = json!(true);
        LayeredMemory::new(&db)
            .with_affect(true)
            .apply(
                "group:10",
                &parse(json!([candidate("ordinary", 0), emotional]), &cfg),
                200. + elapsed,
                &cfg,
            )
            .unwrap();
        assert_eq!(rows(&db).len(), 2);
        assert!(rows(&db)
            .iter()
            .all(|r| r.get("pending").is_none() && r["confidence"] == 0.9));
    }
}

#[test]
fn emotional_flag_is_validated_and_survives_review() {
    let cfg = settings();
    for invalid in [json!(null), json!("true"), json!(1)] {
        let mut v = candidate("bad", 0);
        v["emotional"] = invalid;
        assert_eq!(
            parse_memory_updates(
                &json!([candidate("valid", 1), v]),
                &history(),
                "group:10",
                "20",
                &cfg
            )
            .unwrap_err()
            .to_string(),
            "invalid_memory_emotional"
        );
    }
    let mut v = candidate("heated", 0);
    v["emotional"] = json!(true);
    let parsed = parse(json!([v]), &cfg);
    let reviewed = apply_learning_review(
        &parsed,
        &json!({"reviews":[{"index":0,
        "action":"rewrite", "reason":"限缩结论", "text":"一时不喜欢园艺"}]}),
        &cfg,
    )
    .unwrap();
    assert_eq!(reviewed[0]["emotional"], true);
}

#[test]
fn observed_scale_detects_negative_mood_but_not_neutral_tail() {
    use qq_inner_core::persona::affect::clearly_low_mood;
    for subject in ["group", "person:20"] {
        let db = Store::in_memory().unwrap();
        // A typical +0.007 input step, close to the measured group scale.
        small_mood(&db, subject, 0.07);
        affect_state(&db, subject, "mood", -0.0026);
        assert!(clearly_low_mood(&db, "group:10", subject, 200.).unwrap());
        affect_state(&db, subject, "mood", -0.0001);
        assert!(!clearly_low_mood(&db, "group:10", subject, 200.).unwrap());
        assert!(!clearly_low_mood(&db, "group:other", subject, 200.).unwrap());
        assert!(!clearly_low_mood(&db, "group:10", "person:other", 200.).unwrap());
    }
}


#[test]
fn forwarded_history_cannot_support_person_memory_or_traits() {
    let cfg = settings();
    for body in [
        "[合并转发]\n（外部信息，非当前群对话）\n我喜欢园艺\n[/合并转发]",
        "我的评论 [合并转发]\n（外部信息，非当前群对话）\n我喜欢园艺\n[/合并转发] 后续评论",
        "[forward]",
    ] {
        let db = Store::in_memory().unwrap();
        let mut message = history()[0].clone();
        message["text"] = json!(body);
        db.message(&message).unwrap();
        // Persisted history must retain the rule without transient segment metadata.
        let stored = db.history("group:10", None).unwrap();
        for layer in ["long_term", "traits"] {
            let mut update = candidate("garden", 0);
            update["layer"] = json!(layer);
            assert_eq!(
                parse_memory_updates(&json!([update]), &stored, "group:10", "20", &cfg)
                    .unwrap_err()
                    .to_string(),
                "memory_forward_person_evidence"
            );
        }
        let mut private = message.clone();
        private["chat"] = json!("private:20");
        LayeredMemory::new(&db)
            .capture(&private, 200., &cfg)
            .unwrap();
        assert!(db
            .rows("SELECT * FROM memory_layers", [])
            .unwrap()
            .is_empty());
    }
    assert_eq!(parse(json!([candidate("garden", 0)]), &cfg).len(), 1);
}
