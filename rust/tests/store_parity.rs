#[path = "golden/mod.rs"]
mod golden;
use qq_inner_core::store::Store;
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
#[path = "golden/sqlite.rs"]
mod sqlite;
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn expected(mode: &str) -> Value {
    golden::expected(include_str!("golden/store.json"), &json!({"mode":mode}))
}
fn normalize(mut schema: Value) -> Value {
    // Rust-only schedule aggregates extend the shared legacy schema.
    schema["objects"].as_array_mut().unwrap().retain(|r|
        r["name"] != "group_hours" && r["name"] != "messages_group_hours");
    schema["tables"].as_object_mut().unwrap().remove("group_hours");
    for row in schema["objects"].as_array_mut().unwrap() {
        if let Some(sql) = row["sql"].as_str() {
            row["sql"] = json!(sql
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>());
        }
    }
    schema
}
#[test]
fn shared_js_database_schema_and_json_roundtrip() {
    let f = Fixture(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "store-fixture-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            )),
    );
    fs::create_dir_all(&f.0).unwrap();
    let js = expected("write");
    // 用捕获的 JS DDL 和原始行重建旧库，避免用 Rust Store 自己生成兼容性样本。
    let legacy = rusqlite::Connection::open(f.0.join("agent.sqlite")).unwrap();
    for kind in ["table", "index"] {
        for object in js["schema"]["objects"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["type"] == kind)
        {
            legacy
                .execute_batch(object["sql"].as_str().unwrap())
                .unwrap();
        }
    }
    sqlite::insert(&legacy, &js["rawTables"]);
    drop(legacy);
    let s = Store::open(f.0.join("agent.sqlite")).unwrap();
    assert_eq!(
        normalize(s.schema().unwrap()),
        normalize(js["schema"].clone())
    );
    assert_eq!(s.schema().unwrap()["tables"].as_object().unwrap().len(), 18);
    assert_eq!(
        normalize(Store::in_memory().unwrap().schema().unwrap()),
        normalize(js["schema"].clone())
    );
    let db = s.connection();
    let text: String = db
        .prepare_cached("SELECT text FROM messages WHERE id='js'")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(text, "hello🙂");
    let tags: String = db
        .prepare_cached("SELECT tags FROM decisions")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&tags).unwrap(),
        json!(["中文",{"nested":null}])
    );
    assert_eq!(s.history("g", None).unwrap()[0]["text"], "hello🙂");
    assert_eq!(
        s.assessment("g", "js").unwrap().unwrap()["details"],
        json!({"ok":true,"list":[1,null]})
    );
    assert_eq!(
        s.learning_state("g").unwrap()["sources"],
        json!([{"id":"js"}])
    );
    assert_eq!(
        s.expectation("g", 101.).unwrap().unwrap()["forecast"],
        json!({"reply":true})
    );
    s.message(&json!({"chat":"g","id":"rust","sender":"2","name":"Rust","text":"回应","ts":101.}))
        .unwrap();
    s.decision("g", "wait", 2., &json!({"a":[true,null,"中文"]}), 101.)
        .unwrap();
    s.assess("g", "rust", 101., "ready", &json!({"rust":["中文",null]}))
        .unwrap();
    s.expect("g", 101., 60., &json!({"rust":true})).unwrap();
    s.observe(&json!({"chat":"g","hint":"self"}), 101.).unwrap();
    let expected = expected("read");
    let reopened = Store::open(f.0.join("agent.sqlite")).unwrap();
    // 从真实持久化库重新读取，再与捕获结果比较，不能只断言金标准本身。
    let mut decisions = reopened
        .rows("SELECT * FROM decisions ORDER BY ts", [])
        .unwrap();
    for row in &mut decisions {
        row["tags"] = serde_json::from_str(row["tags"].as_str().unwrap()).unwrap();
    }
    let mut assessment = reopened.assessment("g", "rust").unwrap().unwrap();
    assessment["details"] = json!(assessment["details"].to_string());
    let read = json!({"schema":reopened.schema().unwrap(),"messages":reopened.history("g",None).unwrap(),"decisions":decisions,"assessment":assessment,"expectation":reopened.expectation("g",101.).unwrap()});
    sqlite::assert_rows(&reopened, &expected["rawTables"]);
    assert_eq!(
        normalize(read["schema"].clone()),
        normalize(expected["schema"].clone())
    );
    assert_eq!(read["messages"][1]["text"], "回应");
    assert_eq!(
        read["decisions"][1]["tags"],
        json!({"a":[true,null,"中文"]})
    );
    assert_eq!(
        serde_json::from_str::<Value>(read["assessment"]["details"].as_str().unwrap()).unwrap(),
        json!({"rust":["中文",null]})
    );
    assert_eq!(read["expectation"]["forecast"], json!({"rust":true}));
    assert_eq!(
        read["expectation"]["observation"],
        json!({"event":"human_message","addressed":true,"at":101.0})
    );
    let mode: String = db
        .prepare_cached("PRAGMA journal_mode")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let timeout: i64 = db
        .prepare_cached("PRAGMA busy_timeout")
        .unwrap()
        .query_row([], |r| r.get(0))
        .unwrap();
    assert_eq!(timeout, 5000);
}
