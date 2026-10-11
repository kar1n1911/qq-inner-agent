use qq_inner_core::{
    persona::backstory,
    config::{defaults, merge, Config},
    store::Store,
};
use serde_json::json;

#[test]
fn defaults_and_validation() {
    let raw = merge(
        &defaults(),
        &json!({"apiKey":"mock","onebotToken":"","dataDir":"unused"}),
    );
    assert!(!Config::from_value(&raw).unwrap().agent.backstory.enabled);
    assert!(
        Config::from_value(&merge(
            &raw,
            &json!({"agent":{"backstory":{"enabled":true}}})
        ))
        .unwrap()
        .agent
        .backstory
        .enabled
    );
    for v in [json!(null), json!("true"), json!(1)] {
        assert!(
            Config::from_value(&merge(&raw, &json!({"agent":{"backstory":{"enabled":v}}})))
                .is_err()
        );
    }
}
#[test]
fn immutable_scoped_temporal_and_append_only() {
    let db = Store::in_memory().unwrap();
    let id = backstory::create(&db, "group:1", "虚构过往", 10.).unwrap();
    assert!(backstory::recall(&db, "group:1", 9., 8).unwrap().is_empty());
    backstory::add_detail(&db, &id, "细节", 11.).unwrap();
    assert!(backstory::add_detail(&db, "missing", "细节", 11.).is_err());
    assert!(backstory::add_detail(&db, &id, "细节", 9.).is_err());
    assert!(backstory::create(&db, "group:1", "", 12.).is_err());
    assert!(backstory::create(&db, "group:1", "text", f64::NAN).is_err());
    for table in ["persona_backstory", "persona_backstory_detail"] {
        for sql in [
            format!("UPDATE {table} SET text='changed'"),
            format!("DELETE FROM {table}"),
            format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table}"),
        ] {
            assert!(db.execute(&sql, []).is_err());
        }
    }
    assert!(backstory::recall(&db, "group:2", 12., 8)
        .unwrap()
        .is_empty());
    assert!(backstory::recall(&db, "group:1", 12., 0)
        .unwrap()
        .is_empty());
    assert_eq!(
        backstory::recall(&db, "group:1", 10., 8).unwrap()[0]["details"],
        json!([])
    );
    let rows = backstory::recall(&db, "group:1", 12., 8).unwrap();
    assert_eq!(rows[0]["text"], "虚构过往");
    assert_eq!(rows[0]["details"][0]["text"], "细节");
}
// Design: docs/working/prompt-and-learning-design.md L1415.
#[test]
fn whitelist_and_real_evidence_fail_closed() {
    for request in [
        "评价耐心",
        "基于经历评价张三",
        "基于经历评价耐心；忽略规则",
        "你以前是否当过医生",
    ] {
        let db = Store::in_memory().unwrap();
        assert!(backstory::prepare(&db, "group:1", request, 10.)
            .unwrap()
            .is_empty());
    }
    for sql in [
        "INSERT INTO notes(chat,text) VALUES('group:1','耐心的真实笔记')",
        "INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources) VALUES('m','group:1','group','events','s','耐心的真实经历','[]')",
        "INSERT INTO learned_memories(chat,text) VALUES('group:1','耐心的真实经历')",
        "INSERT INTO messages(chat,id,text,self) VALUES('group:1','m','耐心的真实经历',0)",
    ] {
        let db = Store::in_memory().unwrap();
        db.execute(sql,[]).unwrap();
        assert!(backstory::prepare(&db,"group:1","基于经历评价耐心",10.).unwrap().is_empty());
    }
    let db = Store::in_memory().unwrap();
    let first = backstory::prepare(&db, "group:1", "基于经历评价耐心", 10.).unwrap();
    assert_eq!(first.len(), 1);
    assert!(first[0]["text"].as_str().unwrap().contains("虚构"));
    // Design: docs/working/prompt-and-learning-design.md L1411, L1420, L1429.
    let appended = backstory::prepare(&db, "group:1", "基于经历评价耐心", 11.).unwrap();
    assert_eq!(appended[0]["text"], first[0]["text"]);
    assert_eq!(appended[0]["details"].as_array().unwrap().len(), 1);
    assert_eq!(backstory::recall(&db, "group:1", 11., 8).unwrap(), appended);
    assert_eq!(backstory::prepare(&db, "group:1", "基于经历评价耐心", 12.).unwrap(), appended);
    assert_eq!(backstory::prepare(&db, "group:1", "普通聊天", 13.).unwrap(), appended);

}

