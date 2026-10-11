//! 新功能不变量精确断言；衰减只验证方向，避免把经验参数误当语言理解真值。
use qq_inner_core::{
    config::{defaults, merge, Config},
    persona::expression::decoration_choices,
    persona::humanize::{capture, enable, face_probability},
    store::Store,
};
use serde_json::{json, Value};

fn runtime_defaults() -> Value {
    merge(
        &defaults(),
        &json!({"apiKey":"mock","onebotToken":"","dataDir":"unused"}),
    )
}

fn add(db: &Store, chat: &str, id: &str, ts: f64, own: bool, message: Value) {
    db.message(&json!({"chat":chat,"id":id,"sender":"20","name":"human","text":"sample","ts":ts,"self":own})).unwrap();
    capture(db, chat, id, &json!({"message":message})).unwrap();
}
#[test]
fn flags_default_on_and_reject_non_booleans() {
    // docs/working/prompt-and-learning-design.md §六 L75/L942：开箱即用默认开启。
    let mut omitted = runtime_defaults();
    for key in ["learnFrequency", "faceOnly"] {
        omitted["agent"]["emoji"].as_object_mut().unwrap().remove(key);
    }
    let example: Value = serde_json::from_str(include_str!("../../config.example.json")).unwrap();
    for input in [runtime_defaults(), omitted, merge(&runtime_defaults(), &example)] {
        let config = Config::from_value(&input).unwrap();
        assert!(config.agent.emoji.learn_frequency);
        assert!(config.agent.emoji.face_only);
    }
    for learned in [false, true] {
        for face in [false, true] {
            let input = merge(&runtime_defaults(), &json!({"agent":{"emoji":{
                "learnFrequency":learned,"faceOnly":face
            }}}));
            let config = Config::from_value(&input).unwrap();
            let serialized = serde_json::to_value(&config).unwrap();
            assert_eq!(serialized["agent"]["emoji"]["learnFrequency"], learned);
            assert_eq!(serialized["agent"]["emoji"]["faceOnly"], face);
            let restored: Config = serde_json::from_value(serialized).unwrap();
            assert_eq!(restored.agent.emoji.learn_frequency, learned);
            assert_eq!(restored.agent.emoji.face_only, face);
        }
    }
    for key in ["learnFrequency", "faceOnly"] {
        for value in [json!("true"), json!(1), Value::Null] {
            let config = merge(&runtime_defaults(), &json!({"agent":{"emoji":{key:value}}}));
            assert!(Config::from_value(&config).is_err());
        }
    }
}
#[test]
fn frequency_is_scoped_decayed_capped_and_uses_real_segments() {
    let db = Store::in_memory().unwrap();
    enable(&db).unwrap();
    let now = 40. * 86400.;
    assert_eq!(face_probability(&db, "group:1", now).unwrap(), 0.08);
    for i in 0..10 {
        add(
            &db,
            "group:1",
            &i.to_string(),
            now,
            false,
            json!(if i < 2 { "[CQ:face,id=14]" } else { "hi" }),
        );
        add(
            &db,
            "group:2",
            &i.to_string(),
            now,
            false,
            json!([{"type":"face","data":{"id":14}}]),
        );
        add(
            &db,
            "group:3",
            &i.to_string(),
            now,
            false,
            json!([{"type":"text","data":{"text":"[QQface:14] [CQ:face,id=14]"}}]),
        );
        add(
            &db,
            "group:4",
            &i.to_string(),
            now,
            false,
            json!("&#91;CQ:face,id=14&#93;"),
        );
        add(
            &db,
            "group:1",
            &format!("self{i}"),
            now,
            true,
            json!("[CQ:face,id=14]"),
        );
        add(
            &db,
            "group:1",
            &format!("old{i}"),
            0.,
            false,
            json!("[CQ:face,id=14]"),
        );
    }
    assert!((face_probability(&db, "group:1", now).unwrap() - 0.16).abs() < 1e-12);
    assert_eq!(face_probability(&db, "group:2", now).unwrap(), 0.35);
    assert_eq!(face_probability(&db, "group:3", now).unwrap(), 0.);
    assert_eq!(face_probability(&db, "group:4", now).unwrap(), 0.);
    // 相同数量，最近的 face 应比一周前的 face 权重大。
    for i in 0..10 {
        for (chat, recent_face) in [("group:5", true), ("group:6", false)] {
            add(
                &db,
                chat,
                &format!("new{i}"),
                now,
                false,
                json!(if recent_face && i < 3 {
                    "[CQ:face,id=14]"
                } else {
                    "hi"
                }),
            );
            add(
                &db,
                chat,
                &format!("old{i}"),
                now - 7. * 86400.,
                false,
                json!(if !recent_face && i < 3 {
                    "[CQ:face,id=14]"
                } else {
                    "hi"
                }),
            );
        }
    }
    assert!(
        face_probability(&db, "group:5", now).unwrap()
            > face_probability(&db, "group:6", now).unwrap()
    );
    assert_eq!(
        face_probability(&db, "group:1", now + 31. * 86400.).unwrap(),
        0.08
    );
    db.execute("DELETE FROM messages WHERE chat=?", ["group:1"])
        .unwrap();
    assert!(db
        .rows("SELECT * FROM humanize_faces WHERE chat=?", ["group:1"])
        .unwrap()
        .is_empty());
}
#[test]
fn learned_frequency_respects_existing_probability_and_cooldown() {
    let db = Store::in_memory().unwrap();
    enable(&db).unwrap();
    let config = Config::from_value(&merge(&runtime_defaults(), &json!({"agent":{"emoji":{"enabled":true,"learnFrequency":true,"symbols":[],"faceIds":["14"],"probability":1}}}))).unwrap();
    let mut settings = config.agent.emoji;
    assert_eq!(
        decoration_choices(&db, "group:1", 100., &settings, || 0.079).unwrap()["faceIds"],
        json!(["14"])
    );
    assert_eq!(
        decoration_choices(&db, "group:1", 100., &settings, || 0.08).unwrap()["faceIds"],
        json!([])
    );
    settings.probability = 0.;
    assert_eq!(
        decoration_choices(&db, "group:1", 100., &settings, || 0.).unwrap()["faceIds"],
        json!([])
    );
    settings.probability = 1.;
    db.execute(
        "INSERT INTO decoration_usage VALUES(?,?)",
        rusqlite::params!["group:1", 100.],
    )
    .unwrap();
    assert_eq!(
        decoration_choices(&db, "group:1", 100., &settings, || 0.).unwrap()["faceIds"],
        json!([])
    );
    settings.learn_frequency = false;
    assert_eq!(
        decoration_choices(&db, "group:2", 100., &settings, || 0.5).unwrap()["faceIds"],
        json!(["14"])
    );
}


