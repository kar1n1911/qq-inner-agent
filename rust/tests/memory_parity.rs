#[path = "golden/mod.rs"]
mod golden;
use qq_inner_core::{
    config::{self, Agent, Emoji, Expression, Memory},
    persona::expression::{self, ExpressionMemory},
    memory::{self, LayeredMemory},
    memory::ranking,
    store::{LayeredUpdate, ScopedOptions, Store},
};
use serde_json::{json, Value};
use std::path::PathBuf;
#[path = "golden/sqlite.rs"]
mod sqlite;
fn st<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn settings(input: &Value) -> (Memory, Expression) {
    let d = config::defaults();
    (
        serde_json::from_value(config::merge(&d["agent"]["memory"], &input["settings"])).unwrap(),
        serde_json::from_value(config::merge(
            &d["agent"]["expression"],
            &input["expressions"],
        ))
        .unwrap(),
    )
}
fn clean(mut v: Value) -> Value {
    match &mut v {
        Value::Array(a) => {
            for v in a {
                *v = clean(v.take());
            }
        }
        Value::Object(o) => {
            if o.contains_key("chat") && o.contains_key("layer") {
                o.remove("id");
            }
            for v in o.values_mut() {
                *v = clean(v.take());
            }
        }
        _ => {}
    }
    v
}
fn table(s: &Store, sql: &str, decode: &[&str]) -> anyhow::Result<Vec<Value>> {
    let mut stmt = s.connection().prepare(sql)?;
    let keys: Vec<_> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut cursor = stmt.query([])?;
    let mut result = Vec::new();
    while let Some(r) = cursor.next()? {
        let mut v = json!({});
        for (i, k) in keys.iter().enumerate() {
            use rusqlite::types::ValueRef;
            v[k] = match r.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(n) => json!(n),
                ValueRef::Text(s) => json!(std::str::from_utf8(s)?),
                _ => unreachable!(),
            };
        }
        for k in decode {
            v[k] = serde_json::from_str(v[k].as_str().unwrap())?;
        }
        result.push(v);
    }
    Ok(result)
}
fn dump(s: &Store) -> anyhow::Result<Value> {
    Ok(clean(
        json!({"memory":table(s,"SELECT * FROM memory_layers ORDER BY chat,subject,layer,slot",&["sources","keywords"])? ,"revisions":table(s,"SELECT m.chat,m.subject,m.layer,m.slot,r.revision,r.text,r.sources,r.updated,r.replaced FROM memory_revisions r JOIN memory_layers m ON m.id=r.memory_id ORDER BY m.chat,m.subject,m.layer,m.slot,r.revision",&["sources"])? ,"expressions":table(s,"SELECT * FROM expressions ORDER BY chat,subject,kind,term",&["sources"])? ,"history":table(s,"SELECT * FROM messages ORDER BY chat,id",&[])?}),
    ))
}
fn run(input: &Value) -> Value {
    let (settings, es) = settings(input);
    let defaults = config::defaults();
    let s = Store::in_memory().unwrap();
    let m = LayeredMemory::new(&s);
    let e = ExpressionMemory::new(&s);
    json!(arr(&input["actions"])
        .iter()
        .map(|a| {
            let f = || -> anyhow::Result<Value> {
                let now = a["now"].as_f64().unwrap_or(1000.);
                let chat = a["chat"].as_str().unwrap_or("group:10");
                let sender = a["sender"].as_str().unwrap_or("20");
                Ok(match st(a, "op") {
                    "subjects" => json!(memory::memory_subjects(chat, sender)?),
                    "parse" => json!(memory::parse_memory_updates(
                        &a["value"],
                        arr(&a["history"]),
                        chat,
                        sender,
                        &settings
                    )?),
                    "parseExpressions" => json!(expression::parse_expressions(
                        &a["value"],
                        arr(&a["history"]),
                        chat,
                        sender
                    )?),
                    "tokens" => json!(ranking::tokens(st(a, "text"))),
                    "rank" => json!(ranking::rank_memories(
                        arr(&a["rows"]),
                        st(a, "query"),
                        now,
                        &settings,
                        a["requireMatch"] == true
                    )),
                    "message" => {
                        s.message(&a["message"])?;
                        Value::Null
                    }
                    "capture" => {
                        m.capture(&a["message"], now, &settings)?;
                        Value::Null
                    }
                    "apply" => {
                        m.apply(chat, arr(&a["updates"]), now, &settings)?;
                        Value::Null
                    }
                    "configure" => {
                        m.configure(
                            now,
                            &serde_json::from_value(config::merge(
                                &serde_json::to_value(&settings)?,
                                &a["settings"],
                            ))?,
                        )?;
                        Value::Null
                    }
                    "short" => clean(json!(m.short(
                        chat,
                        sender,
                        now,
                        &settings,
                        &arr(&a["excluded"])
                            .iter()
                            .map(|v| v.as_str().unwrap().to_string())
                            .collect::<Vec<_>>()
                    )?)),
                    "context" => clean(json!(m.context(
                        chat,
                        sender,
                        now,
                        &settings,
                        st(a, "query")
                    )?)),
                    "scoped" => clean(json!(s.retrieve_scoped(
                        chat,
                        sender,
                        st(a, "query"),
                        now,
                        &settings,
                        &ScopedOptions {
                            enabled: a["options"]["enabled"].as_bool(),
                            exclude_ids: arr(&a["options"]["excludeIds"])
                                .iter()
                                .map(|v| v.as_str().unwrap().to_string())
                                .collect(),
                            limit: a["options"]["limit"].as_u64().map(|n| n as usize)
                        }
                    )?)),
                    "learn" => json!(s.learn(
                        chat,
                        &a["update"],
                        now,
                        st(a, "lastId"),
                        &serde_json::from_value(defaults["agent"]["learning"].clone())?,
                        a["epoch"].as_i64().unwrap_or(0),
                        a.get("layered").map(|v| LayeredUpdate {
                            affect_enabled: false,
                            updates: arr(v),
                            settings: &settings,
                            expressions: a.get("expressionUpdates").map(arr),
                            expression_settings: &es
                        })
                    )?),
                    "state" => {
                        let mut v = s.learning_state(chat)?;
                        assert!(v["sources"].is_array());
                        v["rawSourcesType"] = json!("string");
                        v
                    } // Rust 已解析，oracle 显式记录 JS 原始类型。
                    "reset" => {
                        s.reset_learning(chat, now, a["subject"].as_str())?;
                        Value::Null
                    }
                    "expressionApply" => {
                        e.apply(chat, arr(&a["updates"]), now, &es)?;
                        Value::Null
                    }
                    "expressionContext" => {
                        json!(e.context(chat, sender, st(a, "query"), now, &es, &settings)?)
                    }
                    "used" => {
                        e.used(chat, arr(&a["rows"]), st(a, "text"), now)?;
                        Value::Null
                    }
                    "expressionPrune" => {
                        e.prune(
                            now,
                            &serde_json::from_value(config::merge(
                                &serde_json::to_value(&es)?,
                                &a["settings"],
                            ))?,
                        )?;
                        Value::Null
                    }
                    "decorate" => expression::decorate(
                        &a["response"],
                        &a["choices"],
                        a["max"].as_u64().unwrap() as usize,
                    ),
                    "choices" => {
                        let cfg: Emoji = serde_json::from_value(config::merge(
                            &defaults["agent"]["emoji"],
                            &a["settings"],
                        ))?;
                        expression::decoration_choices(&s, chat, now, &cfg, || {
                            a["random"].as_f64().unwrap_or(0.)
                        })?
                    }
                    "usage" => {
                        s.connection().execute(
                            "INSERT OR REPLACE INTO decoration_usage VALUES(?,?)",
                            rusqlite::params![chat, now],
                        )?;
                        Value::Null
                    }
                    "personality" => {
                        let cfg: Agent =
                            serde_json::from_value(config::merge(&defaults["agent"], &a["agent"]))?;
                        expression::personality_context(&cfg, || a["random"].as_f64().unwrap_or(0.))
                    }
                    "prune" => {
                        s.prune(now, a["days"].as_f64().unwrap_or(30.), 500)?;
                        Value::Null
                    }
                    "dump" => dump(&s)?,
                    op => panic!("unknown {op}"),
                })
            };
            match f() {
                Ok(v) => json!({"ok":v}),
                Err(e) => json!({"error":e.to_string()}),
            }
        })
        .collect::<Vec<_>>())
}
fn expected(input: &Value) -> Value {
    golden::expected(include_str!("golden/memory.json"), input)
}
fn equal(a: &Value, b: &Value, path: &str) {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => assert!(
            (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() <= 1e-12,
            "{path}: {a} != {b}"
        ),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}: array length mismatch");
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                equal(a, b, &format!("{path}/{i}"));
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{path}"
            );
            for (k, a) in a {
                equal(a, &b[k], &format!("{path}/{k}"));
            }
        }
        _ => assert_eq!(a, b, "{path}"),
    }
}
fn parity(input: Value) -> Value {
    let actual = run(&input);
    let expected = expected(&input);
    equal(&actual, &expected, "");
    actual
}
fn message(id: &str, sender: &str, ts: f64) -> Value {
    json!({"chat":"group:10","id":id,"sender":sender,"name":"同名","text":"周末一起园艺种花，今天又咕咕了","ts":ts,"self":false})
}
fn update(key: &str, layer: &str, content: &str, id: &str, ts: f64) -> Value {
    json!({"subject":"person:20","layer":layer,"operation":"upsert","key":key,"text":content,"sources":[{"id":id,"sender":"20","ts":ts}],"importance":0.8,"confidence":0.9,"keywords":["园艺"]})
}
#[test]
fn parser_batches_scope_and_unicode_match_golden() {
    let a = message("a", "20", 1000.);
    let b = message("b", "21", 1001.);
    let mut base = update("兴趣", "traits", "喜欢种花", "a", 1000.);
    base["sourceIds"] = json!(["a"]);
    let mut actions = vec![
        json!({"op":"subjects"}),
        json!({"op":"subjects","chat":"private:20"}),
        json!({"op":"subjects","chat":"private:21"}),
        json!({"op":"subjects","sender":"２０"}),
        json!({"op":"subjects","chat":"group:01"}),
    ];
    for patch in [
        json!({}),
        json!({"subject":"person:21"}),
        json!({"sourceIds":["b"]}),
        json!({"sourceIds":["missing"]}),
        json!({"subject":"group","sourceIds":["a","a"]}),
        json!({"subject":"group","sourceIds":["a","b"]}),
        json!({"operation":"forget","text":null}),
        json!({"keywords":[" a ","a"]}),
        json!({"confidence":2}),
        json!({"keywords":"bad"}),
        json!({"key":"🙂".repeat(33)}),
        json!({"text":"🙂".repeat(151)}),
        json!({"key":"\u{feff}"}),
        json!({"key":"\u{85}"}),
        json!({"layer":"short_term"}),
        json!({"operation":"recall"}),
        json!({"importance":"0.8"}),
        json!({"sourceIds":[20]}),
        json!({"keywords":["🙂".repeat(17)]}),
    ] {
        actions.push(json!({"op":"parse","value":[config::merge(&base,&patch)],"history":[a,b]}));
    }
    actions.push(json!({"op":"parse","value":[base,base],"history":[a]}));
    actions.push(json!({"op":"parse","value":[base,config::merge(&base,&json!({"sourceIds":["wrong"]}))],"history":[a]}));
    for history in [
        json!([config::merge(&a, &json!({"self":true}))]),
        json!([config::merge(&a, &json!({"chat":"group:11"}))]),
    ] {
        actions.push(json!({"op":"parse","value":[base],"history":history}));
    }
    for text in [
        "abc中文XYZ 𠀀𠀁 café ²Ⅳ",
        "a 𐐀 אְב काएक ไทย ก้ 々〆〇",
        "İΣ ΟΣ ΣΟ １２３ 中文-园艺",
        "𰀀𱍐 㐀漢字\u{0345}aa",
        "a𐐀😀e\u{301}cole",
    ] {
        actions.push(json!({"op":"tokens","text":text}));
    }
    parity(json!({"actions":actions}));
}
#[test]
fn memory_revisions_scopes_retention_and_transaction_match_golden() {
    let a = message("a", "20", 1000.);
    let b = message("b", "21", 1001.);
    let mut actions = vec![
        json!({"op":"message","message":a}),
        json!({"op":"message","message":b}),
        json!({"op":"capture","message":a}),
        json!({"op":"capture","message":b,"now":1001}),
        json!({"op":"capture","message":a,"now":1002}),
        json!({"op":"short","now":1003}),
        json!({"op":"short","now":1003,"excluded":["a"]}),
        json!({"op":"scoped","query":"园艺","now":1003}),
        json!({"op":"scoped","query":"unrelated","now":1003}),
        json!({"op":"scoped","query":"园艺","options":{"enabled":false}}),
    ];
    for (chat, subject, content) in [
        ("group:10", "person:20", "园艺约定"),
        ("group:10", "person:21", "他人秘密"),
        ("group:11", "person:20", "其他群秘密"),
        ("private:20", "person:20", "私聊秘密"),
        ("group:10", "group", "群话题园艺"),
    ] {
        let mut u = update("约定", "long_term", content, "a", 1000.);
        u["subject"] = json!(subject);
        actions.push(json!({"op":"apply","chat":chat,"updates":[u]}));
    }
    actions.push(json!({"op":"context","query":"园艺","now":1003}));
    for (content, id, ts, now) in [
        ("园艺约定", "a", 1000., 2000.),
        ("原证据改写", "a", 1000., 2001.),
        ("更新的园艺约定", "c", 2002., 2002.),
        ("过期证据不能覆盖", "old", 999., 2003.),
    ] {
        actions.push(
            json!({"op":"apply","now":now,"updates":[update("约定","long_term",content,id,ts)]}),
        );
        actions.push(json!({"op":"dump"}));
    }
    for i in 0..8 {
        actions.push(json!({"op":"apply","now":2100+i,"updates":[update("兴趣","traits",&format!("园艺{i}"),&format!("id{i}"),2100.+i as f64)]}));
    }
    actions.push(json!({"op":"configure","now":2110,"settings":{"revisionLimit":1}}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"learn","now":2200,"lastId":"b","update":{"style":{"text":"轻松","sources":["a"]},"memories":[{"text":"legacy","sources":["a"]}],"forgetIds":[]},"layered":[update("进展","long_term","发芽了","c",2200.)]}));
    actions.push(json!({"op":"state"}));
    actions.push(json!({"op":"reset","subject":"person:20","now":2201}));
    actions.push(json!({"op":"state"}));
    actions.push(json!({"op":"learn","now":2202,"epoch":0,"update":{"style":null,"memories":[],"forgetIds":[]},"layered":[update("禁止","traits","旧任务不能复活","c",2202.)]}));
    actions.push(json!({"op":"dump"}));
    for layer in ["long_term", "traits"] {
        actions.push(
            json!({"op":"apply","updates":[update("寿命",layer,"保留","new",3000.)],"now":3000}),
        );
    }
    for days in [31, 181, 366] {
        actions.push(json!({"op":"prune","now":3000+days*86400}));
        actions.push(json!({"op":"context","now":3000+days*86400}));
    }
    parity(json!({"actions":actions}));
}
#[test]
fn expressions_literal_evidence_cooldown_and_decorations_match_golden() {
    let a = message("a", "20", 1000.);
    let b = message("b", "21", 1001.);
    let c = message("c", "20", 1002.);
    let base = json!({"subject":"group","kind":"jargon","term":"咕咕","meaning":"推迟约定","situation":"轻松调侃","example":"咕咕","confidence":0.9,"sourceIds":["a","b"]});
    let mut actions = Vec::new();
    for patch in [
        json!({}),
        json!({"term":"不存在"}),
        json!({"example":"编造"}),
        json!({"subject":"person:20"}),
        json!({"subject":"person:21"}),
        json!({"confidence":null}),
        json!({"sourceIds":["a",1]}),
        json!({"kind":"affect"}),
    ] {
        actions.push(
            json!({"op":"parseExpressions","value":[config::merge(&base,&patch)],"history":[a,b]}),
        );
    }
    let short_b = config::merge(&b, &json!({"text":"咕咕"}));
    for kind in ["jargon", "expression"] {
        actions.push(json!({"op":"parseExpressions","value":[config::merge(&base,&json!({"kind":kind,"example":"今天又咕咕了"}))],"history":[a,short_b]}));
    }
    let mut u = base.clone();
    u["sources"] = json!([{"id":"a","sender":"20","ts":1000}]);
    actions.push(json!({"op":"expressionApply","updates":[u]}));
    actions.push(json!({"op":"expressionContext","query":"咕咕","now":1010}));
    u["sources"] = json!([{"id":"c","sender":"20","ts":1002}]);
    actions.push(json!({"op":"expressionApply","updates":[u],"now":1002}));
    actions.push(json!({"op":"expressionContext","query":"咕咕","now":1010}));
    u["sources"] = json!([{"id":"b","sender":"21","ts":1001}]);
    actions.push(json!({"op":"expressionApply","updates":[u],"now":1003})); // 更旧证据不降级。
    u["sources"] = json!([{"id":"d","sender":"21","ts":1004}]);
    actions.push(json!({"op":"expressionApply","updates":[u],"now":1004}));
    actions.push(json!({"op":"expressionContext","query":"咕咕","now":1010}));
    actions.push(json!({"op":"used","rows":[u],"text":"又咕咕啦","now":1010}));
    actions.push(json!({"op":"expressionContext","query":"咕咕","now":1011}));
    actions.push(json!({"op":"expressionContext","query":"咕咕","now":2810}));
    actions.push(json!({"op":"expressionContext","chat":"group:11","query":"咕咕","now":2810}));
    u["meaning"] = json!("同证据不允许改写");
    actions.push(json!({"op":"expressionApply","updates":[u],"now":2811}));
    actions.push(json!({"op":"dump"}));
    u["sources"] = json!([{"id":"new","sender":"20","ts":2812}]);
    actions.push(json!({"op":"expressionApply","updates":[u],"now":2812}));
    actions.push(json!({"op":"expressionContext","query":"咕咕","now":2813}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"parseExpressions","value":[config::merge(&base,&json!({"subject":"person:20","sourceIds":["a","c"]}))],"history":[a,c]}));
    actions.push(json!({"op":"expressionPrune","now":2812+366*86400}));
    actions.push(json!({"op":"dump"}));
    let choices = json!({"symbols":["🙂","👩‍🌾",""],"faceIds":["14"]});
    for max in [0, 1, 2, 3, 5, 10] {
        for response in [
            json!({"text":"  你好世界🙂 ","emoji":"🙂","faceId":"14"}),
            json!({"text":"你好世界","emoji":"🙂"}),
            json!({"text":"你好世界","emoji":"👩‍🌾"}),
            json!({"text":"abc","emoji":"未知","faceId":"999"}),
            json!({"text":"abc","faceId":"14"}),
        ] {
            actions.push(json!({"op":"decorate","response":response,"choices":choices,"max":max}));
        }
    }
    actions.push(json!({"op":"choices","settings":{"enabled":false,"probability":1}}));
    actions
        .push(json!({"op":"choices","settings":{"enabled":true,"probability":0.5},"random":0.5}));
    actions.push(json!({"op":"choices","settings":{"enabled":true,"probability":1}}));
    actions.push(json!({"op":"usage"}));
    actions.push(json!({"op":"choices","now":1001,"settings":{"enabled":true,"probability":1}}));
    actions.push(json!({"op":"choices","now":3000,"settings":{"enabled":true,"probability":1,"cooldownSeconds":2000}}));
    actions.push(json!({"op":"personality","agent":{"persona":"自定义身份","personality":{"variants":["轻松","严谨"],"variantProbability":1}},"random":0.75}));
    parity(json!({"actions":actions}));
}
#[test]
fn ranking_rrf_overlap_duplicates_owner_and_budget_match_golden() {
    let mut rows = Vec::new();
    for i in 0..48 {
        rows.push(json!({"id":format!("id{i:03}"),"subject":format!("person:{}",i%3),"layer":if i==0{"owner_note"}else{"long_term"},"text":format!("{} 园艺{}",["今天种花","今天种花","音乐展览","café","garden flowers","𠀀𠀁"][i%6],i%9),"slot":format!("slot{}",i%4),"keywords":if i%5==0{json!(["rareword"])}else{json!([])},"updated":1000+i*11,"importance":(i%7) as f64/7.,"confidence":if i%9==0{0.1}else{0.9}}));
    }
    let input = json!({"actions":[{"op":"rank","rows":rows,"query":"园艺 rareword","now":2000},{"op":"rank","rows":rows,"query":"不存在的xyz","requireMatch":true,"now":2000},{"op":"rank","rows":rows,"query":"","now":2000}]});
    parity(input);
    let settings = settings(&json!({})).0;
    let (_, stats) = ranking::rank_with_stats(&rows, "园艺 rareword", 2000., &settings, false);
    assert_eq!(stats.tokenizations, 1 + 2 * rows.len());
    assert!(stats.overlaps > rows.len());
}
#[test]
fn incremental_batch_converges_to_one_final_enforce_and_js() {
    let settings = settings(&json!({})).0;
    let incremental = Store::in_memory().unwrap();
    let once = Store::in_memory().unwrap();
    let batch = LayeredMemory::new(&incremental);
    let raw = LayeredMemory::new(&once);
    let mut actions = Vec::new();
    for i in 0..500 {
        let sender = format!("{}", 20 + i % 125);
        let message = message(&format!("m{i}"), &sender, 1000. + i as f64);
        let now = 1000. + i as f64;
        batch.capture(&message, now, &settings).unwrap();
        for subject in memory::memory_subjects("group:10", &sender).unwrap() {
            raw.put(
                "group:10",
                &subject,
                "short_term",
                st(&message, "id"),
                &format!(
                    "{} ({}): {}",
                    st(&message, "name"),
                    sender,
                    st(&message, "text")
                ),
                &[json!({"id":message["id"],"sender":sender,"ts":now})],
                0.5,
                now,
                now + settings.short_hours * 3600.,
                &json!({"confidence":1}),
                3,
            )
            .unwrap();
        }
        actions.push(json!({"op":"capture","message":message,"now":now}));
    }
    batch.configure(1500., &settings).unwrap();
    raw.enforce("group:10", 1500., &settings, None).unwrap();
    equal(
        &dump(&incremental).unwrap(),
        &dump(&once).unwrap(),
        "convergence",
    );
    actions.push(json!({"op":"configure","now":1500}));
    actions.push(json!({"op":"dump"}));
    let expected = expected(&json!({"actions":actions}));
    equal(
        &dump(&incremental).unwrap(),
        &expected.as_array().unwrap().last().unwrap()["ok"],
        "js convergence",
    );
    // 无入站时维护同样会清理已过期的静默 chat。
    batch.maintain(1500. + 73. * 3600., &settings).unwrap();
    assert!(dump(&incremental).unwrap()["memory"]
        .as_array()
        .unwrap()
        .is_empty());
}
#[test]
fn capacity_forgetting_and_live_settings_match_golden() {
    let mut actions = Vec::new();
    for i in 0..30 {
        let mut u = update(
            &format!("k{i}"),
            "long_term",
            &format!("item{i}"),
            &format!("id{i}"),
            1000. + i as f64,
        );
        u["importance"] = json!(if i % 2 == 0 { 0.9 } else { 0.1 });
        actions.push(json!({"op":"apply","updates":[u],"now":1000+i}));
    }
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"configure","now":1040,"settings":{"longChars":20}}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"apply","updates":[{"subject":"person:20","layer":"long_term","key":"k28","operation":"forget"}],"now":1041}));
    actions.push(json!({"op":"dump"}));
    for i in 0..4 {
        actions.push(json!({"op":"capture","message":message(&format!("m{i}"),&format!("{}",20+i),2000.+i as f64),"now":2000+i}));
    }
    actions.push(json!({"op":"configure","now":2010,"settings":{"maxPeople":1,"shortLimit":1}}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"configure","now":2010+3601,"settings":{"shortHours":1}}));
    actions.push(json!({"op":"dump"}));
    parity(json!({"actions":actions}));
}
#[test]
fn learning_rolls_back_revision_and_expression_on_failure() {
    let s = Store::in_memory().unwrap();
    let m = LayeredMemory::new(&s);
    let (settings, es) = settings(&json!({}));
    m.apply(
        "group:10",
        &[update("key", "traits", "原文", "a", 1000.)],
        1000.,
        &settings,
    )
    .unwrap();
    let before = dump(&s).unwrap();
    s.connection().execute_batch("CREATE TEMP TRIGGER reject_learning BEFORE INSERT ON chat_learning BEGIN SELECT RAISE(ABORT,'test rollback'); END;").unwrap();
    let revised = vec![update("key", "traits", "新文本", "b", 1001.)];
    let learning = serde_json::from_value(config::defaults()["agent"]["learning"].clone()).unwrap();
    assert!(s.learn("group:10",&json!({"style":null,"memories":[],"forgetIds":[]}),1001.,"b",&learning,0,Some(LayeredUpdate{affect_enabled:false,updates:&revised,settings:&settings,expressions:Some(&[json!({"subject":"person:20","kind":"jargon","term":"咕咕","meaning":"推迟","situation":"聊天","example":"咕咕","confidence":0.9,"sources":[{"id":"b","sender":"20","ts":1001}]})]),expression_settings:&es})).is_err());
    assert_eq!(dump(&s).unwrap(), before);
    assert!(s.connection().is_autocommit());
    assert_eq!(s.learning_state("group:10").unwrap()["epoch"], 0);
}
#[test]
fn people_capacity_triggers_immediately_and_matches_js_churn() {
    let mut actions = Vec::new();
    for i in 0..40 {
        actions.push(json!({"op":"capture","message":message(&format!("m{i}"),&format!("{}",20+i%7),1000.+i as f64),"now":1000+i}));
    }
    actions.push(json!({"op":"configure","now":1040}));
    actions.push(json!({"op":"dump"}));
    parity(json!({"settings":{"maxPeople":5,"shortLimit":10},"actions":actions}));
}
#[test]
fn evidence_caps_empty_sources_and_expired_duplicate_do_not_revive() {
    let mut actions = Vec::new();
    for i in 0..16 {
        actions.push(json!({"op":"apply","updates":[update("topic","traits",&format!("text{i}"),&format!("id{i}"),1000.+i as f64)],"now":1000+i}));
    }
    let mut no_evidence = update("topic", "traits", "no fresh evidence", "unused", 2000.);
    no_evidence["sources"] = json!([]);
    actions.push(json!({"op":"apply","updates":[no_evidence],"now":2000}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"apply","updates":[update("topic","traits","no fresh evidence","id15",1015.)],"now":1015+181*86400}));
    actions.push(json!({"op":"dump"}));
    parity(json!({"actions":actions}));
}
#[test]
fn expression_retention_capacity_and_personal_isolation_match_golden() {
    let mut actions = Vec::new();
    for i in 0..6 {
        actions.push(json!({"op":"expressionApply","now":1000+i,"updates":[{"subject":"person:20","kind":"expression","term":format!("term{i}"),"meaning":"garden","situation":"casual","example":"literal","confidence":0.9,"sources":[{"id":format!("a{i}"),"sender":"20","ts":1000+i},{"id":format!("b{i}"),"sender":"20","ts":1000+i}]}]}));
    }
    actions.push(json!({"op":"expressionContext","sender":"21","query":"garden","now":1010}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"reset","subject":"person:21","now":1011}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"expressionPrune","settings":{"maxEntries":1},"now":1012}));
    actions.push(json!({"op":"dump"}));
    actions.push(json!({"op":"expressionPrune","settings":{"retentionDays":1},"now":1005+86400}));
    actions.push(json!({"op":"dump"}));
    parity(json!({"expressions":{"maxEntries":3},"actions":actions}));
}
#[test]
fn measured_performance() {
    let (settings, _) = settings(&json!({}));
    let seed: Vec<_> = (0..200)
        .map(|i| {
            message(
                &format!("seed{i}"),
                &format!("{}", 20 + i),
                1000. + i as f64,
            )
        })
        .collect();
    let messages: Vec<_> = (0..400)
        .map(|i| {
            message(
                &format!("live{i}"),
                &format!("{}", 20 + i % 200),
                2000. + i as f64,
            )
        })
        .collect();
    let rows:Vec<_>=(0..80).map(|i|json!({"id":format!("id{i:03}"),"subject":"person:20","layer":"long_term","slot":format!("slot{i}"),"text":format!("园艺种花 周末花园计划 {i} garden flowers rareword"),"keywords":["园艺"],"updated":1000+i,"importance":(i%7) as f64/7.,"confidence":0.9})).collect();
    let input = json!({"seed":seed,"messages":messages,"rows":rows});
    // 原先忽略的基准现默认执行：输出和排名是固化金标准，耗时只报告、不作机器相关断言。
    let js = expected(&input);
    let mut capture_times = Vec::new();
    let mut rank_times = Vec::new();
    let mut tokenizations = 0;
    for _ in 0..3 {
        let s = Store::in_memory().unwrap();
        let m = LayeredMemory::new(&s);
        for item in &seed {
            m.capture(item, item["ts"].as_f64().unwrap(), &settings)
                .unwrap();
        }
        let start = std::time::Instant::now();
        for item in &messages {
            m.capture(item, item["ts"].as_f64().unwrap(), &settings)
                .unwrap();
        }
        m.configure(3000., &settings).unwrap();
        capture_times.push(start.elapsed().as_secs_f64() * 1000.);
        equal(
            &dump(&s).unwrap()["memory"],
            &js["memory"],
            "benchmark final rows",
        );
        let start = std::time::Instant::now();
        let (ranked, stats) =
            ranking::rank_with_stats(&rows, "园艺 rareword", 3000., &settings, false);
        rank_times.push(start.elapsed().as_secs_f64() * 1000.);
        tokenizations = stats.tokenizations;
        equal(&json!(ranked), &js["ranked"], "benchmark rank");
    }
    capture_times.sort_by(f64::total_cmp);
    rank_times.sort_by(f64::total_cmp);
    assert_eq!(tokenizations, 161);
    // Rust 缓存分词，JS 旧实现会重复分词；保留 Rust 的 161 次精确断言，不能要求二者相等。
    assert!(tokenizations as u64 <= js["tokenizations"].as_u64().unwrap());
    println!(
        "PERF Rust capture median {:.3} ms; ranking median {:.3} ms; tokenizations {}",
        capture_times[1], rank_times[1], tokenizations
    );
}
#[test]
fn shared_capacity_index_survives_learning_reset_and_rollback() {
    let s = Store::in_memory().unwrap();
    let m = LayeredMemory::new(&s);
    let (mut settings, es) = settings(&json!({}));
    settings.max_people = 1.;
    m.capture(&message("a", "20", 1000.), 1000., &settings)
        .unwrap();
    let learning = serde_json::from_value(config::defaults()["agent"]["learning"].clone()).unwrap();
    let mut u = update("key", "traits", "test", "b", 1001.);
    u["subject"] = json!("person:21");
    s.learn(
        "group:10",
        &json!({"memories":[],"forgetIds":[]}),
        1001.,
        "b",
        &learning,
        0,
        Some(LayeredUpdate {
            affect_enabled: false,
            updates: &[u.clone()],
            settings: &settings,
            expressions: None,
            expression_settings: &es,
        }),
    )
    .unwrap();
    m.capture(&message("c", "22", 1002.), 1002., &settings)
        .unwrap();
    assert!(m
        .rows("group:10", "person:21", "traits", 1003.)
        .unwrap()
        .is_empty());
    s.reset_learning("group:10", 1003., Some("person:22"))
        .unwrap();
    m.capture(&message("d", "23", 1004.), 1004., &settings)
        .unwrap();
    s.connection().execute_batch("CREATE TEMP TRIGGER fail BEFORE INSERT ON chat_learning BEGIN SELECT RAISE(ABORT,'rollback'); END;").unwrap();
    assert!(s
        .learn(
            "group:10",
            &json!({"memories":[],"forgetIds":[]}),
            1005.,
            "b",
            &learning,
            1,
            Some(LayeredUpdate {
                affect_enabled: false,
                updates: &[u],
                settings: &settings,
                expressions: None,
                expression_settings: &es
            })
        )
        .is_err());
    m.capture(&message("e", "21", 1006.), 1006., &settings)
        .unwrap();
    assert!(m
        .rows("group:10", "person:23", "short_term", 1007.)
        .unwrap()
        .is_empty());
}
#[test]
fn shared_sqlite_memory_roundtrip_and_reopen() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(format!(
            "memory-shared-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("agent.sqlite");
    let (settings, _) = settings(&json!({}));
    {
        let s = Store::open(&path).unwrap();
        s.message(&message("a", "20", 1000.)).unwrap();
        LayeredMemory::new(&s)
            .apply(
                "group:10",
                &[update("兴趣", "traits", "园艺", "a", 1000.)],
                1000.,
                &settings,
            )
            .unwrap();
        let snapshot = expected(&json!({"kind":"shared_database"}));
        // 原始 SQL 字段与 JS 读取到的快照一致（仅 UUID 不参与值比较）。
        sqlite::assert_rows(&s, &snapshot["before"]);
        let rows = LayeredMemory::new(&s)
            .rows("group:10", "person:20", "traits", 1001.)
            .unwrap();
        assert_eq!(rows[0]["text"], "园艺");
        assert_eq!(rows[0]["sources"][0]["id"], "a");
        assert_eq!(rows[0]["keywords"][0], "园艺");
        // 原生写路径也要产生与历史 JS 更新相同的字段和 revision。
        LayeredMemory::new(&s).apply("group:10", &[json!({"subject":"person:20","layer":"traits","key":"兴趣","operation":"upsert","text":"音乐","importance":0.8,"sources":[{"id":"b","sender":"20","ts":1002}]})], 1002., &settings).unwrap();
        sqlite::assert_rows(&s, &snapshot["after"]);
        // 回放真实 JS 原始行，验证 Rust 能读取旧库 JSON 表示及 revision 关联。
        s.connection()
            .execute_batch("DELETE FROM memory_revisions; DELETE FROM memory_layers;")
            .unwrap();
        sqlite::insert(s.connection(), &snapshot["after"]);
        let r = LayeredMemory::new(&s)
            .rows("group:10", "person:20", "traits", 1003.)
            .unwrap();
        assert_eq!(r[0]["text"], "音乐");
        assert_eq!(r[0]["revision"], 2);
        assert!(r[0]["sources"].is_array());
    }
    {
        let s = Store::open(&path).unwrap();
        assert_eq!(dump(&s).unwrap()["revisions"][0]["text"], "园艺");
        s.reset_learning("group:10", 1004., Some("person:20"))
            .unwrap();
        assert_eq!(s.history("group:10", None).unwrap().len(), 1);
        assert!(dump(&s).unwrap()["revisions"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn utf16_capture_boundary_matches_sqlite_replacement() {
    let mut m = message("a", "20", 1000.);
    m["text"] = json!("🙂🙂");
    parity(
        json!({"settings":{"shortChars":10},"actions":[{"op":"capture","message":m},{"op":"dump"}]}),
    );
}
#[test]
fn tied_non_uuid_ids_expose_locale_ordering_boundary() {
    let rows = json!([{"id":"a","subject":"person:20","layer":"traits","text":"first","updated":1000},{"id":"A","subject":"person:20","layer":"traits","text":"second","updated":1000}]);
    let input = json!({"actions":[{"op":"rank","rows":rows,"query":"","now":1000}]});
    // 没有 ICU 排序依赖：UUID 的排序一致，大小写/重音等人为 ID 的同分排序记录为已知边界。
    assert_eq!(run(&input)[0]["ok"][0]["id"], "A");
    assert_eq!(expected(&input)[0]["ok"][0]["id"], "a");
}
#[test]
fn scoped_notes_and_context_obey_distinct_budgets_and_match_gates() {
    let s = Store::in_memory().unwrap();
    let (mut settings, _) = settings(&json!({}));
    let m = LayeredMemory::new(&s);
    s.note("group:10", "owner note", 1000.).unwrap();
    s.note("group:11", "other chat", 1000.).unwrap();
    let notes = s
        .retrieve_scoped(
            "group:10",
            "20",
            "unmatched",
            1001.,
            &settings,
            &ScopedOptions {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["layer"], "owner_note");
    assert_eq!(notes[0]["sources"], json!([]));
    settings.recall_chars = 4.;
    assert!(s
        .retrieve_scoped(
            "group:10",
            "20",
            "unmatched",
            1001.,
            &settings,
            &ScopedOptions::default()
        )
        .unwrap()
        .is_empty());
    settings.recall_chars = 8.;
    let mut u = update("one", "long_term", "园艺花展", "a", 1000.);
    m.apply("group:10", &[u.clone()], 1000., &settings).unwrap();
    u["key"] = json!("two");
    u["layer"] = json!("traits");
    u["text"] = json!("喜欢园艺");
    m.apply("group:10", &[u], 1001., &settings).unwrap();
    let rows = m
        .context("group:10", "20", 1002., &settings, "unmatched")
        .unwrap();
    assert_eq!(rows[1]["long_term"].as_array().unwrap().len(), 1);
    assert_eq!(rows[1]["traits"].as_array().unwrap().len(), 1);
    settings.recall_chars = 4.;
    let rows = m
        .context("group:10", "20", 1002., &settings, "unmatched")
        .unwrap();
    assert_eq!(
        rows[1]["long_term"].as_array().unwrap().len()
            + rows[1]["traits"].as_array().unwrap().len(),
        1
    );
}