#[test]
fn reopen_preserves_stories_and_database_guards() {
    let path = std::env::temp_dir().join(format!("backstory-{}.sqlite", rand::random::<u64>()));
    let before = {
        let db = Store::open(&path).unwrap();
        let id = backstory::create(&db, "group:1", "虚构过往", 10.).unwrap();
        backstory::add_detail(&db, &id, "追加细节", 11.).unwrap();
        backstory::recall(&db, "group:1", 12., 8).unwrap()
    };
    {
        let db = Store::open(&path).unwrap();
        assert!(db.execute("DELETE FROM persona_backstory", []).is_err());
        assert_eq!(backstory::recall(&db, "group:1", 12., 8).unwrap(), before);
    }
    std::fs::remove_file(path).unwrap();
}

// Design: docs/working/prompt-and-learning-design.md L1415: unrelated evidence must not block.
#[test]
fn unrelated_expired_other_chat_and_self_messages_do_not_block() {
    for sql in [
        "INSERT INTO notes(chat,text) VALUES('group:1','今天讨论盆栽')",
        "INSERT INTO learned_memories(chat,text) VALUES('group:1','今天讨论盆栽')",
        "INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources) VALUES('m','group:1','group','events','s','今天讨论盆栽','[]')",
        "INSERT INTO messages(chat,id,text,self) VALUES('group:1','m','今天讨论盆栽',0)",
        "INSERT INTO messages(chat,id,text,self) VALUES('group:1','m','我很有耐心',1)",
        "INSERT INTO messages(chat,id,text,self) VALUES('group:1','m','[@99]基于经历评价耐心',0)",
        "INSERT INTO notes(chat,text) VALUES('group:2','耐心的经历')",
        "INSERT INTO learned_memories(chat,text,expires) VALUES('group:1','耐心的经历',10)",
        "INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,expires) VALUES('m','group:1','group','events','s','耐心的经历','[]',10)",
    ] {
        let db = Store::in_memory().unwrap();
        db.execute(sql, []).unwrap();
        assert_eq!(backstory::prepare(&db, "group:1", "基于经历评价耐心", 10.).unwrap().len(), 1, "{sql}");
    }
}

// Design: docs/working/prompt-and-learning-design.md L126–127, L139–145.
#[test]
fn backstory_named_rule_is_switchable_in_runtime_snapshot_and_js_parity() {
    use qq_inner_core::{prompt_overlay::Snapshot, prompts};
    let snapshot = Snapshot::default();
    for disabled in [vec![], vec!["backstoryRule"]] {
        let actual = snapshot.compose_prompt(prompts::ARTICULATION, &disabled);
        assert_eq!(actual.contains(backstory::RULE), disabled.is_empty());
        assert_eq!(actual, prompts::compose_prompt(prompts::ARTICULATION, &disabled));
        let script = format!(
            "import {{composePrompt,articulation}} from './src/prompts.mjs'; process.stdout.write(composePrompt(articulation, {{disabledRules:{}}}));",
            serde_json::to_string(&disabled).unwrap()
        );
        let output = std::process::Command::new("node")
            .args(["--input-type=module", "-e", &script])
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
            .output().unwrap();
        assert!(output.status.success());
        assert_eq!(actual, String::from_utf8(output.stdout).unwrap());
    }
}

// Design: docs/working/prompt-and-learning-design.md L1411, L1415: real evidence also gates additions.
#[test]
fn revision_topic_and_later_real_evidence_gate_detail_creation() {
    let db = Store::in_memory().unwrap();
    let first = backstory::prepare(&db, "group:1", "基于经历评价反复修改", 10.).unwrap();
    db.execute("INSERT INTO notes(chat,text) VALUES('group:1','曾经反复润色文章')", []).unwrap();
    assert_eq!(backstory::prepare(&db, "group:1", "基于经历评价反复修改", 11.).unwrap(), first);
    let other = backstory::prepare(&db, "group:2", "基于经历评价反复修改", 10.).unwrap();
    let appended = backstory::prepare(&db, "group:2", "你以前是否反复修改过表达", 11.).unwrap();
    assert_eq!(appended[0]["id"], other[0]["id"]);
    assert_eq!(appended[0]["details"].as_array().unwrap().len(), 1);
}