#[test]
fn multi_bubble_bounds_and_delay_formula() {
    use qq_inner_core::persona::humanize::{bubble_delay_ms, multi_bubbles, multi_bubble_allowed};
    // C2/C3 可用性：human-like-replies.md:172,175–179；不修改长度抽签本身。
    assert!(multi_bubbles(&json!(["甲", "乙"]), 2).unwrap().is_some());
    for invalid in [json!(["甲", "乙丙"]), json!(["", "乙"]), json!(["甲", 1]), json!(["甲","乙","丙","丁"])] {
        assert!(multi_bubbles(&invalid, 2).is_err());
    }
    for length in ["tiny", "short", "medium", "long"] {
        assert_eq!(multi_bubble_allowed(length, &[json!({"text":"哈哈","self":false})]), length == "medium");
        assert!(!multi_bubble_allowed(length, &[json!({"text":"求助：编译失败","self":false})]));
    }
    // C4 可用性：human-like-replies.md:185–186；封顶和抖动边界，空正文也有延迟。
    assert_eq!(bubble_delay_ms(0, true, 0), 400);
    assert_eq!(bubble_delay_ms(5, true, 100), 600);
    assert_eq!(bubble_delay_ms(10000, true, 0), 2500);
    for jitter in 0..1000 {
        assert!((300..=800).contains(&(bubble_delay_ms(5, false, jitter) - bubble_delay_ms(5, true, jitter))));
    }
}
