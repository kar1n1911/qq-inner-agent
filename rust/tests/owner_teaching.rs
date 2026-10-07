use qq_inner_core::{
    config::{defaults, merge, Config},
    persona::expression::ExpressionMemory,
    persona::owner_teaching::handle,
    store::Store,
};
use serde_json::{json, Value};

#[test]
fn validation_and_literal_forgetting_are_scoped() {
    let db = Store::in_memory().unwrap();
    let mut a = Config::from_value(&merge(
        &defaults(),
        &json!({"apiKey":"","onebotToken":"","dataDir":"unused"}),
    ))
    .unwrap()
    .agent;
    a.owner_teaching.enabled = true;
    let owner = "1950202917";
    let chat = "private:1950202917";
    for input in [
        "/黑话",
        "/黑话 =B",
        "/黑话 A=",
        "/忘记",
        &format!("/黑话 {}=B", "A".repeat(41)),
        &format!("/黑话 A={}", "B".repeat(161)),
        &format!("/忘记 {}", "A".repeat(501)),
    ] {
        assert!(handle(&db, &a, chat, owner, input, 100.)
            .unwrap()
            .starts_with("没看懂："));
    }
    assert!(db.rows("SELECT * FROM expressions", []).unwrap().is_empty());
    assert!(db
        .rows("SELECT * FROM memory_layers", [])
        .unwrap()
        .is_empty());
    assert_eq!(
        handle(&db, &a, chat, owner, "/黑话 AA=B", 100.),
        Some("记住了".into())
    );
    let recall = ExpressionMemory::new(&db)
        .context(chat, owner, "AA", 101., &a.expression, &a.memory)
        .unwrap();
    assert_eq!(recall.len(), 1);
    assert_eq!(recall[0]["sources"], json!(["owner-teaching"]));
    handle(&db, &a, chat, owner, "/忘记 %", 102.);
    assert_eq!(db.rows("SELECT * FROM expressions", []).unwrap().len(), 1);
    assert_eq!(handle(&db, &a, "private:12", "12", "/忘记 A", 103.), None);
    assert_eq!(handle(&db, &a, chat, owner, "你好", 103.), None);
    let rows = db.rows("SELECT * FROM expressions", []).unwrap();
    let sources: Value = serde_json::from_str(rows[0]["sources"].as_str().unwrap()).unwrap();
    assert_eq!(sources, json!(["owner-teaching"]));
}

#[test]
fn teaching_replaces_human_provenance_without_forging_message_ids() {
    let db = Store::in_memory().unwrap();
    let mut a = Config::from_value(&merge(
        &defaults(),
        &json!({"apiKey":"","onebotToken":"","dataDir":"unused"}),
    ))
    .unwrap()
    .agent;
    a.owner_teaching.enabled = true;
    let chat = "private:1950202917";
    ExpressionMemory::new(&db).apply(chat, &[json!({"subject":"person:1950202917","kind":"jargon","term":"AA","meaning":"BB","situation":"聊天","example":"AA","confidence":1.,"sources":[{"id":"real-human-id","sender":"1950202917","ts":100.}]})], 100., &a.expression).unwrap();
    assert_eq!(
        handle(&db, &a, chat, "1950202917", "/黑话 AA=BB", 101.),
        Some("记住了".into())
    );
    let rows = db.rows("SELECT * FROM expressions", []).unwrap();
    assert_eq!(rows.len(), 1);
    let sources: Value = serde_json::from_str(rows[0]["sources"].as_str().unwrap()).unwrap();
    assert_eq!(sources, json!(["owner-teaching"]));
    assert!(!sources.to_string().contains("real-human-id"));
}
