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
fn flags_default_off_and_reject_non_booleans() {
    let config = Config::from_value(&runtime_defaults()).unwrap();
    assert!(!config.agent.emoji.learn_frequency);
    assert!(!config.agent.emoji.face_only);
    let serialized = serde_json::to_value(&config.agent.emoji).unwrap();
    assert!(serialized.get("learnFrequency").is_none());
    assert!(serialized.get("faceOnly").is_none());
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
