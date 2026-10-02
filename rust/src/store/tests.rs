use super::*;
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
    assert_eq!(again.schema()?["tables"].as_object().unwrap().len(), 17);
    Ok(())
}

fn message(id: &str, ts: f64) -> Value {
    json!({"chat":"g","id":id,"sender":"1","name":"名字","text":"中文🙂","ts":ts})
}
#[test]
fn messages_history_batch_and_active_chats() -> Result<()> {
    let s = Store::in_memory()?;
    assert!(s.history("g", None)?.is_empty());
    assert!(s.active_chats(0.)?.is_empty());
    assert!(s.message(&message("a", 10.25))?);
    assert!(!s.message(&message("a", 99.))?);
    assert_eq!(
        s.messages(&[message("b", 10.25), message("a", 88.), message("c", 9.)])?,
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
