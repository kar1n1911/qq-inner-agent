use super::*;
use std::fs;
#[test]
fn schema_migration_and_constraints() -> Result<()> {
    let db = Connection::open_in_memory()?;
    db.execute_batch("CREATE TABLE thoughts(id TEXT PRIMARY KEY, chat TEXT, text TEXT, kind TEXT, created REAL, used INTEGER DEFAULT 0, score REAL DEFAULT 0); INSERT INTO thoughts VALUES('old','g','t','k',1,0,0);
    CREATE TABLE memory_layers(id TEXT PRIMARY KEY, chat TEXT NOT NULL, subject TEXT NOT NULL, layer TEXT NOT NULL, slot TEXT NOT NULL, text TEXT NOT NULL, sources TEXT NOT NULL, importance REAL, created REAL, updated REAL, expires REAL, revision INTEGER DEFAULT 1, UNIQUE(chat,subject,layer,slot));
    INSERT INTO memory_layers VALUES('old','g','group','long_term','s','t','[]',1,1,1,2,1);")?;
    let s = Store::initialize(db)?;
    assert_eq!(
        s.rows("SELECT * FROM thoughts", [])?[0]["subject"],
        Value::Null
    );
    let r = &s.rows("SELECT * FROM memory_layers", [])?[0];
    assert_eq!(r["keywords"], "[]");
    assert_eq!(r["confidence"], 0.6);
    s.execute(
        "INSERT INTO memory_revisions(memory_id,revision) VALUES('old',1)",
        [],
    )?;
    s.execute("DELETE FROM memory_layers WHERE id='old'", [])?;
    assert!(s.rows("SELECT * FROM memory_revisions", [])?.is_empty());
    assert!(s
        .execute("INSERT INTO activity_rhythm(id) VALUES(2)", [])
        .is_err());
    s.execute("INSERT INTO activity_rhythm(id) VALUES(1)", [])?;
    assert!(s
        .execute("INSERT INTO activity_rhythm(id) VALUES(1)", [])
        .is_err());
    let again = Store::initialize(s.db)?;
    assert_eq!(again.schema()?["tables"].as_object().unwrap().len(), 27);
    Ok(())
}

#[test]
fn received_at_migration_is_idempotent_and_marks_unknown_history() -> Result<()> {
    // Goal: UPDATE-1/UPDATE-2 plus the user's migration rule forbid a new
    // binary from opening the old shape and forbid invented historical time.
    let path = std::env::temp_dir().join(format!(
        "qq-received-at-{}-{}.sqlite",
        std::process::id(),
        rand::random::<u64>()
    ));
    let db = Connection::open(&path)?;
    db.execute_batch(
        "CREATE TABLE messages(chat TEXT,id TEXT,sender TEXT,name TEXT,text TEXT,ts REAL,self INTEGER DEFAULT 0,PRIMARY KEY(chat,id));
         INSERT INTO messages VALUES('g','old','1','n','text',123,0);",
    )?;
    drop(db);
    let error = match Store::open(&path) {
        Ok(_) => panic!("old schema opened without migration"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("database migration required"));
    Store::migrate_received_at(&path)?;
    Store::migrate_received_at(&path)?;
    Store::migrate_message_keys(&path)?;
    Store::migrate_message_keys(&path)?;
    Store::migrate_manual_participation(&path)?;
    let store = Store::open(&path)?;
    let row = &store.rows(
        "SELECT ts,received_at FROM messages WHERE id='old'",
        [],
    )?[0];
    assert_eq!(row["ts"], 123.);
    assert_eq!(row["received_at"], Store::UNKNOWN_RECEIVED_AT);
    assert_eq!(
        store.rows("SELECT count(*) AS n FROM migration_received_at_v1", [])?[0]["n"],
        1
    );
    drop(store);
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn reply_relation_migration_requires_full_and_does_not_invent_old_edges() -> Result<()> {
    let path = std::env::temp_dir().join(format!("qq-reply-relation-{}-{}.sqlite", std::process::id(), rand::random::<u64>()));
    let store = Store::open(&path)?;
    let (_, key) = store.record_message(&message("before-r", 100.))?;
    store.db.execute_batch("DROP VIEW inbound_reply_status; DROP TRIGGER inbound_reply_cleanup; DROP TABLE inbound_reply_relations;")?;
    drop(store);
    assert!(Store::requires_message_key_migration(&path)?);
    assert!(Store::open(&path).err().is_some_and(|error| error.to_string().contains("update full")));
    Store::migrate_message_keys(&path)?;
    Store::migrate_message_keys(&path)?;
    let store = Store::open(&path)?;
    assert_eq!(store.rows("SELECT source_message_key,status,target_message_key FROM inbound_reply_status WHERE source_message_key=?", [&key])?,
        vec![json!({"source_message_key":key,"status":"unknown","target_message_key":null})]);
    drop(store);
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn topic_feedback_is_scoped_idempotent_and_silence_is_weak() -> Result<()> {
    let db = Store::in_memory()?;
    let delivery = db.delivery("group:10", true, 100.)?;
    db.finish_delivery(&delivery, "sent_unconfirmed", Some(&json!("bot-id")))?;
    db.outbound_message(&json!({"chat":"group:10","id":"bot-id","sender":"99","text":"海豚照片梗经典回顾","ts":100.,"self":true}), &delivery)?;
    let bot_key = db.first("SELECT id FROM outbound_message_facts WHERE delivery_id=?",[&delivery])?.unwrap()["id"].as_str().unwrap().to_owned();
    db.classify_topic_once(TopicClassification { chat:"group:10",topic_key:&bot_key,topic:"海豚照片梗经典回顾",
        stage:"natural_end",category:"shared_recurrence",evidence:"海豚照片梗",now:100. })?;
    db.link_topic_send("group:10",&delivery,&bot_key)?;
    let (_, key) = db.record_inbound_message(&json!({"chat":"group:10","id":"reply-id","sender":"30","text":"这个海豚照片梗真好笑，再来","ts":101.,"self":false}),ReplyInput::Target("bot-id".into()))?;
    assert!(db.learn_topic_reply(&key,101.,"natural_end")?);
    db.learn_topic_reply(&key,101.,"natural_end")?;
    let row = db.topic_revisit("group:10","海豚照片梗经典回顾")?.unwrap();
    assert_eq!(row["positives"],1);
    let score = row["revisitability"].as_f64().unwrap();
    assert!(score > 0.49);
    assert_eq!(db.first("SELECT positives FROM topic_revisit_category_status WHERE chat='group:10' AND category='shared_recurrence'",[])?.unwrap()["positives"],1);
    assert!(db.topic_revisit("group:11","海豚照片梗经典回顾")?.is_none());
    let quiet = db.delivery("group:10",true,200.)?;
    db.finish_delivery(&quiet,"sent_unconfirmed",Some(&json!("quiet-id")))?;
    db.outbound_message(&json!({"chat":"group:10","id":"quiet-id","sender":"99","text":"海豚照片梗经典回顾","ts":200.,"self":true}),&quiet)?;
    db.link_topic_send("group:10",&quiet,&bot_key)?;
    db.observe_topic_silence("group:10",1000.,600.)?;
    let after = db.topic_revisit("group:10","海豚照片梗经典回顾")?.unwrap()["revisitability"].as_f64().unwrap();
    assert!(after > 0. && after < score);
    db.observe_topic_silence("group:10",1000.,600.)?;
    assert_eq!(db.topic_revisit("group:10","海豚照片梗经典回顾")?.unwrap()["revisitability"], after);
    Ok(())
}

#[test]
fn topic_category_is_frozen_and_full_migration_keeps_old_scores_unknown() -> Result<()> {
    let path = std::env::temp_dir().join(format!("qq-topic-revisit-{}-{}.sqlite", std::process::id(), rand::random::<u64>()));
    let db = Store::open(&path)?;
    let (_, old_key) = db.record_message(&message("old-topic",100.))?;
    db.db.execute_batch("DROP VIEW topic_revisit_category_status; DROP VIEW topic_revisit_status;
        DROP TABLE topic_revisit_feedback; DROP TABLE topic_revisit_sends; DROP TABLE topic_revisit_topics;")?;
    drop(db);
    assert!(Store::requires_message_key_migration(&path)?);
    assert!(Store::open(&path).err().is_some_and(|e| e.to_string().contains("update full")));
    Store::migrate_message_keys(&path)?;
    Store::migrate_message_keys(&path)?;
    let db = Store::open(&path)?;
    assert!(db.topic_revisit("g","old")?.is_none());
    let first = TopicClassification { chat:"g",topic_key:&old_key,topic:"old",stage:"natural_end",
        category:"shared_recurrence",evidence:"group reference",now:200. };
    db.classify_topic_once(first)?;
    db.classify_topic_once(TopicClassification { chat:"g",topic_key:&old_key,topic:"old",stage:"natural_end",
        category:"ephemeral_chat",evidence:"later guess",now:300. })?;
    assert_eq!(db.topic_revisit("g","old")?.unwrap()["category"],"shared_recurrence");
    drop(db);
    fs::remove_file(path)?;
    Ok(())
}

fn message(id: &str, ts: f64) -> Value {
    json!({"chat":"g","id":id,"sender":"1","name":"名字","text":"中文🙂","ts":ts})
}
// Basis: the message-id audit found 518 inversions in 1022 VM messages;
// OB11 exposes no msgSeq. Distinct evidence must not be lost under one raw id.
#[test]
fn local_message_keys_keep_collisions_and_replays_distinct() -> Result<()> {
    let s = Store::in_memory()?;
    let first = message("reused", 100.);
    let mut second = message("reused", 101.);
    second["text"] = json!("另一条消息");
    let (inserted, first_key) = s.record_message(&first)?;
    assert!(inserted);
    let (inserted, second_key) = s.record_message(&second)?;
    assert!(inserted);
    assert_ne!(first_key, second_key);
    assert!(first_key.starts_with("local-message:"));
    assert_eq!(s.record_message(&second)?, (false, second_key));
    assert_eq!(s.rows("SELECT id,text,ts FROM messages WHERE chat='g' ORDER BY ts", [])?,
        vec![json!({"id":"reused","text":"中文🙂","ts":100.}),json!({"id":"reused","text":"另一条消息","ts":101.})]);
    assert_eq!(s.rows("SELECT kind,raw_id FROM message_identity_audit", [])?,
        vec![json!({"kind":"id_collision","raw_id":"reused"})]);
    assert!(s.resolve_message_key("g", "reused")?.is_none());
    assert_eq!(s.rows("SELECT kind FROM message_identity_audit WHERE kind='ambiguous_lookup'", [])?.len(), 1);
    let outbound = json!({"chat":"g","id":"same-ack","sender":"bot","text":"同文","ts":200.,"self":true});
    for _ in 0..2 {
        let delivery = s.delivery("g", false, 200.)?;
        s.outbound_message(&outbound, &delivery)?;
        s.outbound_message(&outbound, &delivery)?;
    }
    assert_eq!(s.rows("SELECT id FROM messages WHERE id='same-ack'", [])?.len(), 2);
    assert_eq!(s.rows("SELECT id FROM outbound_message_facts", [])?.len(), 2);
    Ok(())
}

// Basis: the same audit and the first-stage migration plan require all old
// rows to remain readable, with unverifiable references retained for review.
#[test]
fn message_key_migration_is_atomic_idempotent_and_audits_orphans() -> Result<()> {
    let path = std::env::temp_dir().join(format!("qq-message-key-{}-{}.sqlite", std::process::id(),rand::random::<u64>()));
    let db = Connection::open(&path)?;
    db.execute_batch("CREATE TABLE messages(chat TEXT,id TEXT,sender TEXT,name TEXT,text TEXT,ts REAL,self INTEGER DEFAULT 0,received_at REAL NOT NULL DEFAULT -1,PRIMARY KEY(chat,id));
        INSERT INTO messages VALUES('g','old','1','n','original',123,0,124);
        CREATE TABLE media_ocr(chat TEXT,message_id TEXT,hash TEXT,text TEXT,engine TEXT,created REAL,PRIMARY KEY(chat,message_id));
        INSERT INTO media_ocr VALUES('g','old','h','ocr','e',125);
        CREATE TABLE media_receipts(chat TEXT,message_id TEXT,segment INTEGER,PRIMARY KEY(chat,message_id,segment));
        INSERT INTO media_receipts VALUES('g','missing',0);
        CREATE TABLE memory_layers(id TEXT PRIMARY KEY,chat TEXT,subject TEXT,layer TEXT,slot TEXT,sources TEXT,expires REAL);
        INSERT INTO memory_layers VALUES('one','g','group','short_term','old','[{\"id\":\"old\"}]',NULL);")?;
    drop(db);
    Store::migrate_message_keys(&path)?;
    Store::migrate_message_keys(&path)?;
    Store::migrate_manual_participation(&path)?;
    let s = Store::open(&path)?;
    let key = s.resolve_message_key("g", "old")?.unwrap();
    assert_eq!(s.rows("SELECT id,text,ts,received_at FROM messages", [])?,
        vec![json!({"id":"old","text":"original","ts":123.,"received_at":124.})]);
    assert_eq!(s.rows("SELECT message_id FROM media_ocr", [])?[0]["message_id"], key);
    assert_eq!(s.rows("SELECT slot,sources FROM memory_layers", [])?[0], json!({"slot":key,"sources":format!("[{{\"id\":\"{key}\"}}]")}));
    assert_eq!(s.rows("SELECT message_id FROM media_receipts", [])?[0]["message_id"], "unresolved:media_receipts:1");
    assert_eq!(s.rows("SELECT kind,table_name,raw_id FROM message_identity_audit", [])?,
        vec![json!({"kind":"unresolved_reference","table_name":"media_receipts","raw_id":"missing"})]);
    assert_eq!(s.rows("SELECT count(*) AS n FROM migration_message_key_v1", [])?[0]["n"], 1);
    drop(s);
    fs::remove_file(path)?;
    Ok(())
}
// Basis: 2026-10-12 NapCat protocol msgTime finding and the S-section time contract.
#[test]
fn replay_preserves_server_time_and_updates_receive_time() -> Result<()> {
    let s = Store::in_memory()?;
    let mut first = message("replay", 1000.);
    first["received_at"] = json!(1002.);
    assert!(s.message(&first)?);
    let mut replay = message("replay", 1000.);
    replay["received_at"] = json!(1010.);
    assert!(!s.message(&replay)?);
    let row = &s.history("g", None)?[0];
    assert_eq!(row["ts"], 1000.);
    assert_eq!(row["received_at"], 1010.);
    Ok(())
}

// Basis: 2026-10-12 no self echo, ACK has no timestamp, and T requires a
// queryable local estimate plus a future server-time correction path.
#[test]
fn outbound_time_source_can_be_confirmed_without_changing_inbound_time() -> Result<()> {
    let s = Store::in_memory()?;
    s.message(&json!({"chat":"g","id":"in","text":"in","ts":100.,"self":false,"received_at":110.}))?;
    let delivery = s.delivery("g", false, 200.)?;
    s.finish_delivery(&delivery, "sent_unconfirmed", Some(&json!(42)))?;
    s.outbound_message(&json!({"chat":"g","id":"42","text":"out","ts":201.,"self":true}), &delivery)?;
    let key = s.resolve_message_key("g", "42")?.unwrap();
    assert_eq!(s.rows("SELECT m.ts,f.time_source FROM messages m JOIN outbound_message_facts f ON f.chat=m.chat AND f.id=m.message_key WHERE m.id='42'", [])?[0],
        json!({"ts":201.,"time_source":"local_estimate"}));
    assert!(s.confirm_outbound("g", &key, 199.)?);
    assert_eq!(s.rows("SELECT m.ts,f.time_source,d.status FROM messages m JOIN outbound_message_facts f ON f.chat=m.chat AND f.id=m.message_key JOIN deliveries d ON d.id=f.delivery_id WHERE m.id='42'", [])?[0],
        json!({"ts":199.,"time_source":"server","status":"confirmed"}));
    assert!(!s.confirm_outbound("g", "in", 999.)?);
    assert_eq!(s.rows("SELECT ts,received_at FROM messages WHERE id='in'", [])?[0], json!({"ts":100.,"received_at":110.}));
    Ok(())
}

#[test]
fn outbound_evidence_requires_exact_ack_and_preserves_first_confirmation() -> Result<()> {
    let s = Store::in_memory()?;
    let delivery = s.delivery("group:10", false, 200.)?;
    s.finish_delivery(&delivery, "sent_unconfirmed", Some(&json!(42)))?;
    s.outbound_message(&json!({"chat":"group:10","id":"42","sender":"99","text":"sent","ts":201.,"self":true}), &delivery)?;
    assert!(!s.reconcile_outbound("group:10", "42", "99", "different", 202., 203.)?);
    assert!(s.reconcile_outbound("group:10", "42", "99", "sent", 204., 205.)?);
    assert!(!s.reconcile_outbound("group:10", "42", "99", "sent", 999., 1000.)?);
    assert_eq!(s.rows("SELECT m.ts,m.received_at,d.status,f.time_source FROM messages m JOIN outbound_message_facts f ON f.id=m.message_key JOIN deliveries d ON d.id=f.delivery_id", [])?[0],
        json!({"ts":204.,"received_at":205.,"status":"confirmed","time_source":"server"}));
    Ok(())
}
#[test]
fn messages_history_batch_and_active_chats() -> Result<()> {
    let s = Store::in_memory()?;
    assert!(s.history("g", None)?.is_empty());
    assert!(s.active_chats(0.)?.is_empty());
    assert!(s.message(&message("a", 10.25))?);
    assert!(!s.message(&message("a", 10.25))?);
    assert_eq!(
        s.messages(&[message("b", 10.25), message("a", 10.25), message("c", 9.)])?,
        vec![true, false, true]
    );
    assert_eq!(
        s.history("g", Some(2))?
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(s.history("g", Some(-1))?.len(), 3);
    assert!(s.history("g", Some(0))?.is_empty());
    assert_eq!(s.active_chats(10.)?, vec!["g"]);
    assert!(s.active_chats(10.25)?.is_empty());
    // 触发第二条失败，验证批量入口没有留下半批数据。
    s.db.execute_batch("CREATE TRIGGER reject_bad BEFORE INSERT ON messages WHEN NEW.id='bad' BEGIN SELECT RAISE(ABORT,'bad'); END;")?;
    assert!(s
        .messages(&[message("new", 30.), message("bad", 30.)])
        .is_err());
    assert_eq!(s.history("g", None)?.len(), 3);
    for i in 0..30 {
        s.message(&message(&format!("more{i}"), 40. + i as f64))?;
    }
    assert_eq!(s.history("g", None)?.len(), 24);
    Ok(())
}
#[test]
fn rolling_budget_boundaries() -> Result<()> {
    let s = Store::in_memory()?;
    assert!(!s.call_budget(100., 0.)?);
    assert!(s.call_budget(100., 2.)?);
    assert!(s.call_budget(101., 2.)?);
    assert!(!s.call_budget(102., 2.)?);
    assert_eq!(s.rows("SELECT * FROM calls", [])?.len(), 2);
    assert!(!s.call_budget(3700., 2.)?); // 恰好一小时尚未删除。
    assert!(s.call_budget(3700.01, 2.)?);
    assert!(!s.call_budget(3700.02, 2.)?);
    assert!(!s.call_budget(10000., 0.)?); // 拒绝也执行清理。
    assert!(s.rows("SELECT * FROM calls", [])?.is_empty());
    Ok(())
}
#[test]
fn deliveries_counts_recovery_and_timing() -> Result<()> {
    let s = Store::in_memory()?;
    assert_eq!(
        s.counts("g", 100.)?,
        json!({"total":0,"proactive":0,"last":0})
    );
    assert_eq!(
        s.sending_timing("g", 100., 123.)?,
        json!({"gap":123.,"recentHumans":0})
    );
    s.recover_deliveries()?;
    let a = s.delivery("g", true, 100.)?;
    let b = s.delivery("g", false, 101.)?;
    s.finish_delivery(&b, "failed", None)?;
    s.recover_deliveries()?;
    assert_eq!(
        s.rows("SELECT status FROM deliveries WHERE id=?", [&a])?[0]["status"],
        "uncertain"
    );
    assert_eq!(
        s.counts("g", 102.)?,
        json!({"total":1,"proactive":1,"last":100.})
    );
    s.finish_delivery(&a, "sent", Some(&json!(42)))?;
    assert_eq!(
        s.rows("SELECT message_id FROM deliveries WHERE id=?", [&a])?[0]["message_id"],
        "42"
    );
    s.finish_delivery(&a, "sent", Some(&Value::Null))?;
    assert_eq!(
        s.rows("SELECT message_id FROM deliveries WHERE id=?", [&a])?[0]["message_id"],
        Value::Null
    );
    assert_eq!(s.counts("g", 3700.)?["total"], 0);
    s.messages(&[
        message("a", 40.),
        message("b", 39.99),
        json!({"chat":"g","id":"self","ts":100,"self":true}),
        message("future", 101.),
    ])?;
    assert_eq!(
        s.sending_timing("g", 100., 123.)?,
        json!({"gap":0.,"recentHumans":2})
    );
    assert_eq!(s.sending_timing("g", 99., 123.)?["gap"], 0.);
    assert_eq!(s.sending_timing("g", 120., 123.)?["gap"], 20.);
    Ok(())
}
#[test]
fn thoughts_reservoir_score_use_note_and_decision() -> Result<()> {
    let s = Store::in_memory()?;
    assert!(s.reservoir("g", 100., 10., 10, None)?.is_empty());
    let old = s.add_thought("g", &json!({"text":"old","kind":"k"}), 90.)?;
    let a = s.add_thought(
        "g",
        &json!({"text":"a","kind":"k","subject":"person:1"}),
        91.,
    )?;
    s.add_thought(
        "g",
        &json!({"text":"b","kind":"k","subject":"person:2"}),
        92.,
    )?;
    s.add_thought("other", &json!({"text":"other","kind":"k"}), 99.)?;
    assert!(old.get("subject").is_none());
    assert_eq!(s.reservoir("g", 100., 10., 10, None)?.len(), 2);
    assert_eq!(s.reservoir("g", 100., 10., 1, None)?[0]["text"], "b");
    assert!(s.reservoir("g", 100., 10., 0, None)?.is_empty());
    s.score(a["id"].as_str().unwrap(), 4.5)?;
    let r = s.reservoir("g", 100., 10., 10, Some("person:1"))?;
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["score"], 4.5);
    s.r#use(a["id"].as_str().unwrap())?;
    assert!(s
        .reservoir("g", 100., 10., 10, Some("person:1"))?
        .is_empty());
    s.score("missing", 1.)?;
    s.r#use("missing")?;
    s.note("g", "owner note", 100.25)?;
    assert_eq!(
        s.rows("SELECT text,created FROM notes", [])?[0],
        json!({"text":"owner note","created":100.25})
    );
    let tags = json!(["标签",{"nested":[true,null]}]);
    s.decision("g", "wait", 3., &tags, 100.25)?;
    let row = s.rows("SELECT * FROM decisions", [])?;
    assert_eq!(
        serde_json::from_str::<Value>(row[0]["tags"].as_str().unwrap())?,
        tags
    );
    Ok(())
}
#[test]
fn learning_handled_and_assessments() -> Result<()> {
    let s = Store::in_memory()?;
    assert_eq!(
        s.learning_state("g")?,
        json!({"style":"","sources":[],"updated":0,"last_id":"","epoch":0})
    );
    s.execute(
        "INSERT INTO chat_learning VALUES('g','style','[{\"id\":\"a\"}]',10,'a',2)",
        [],
    )?;
    assert_eq!(s.learning_state("g")?["sources"], json!([{"id":"a"}]));
    assert!(s.handled("g")?.is_none());
    s.mark_handled("g", "a", true)?;
    s.mark_handled("g", "b", false)?;
    assert_eq!(
        s.handled("g")?,
        Some(json!({"chat":"g","human_id":"b","pause_done":0}))
    );
    assert!(s.assessment("g", "a")?.is_none());
    s.assessment_status("g", "a", "missing")?;
    s.assess("g", "a", 10.25, "ready", &json!({"ok":true,"missing":null}))?;
    assert!(s.assess("g", "a", 11., "duplicate", &json!({})).is_err());
    s.assessment_status("g", "a", "sent")?;
    let row = s.assessment("g", "a")?.unwrap();
    assert_eq!(row["status"], "sent");
    assert_eq!(row["details"], json!({"ok":true,"missing":null}));
    assert!(row["details"].get("absent").is_none());
    Ok(())
}
#[test]
fn expectation_observation_boundaries() -> Result<()> {
    let s = Store::in_memory()?;
    assert!(s.expectation("g", 100.)?.is_none());
    s.observe(&json!({"chat":"g"}), 100.)?;
    s.expect("g", 100., 10., &json!({"negative":null}))?;
    assert_eq!(
        s.expectation("g", 99.)?.unwrap(),
        json!({"forecast":{"negative":null},"elapsedSeconds":0.,"observation":{"event":"no_message_yet"}})
    );
    s.observe(&json!({"chat":"g","hint":"self"}), 99.)?;
    assert_eq!(
        s.expectation("g", 100.)?.unwrap()["observation"]["event"],
        "no_message_yet"
    );
    s.observe(&json!({"chat":"g","hint":"self"}), 100.)?;
    s.observe(&json!({"chat":"g"}), 101.)?;
    assert_eq!(
        s.expectation("g", 101.)?.unwrap()["observation"],
        json!({"event":"human_message","addressed":true,"at":100.})
    );
    assert!(s.expectation("g", 110.)?.is_none());
    s.expect("g", 100., 10., &json!([]))?;
    s.observe(&json!({"chat":"g"}), 110.)?; // observe 包含终点，读取不包含终点。
    let row = &s.rows("SELECT * FROM expectations", [])?[0];
    assert!(row["observation"].as_str().unwrap().contains("110"));
    s.expect("g", 200., 10., &Value::Null)?;
    s.observe(&json!({"chat":"g"}), 211.)?;
    assert_eq!(
        s.expectation("g", 200.)?.unwrap()["observation"]["event"],
        "no_message_yet"
    );
    Ok(())
}
#[test]
fn prune_all_nine_categories_and_boundaries() -> Result<()> {
    let s = Store::in_memory()?;
    let now = 200000.;
    let cutoff = now - 86400.;
    s.prune(now, 1., 2)?;
    s.messages(&[
        message("old", cutoff - 0.1),
        message("boundary", cutoff),
        message("a", now),
        message("b", now),
    ])?;
    s.mark_handled("g", "a", true)?;
    s.mark_handled("gone", "a", false)?;
    s.add_thought("g", &json!({"text":"old","kind":"k"}), cutoff - 0.1)?;
    s.add_thought("g", &json!({"text":"boundary","kind":"k"}), cutoff)?;
    for ts in [cutoff - 0.1, cutoff] {
        s.decision("g", "wait", 1., &json!([]), ts)?;
        s.delivery("g", false, ts)?;
        s.assess("g", &ts.to_string(), ts, "ready", &json!({}))?;
        s.expect(&ts.to_string(), ts, 1., &json!({}))?;
    }
    s.note("g", "permanent", 0.)?;
    s.db.execute_batch("INSERT INTO learned_memories VALUES('expired','g','t','[]',199999,200000),('old','g','t','[]',0,300000),('keep','g','t','[]',200000,300000);
    INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,created,expires) VALUES('expired','g','group','long_term','a','t','[]',0,200000),('keep','g','group','long_term','b','t','[]',0,300000);
    INSERT INTO memory_revisions(memory_id,revision) VALUES('expired',1),('keep',1);
    INSERT INTO group_orientation(chat,started,sources,analysis) VALUES('g',0,'{\"history\":[1],\"notices\":[2],\"availability\":{},\"info\":true}','{\"style\":\"keep\"}');
    INSERT INTO chat_learning VALUES('g','old','[1]',0,'last',3);
    INSERT INTO calls VALUES(0);")?;
    s.prune(now, 1., 2)?;
    assert_eq!(
        s.history("g", None)?
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    assert_eq!(
        s.rows("SELECT text FROM thoughts", [])?[0]["text"],
        "boundary"
    );
    for table in [
        "thoughts",
        "learned_memories",
        "memory_layers",
        "memory_revisions",
        "decisions",
        "deliveries",
        "send_assessments",
        "expectations",
        "handled",
        "notes",
        "calls",
    ] {
        assert_eq!(
            s.rows(&format!("SELECT * FROM {table}"), [])?.len(),
            1,
            "{table}"
        );
    }
    let orientation = &s.rows("SELECT * FROM group_orientation", [])?[0];
    assert_eq!(
        serde_json::from_str::<Value>(orientation["sources"].as_str().unwrap())?,
        json!({"availability":{},"info":true})
    );
    assert_eq!(orientation["analysis"], "{\"style\":\"keep\"}");
    let learning = s.learning_state("g")?;
    assert_eq!(learning["style"], "");
    assert_eq!(learning["sources"], json!([]));
    assert_eq!(learning["epoch"], 3);
    assert_eq!(learning["last_id"], "last");
    s.prune(now, 1., 0)?;
    assert!(s.history("g", None)?.is_empty());
    assert!(s.handled("g")?.is_none());
    Ok(())
}

#[test]
fn group_hours_migrates_once_and_ingests_atomically() -> Result<()> {
    let legacy = Connection::open_in_memory()?;
    legacy.execute_batch(include_str!("schema.sql"))?;
    let now = chrono::Utc::now().timestamp() as f64;
    for (id, age, own) in [
        ("a", 100., 0),
        ("b", 200., 0),
        ("c", 86400., 0),
        ("old", 31. * 86400., 0),
        ("future", -86400., 0),
        ("self", 100., 1),
    ] {
        legacy.execute(
            "INSERT INTO messages(message_key,chat,id,sender,name,text,ts,self) VALUES(?,'g',?,'u','n','text',?,?)",
            rusqlite::params![format!("test-g-{id}"), id, now - age, own],
        )?;
    }
    // Existing retention-limited active group must be awake immediately after upgrade.
    for i in 0..507 {
        legacy.execute(
                "INSERT INTO messages(message_key,chat,id,sender,name,text,ts,self) VALUES(?,'busy',?,'u','n','text',?,0)",
                rusqlite::params![format!("test-busy-{i}"), i.to_string(), now - (i - 20).max(0) as f64 * 40.],
        )?;
    }
    let s = Store::initialize(legacy)?;
    crate::media::media_select::enable(&s)?;
    assert!(crate::media::media_select::group_activity(&s, "busy", now, 300.)?.awake);
    let expected = s.rows(
        "SELECT chat,CAST(ts/3600 AS INTEGER) AS hour,count(*) AS count,max(ts) AS updated
        FROM messages WHERE self=0 AND ts>=? AND ts<=? GROUP BY chat,hour ORDER BY chat,hour",
        rusqlite::params![now - 30. * 86400., now],
    )?;
    assert_eq!(
        s.rows("SELECT * FROM group_hours ORDER BY chat,hour", [])?,
        expected
    );
    let s = Store::initialize(s.db)?;
    assert_eq!(
        s.rows("SELECT * FROM group_hours ORDER BY chat,hour", [])?,
        expected
    );
    let m = json!({"chat":"g","id":"new","ts":now,"self":false});
    assert!(s.message(&m)?);
    assert!(!s.message(&m)?);
    assert_eq!(
        s.first("SELECT sum(count) AS n FROM group_hours WHERE chat='g'", [])?
            .unwrap()["n"],
        4
    );
    let tx = s.immediate()?;
    s.message(&json!({"chat":"g","id":"rollback","ts":now}))?;
    tx.rollback()?;
    assert_eq!(
        s.first("SELECT sum(count) AS n FROM group_hours WHERE chat='g'", [])?
            .unwrap()["n"],
        4
    );
    Ok(())
}
