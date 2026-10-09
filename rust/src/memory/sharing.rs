//! §15: symmetric, hourly group similarity. Never exports source records or examples.
use super::{array, num, text, text::terms};
use crate::{
    config::{Expression, Memory},
    store::Store,
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};

#[derive(Default)]
pub(crate) struct Cache {
    at: Option<f64>,
    features: BTreeMap<String, HashSet<String>>,
    pub reports: BTreeMap<String, Value>,
}

pub fn share(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.;
    }
    ((a.intersection(b).count() as f64 / a.len().min(b.len()) as f64 - 0.35) / (0.75 - 0.35))
        .clamp(0., 1.)
}

fn group(chat: &str) -> bool {
    chat.strip_prefix("group:").is_some_and(|id| {
        id.starts_with(|c: char| ('1'..='9').contains(&c)) && id.bytes().all(|c| c.is_ascii_digit())
    })
}

/// A closed output schema, enforced at the data exit even for direct callers.
/// No row cloning: sources, keywords, examples, IDs and identity fields cannot escape.
fn export(row: &Value) -> Result<Value> {
    ensure!(
        group(text(row, "chat")) && row["subject"] == "group",
        "invalid_shared_scope"
    );
    let mut out = json!({"subject":"group", "sourceChat":row["chat"]});
    if matches!(text(row, "layer"), "traits" | "long_term") {
        ensure!(row["text"].is_string(), "invalid_shared_text");
        out["layer"] = row["layer"].clone();
        out["text"] = row["text"].clone();
    } else {
        ensure!(
            matches!(text(row, "kind"), "expression" | "jargon"),
            "invalid_shared_kind"
        );
        for key in ["kind", "term", "meaning", "situation"] {
            ensure!(row[key].is_string(), "invalid_shared_text");
            out[key] = row[key].clone();
        }
    }
    const ALLOWED: &[&str] = &[
        "subject",
        "sourceChat",
        "layer",
        "text",
        "kind",
        "term",
        "meaning",
        "situation",
    ];
    ensure!(
        out.as_object()
            .unwrap()
            .keys()
            .all(|k| ALLOWED.contains(&k.as_str())),
        "invalid_shared_field"
    );
    Ok(out)
}

fn candidates(
    db: &Store,
    now: f64,
    settings: &Memory,
    expressions: &Expression,
) -> Result<Vec<Value>> {
    let mut rows = db.rows("SELECT * FROM memory_layers WHERE subject='group' AND layer IN ('traits','long_term') AND expires>? AND confidence>=? ORDER BY chat,layer,slot", params![now, settings.min_confidence])?;
    rows.retain(|r| {
        num(r, "updated")
            + if r["layer"] == "traits" {
                settings.trait_days
            } else {
                settings.long_days
            } * 86400.
            > now
    });
    rows.extend(db.rows("SELECT * FROM expressions WHERE subject='group' AND kind IN ('expression','jargon') ORDER BY chat,kind,term", [])?);
    rows.retain(|r| {
        group(text(r, "chat"))
            && (!r["kind"].is_string()
                || (expressions.use_learned
                    && num(r, "updated") > now - expressions.retention_days * 86400.
                    && num(r, "confidence") >= expressions.min_confidence))
    });
    // Privacy veto only: chat records never contribute features or output. Also
    // reject summaries accidentally containing a known nickname, identity or
    // complete source utterance, rather than trying to rewrite private content.
    let chats: HashSet<_> = rows.iter().map(|r| text(r, "chat").to_owned()).collect();
    let mut forbidden = BTreeMap::<String, HashSet<String>>::new();
    for chat in chats {
        for message in db.rows(
            "SELECT id,name,sender,text FROM messages WHERE chat=?",
            [&chat],
        )? {
            for key in ["id", "name", "sender", "text"] {
                let value = text(&message, key);
                if !value.is_empty() {
                    forbidden
                        .entry(chat.clone())
                        .or_default()
                        .insert(value.to_lowercase());
                }
            }
        }
    }
    rows.retain(|row| {
        let Ok(safe) = export(row) else {
            return false;
        };
        let content = format!(
            "{}\n{}",
            ["text", "term", "meaning", "situation"]
                .map(|k| text(&safe, k).to_lowercase())
                .join("\n"),
            text(row, "keywords").to_lowercase()
        );
        if forbidden
            .get(text(row, "chat"))
            .is_some_and(|values| values.iter().any(|v| content.contains(v)))
        {
            return false;
        }
        let sources: Value = serde_json::from_str(text(row, "sources")).unwrap_or(Value::Null);
        if row["kind"].is_string()
            && (array(&sources).len() < 2
                || array(&sources)
                    .iter()
                    .filter_map(|s| s["sender"].as_str())
                    .collect::<HashSet<_>>()
                    .len()
                    < 2)
        {
            return false;
        }
        !array(&sources).iter().any(|source| {
            if text(source, "chat").starts_with("private:") {
                return true;
            }
            ["id", "sender", "name", "card", "text"].iter().any(|k| {
                let value = text(source, k);
                !value.is_empty() && content.contains(&value.to_lowercase())
            })
        })
    });
    Ok(rows)
}

/// Keywords are the already summarized topic neighborhood. Count across group
/// summaries only; never derive sharing features from messages or person rows.
fn features(rows: &[Value]) -> Result<BTreeMap<String, HashSet<String>>> {
    let mut result = BTreeMap::<String, HashSet<String>>::new();
    let mut frequency = BTreeMap::<(String, String), usize>::new();
    for row in rows {
        let safe = export(row)?;
        let set = result.entry(text(row, "chat").into()).or_default();
        set.extend(terms(if safe["text"].is_string() {
            text(&safe, "text")
        } else {
            text(&safe, "term")
        }));
        let keywords: Value = serde_json::from_str(text(row, "keywords")).unwrap_or(Value::Null);
        let tokens: HashSet<_> = array(&keywords)
            .iter()
            .filter_map(Value::as_str)
            .flat_map(terms)
            .collect();
        for token in tokens {
            *frequency
                .entry((text(row, "chat").into(), token))
                .or_default() += 1;
        }
    }
    for ((chat, token), count) in frequency {
        if count >= 2 {
            result.entry(chat).or_default().insert(token);
        }
    }
    Ok(result)
}

/// Select independently in both directions from the same cached feature sets.
/// A positive weight reserves ceil(weight * eligible rows) per source group, so
/// query ranking / a busy recipient cannot make a similar pair one-way only.
/// Data is reread every time: deletion, expiry and disabling take effect at once.
pub fn recall(
    db: &Store,
    chat: &str,
    now: f64,
    settings: &Memory,
    expression_settings: &Expression,
    expressions: bool,
) -> Result<Vec<Value>> {
    if settings.cross_group_disabled || !group(chat) {
        return Ok(vec![]);
    }
    let rows = candidates(db, now, settings, expression_settings)?;
    if !rows.iter().any(|r| text(r, "chat") == chat) {
        return Ok(vec![]);
    }
    let mut cache = db.group_sharing.borrow_mut();
    if cache.at.is_none_or(|at| now < at || now - at >= 3600.) {
        cache.features = features(&rows)?;
        cache.at = Some(now);
    }
    let Some(own) = cache.features.get(chat) else {
        return Ok(vec![]);
    };
    let mut output = Vec::new();
    for (other, other_features) in &cache.features {
        if other == chat {
            continue;
        }
        let weight = share(own, other_features);
        if weight == 0. {
            continue;
        }
        let eligible: Vec<_> = rows
            .iter()
            .filter(|r| text(r, "chat") == other && r["kind"].is_string() == expressions)
            .collect();
        let count = (eligible.len() as f64 * weight).ceil() as usize;
        for row in eligible.into_iter().take(count) {
            let mut safe = export(row)?;
            safe["share"] = json!(weight);
            output.push(safe);
        }
    }
    let evaluated_at = cache.at;
    cache.reports.insert(format!("{chat}/{}", if expressions {"expressions"} else {"memory"}), json!({"chat":chat,"at":now,"evaluatedAt":evaluated_at,"count":output.len(),"entries":output}));
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config, memory::LayeredMemory, persona::expression::ExpressionMemory};

    fn settings() -> (Memory, Expression) {
        let d = config::defaults();
        (
            serde_json::from_value(d["agent"]["memory"].clone()).unwrap(),
            serde_json::from_value(d["agent"]["expression"].clone()).unwrap(),
        )
    }
    fn memory(db: &Store, chat: &str, subject: &str, key: &str, body: &str) {
        db.execute("INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,importance,created,updated,expires,confidence,keywords) VALUES(?,?,?,'traits',?,?,'[]',0.8,1000,1000,999999,0.9,'[]')",
            params![format!("{chat}/{subject}/{key}"),chat,subject,key,body]).unwrap();
    }
    fn expression(db: &Store, chat: &str, subject: &str) {
        db.execute("INSERT INTO expressions(chat,subject,kind,term,meaning,situation,example,confidence,sources,updated) VALUES(?,?,'jargon','RUST','compiled language','technical discussion','RAW_EXAMPLE',0.9,?,1000)",
            params![chat,subject,json!([{"id":"SOURCE_ID_ONE","sender":"9911","name":"NICKNAME","card":"GROUP_CARD"},{"id":"SOURCE_ID_TWO","sender":"9922"}]).to_string()]).unwrap();
    }
    #[test]
    fn continuous_monotone_symmetric_mapping() {
        let a: HashSet<_> = (0..100).map(|i| format!("token{i}")).collect();
        let mut previous = 0.;
        for common in 0..=100 {
            let b = (0..common)
                .map(|i| format!("token{i}"))
                .chain((common..100).map(|i| format!("other{i}")))
                .collect();
            let weight = share(&a, &b);
            assert_eq!(weight, share(&b, &a));
            assert!(weight >= previous);
            let expected = ((common as f64 / 100. - 0.35) / (0.75 - 0.35)).clamp(0., 1.);
            assert!((weight - expected).abs() < 1e-12);
            if common <= 35 {
                assert_eq!(weight, 0.);
            }
            if common >= 75 {
                assert_eq!(weight, 1.);
            }
            previous = weight;
        }
        assert_eq!(share(&HashSet::new(), &a), 0.);
        assert_eq!(share(&terms("Rust"), &terms("rust sqlite")), 1.);
    }
    #[test]
    fn export_rejects_non_whitelisted_scopes_and_drops_metadata() {
        for (chat, subject, layer) in [
            ("private:1", "group", "traits"),
            ("group:1", "person:2", "traits"),
            ("group:1", "group", "short_term"),
        ] {
            assert!(
                export(&json!({"chat":chat,"subject":subject,"layer":layer,"text":"secret"}))
                    .is_err()
            );
        }
        let safe = export(&json!({"chat":"group:1","subject":"group","layer":"traits","text":"rust","sources":[{"name":"nickname"}],"id":"source","name":"nickname","card":"card","example":"body"})).unwrap();
        assert_eq!(
            safe,
            json!({"sourceChat":"group:1","subject":"group","layer":"traits","text":"rust"})
        );
    }
    #[test]
    fn context_is_bidirectional_and_zero_leakage() {
        let db = Store::in_memory().unwrap();
        let (m, e) = settings();
        for chat in ["group:1", "group:2"] {
            memory(&db, chat, "group", "safe", "rust compiler");
            memory(&db, chat, "person:77", "person", "PERSON_SECRET");
            expression(&db, chat, "group");
            expression(&db, chat, "person:77");
            db.execute("INSERT INTO messages(chat,id,sender,name,text,ts) VALUES(?,'MESSAGE_ID','8811','MESSAGE_NICK','RAW_CHAT_BODY',1000)",[chat]).unwrap();
            memory(&db, chat, "group", "bad-name", "rust MESSAGE_NICK");
            memory(&db, chat, "group", "bad-body", "rust RAW_CHAT_BODY");
            memory(&db, chat, "group", "bad-identity", "rust 8811");
        }
        memory(
            &db,
            "private:3",
            "group",
            "private",
            "rust compiler PRIVATE_SECRET",
        );
        for (chat, other) in [("group:1", "group:2"), ("group:2", "group:1")] {
            let context = LayeredMemory::new(&db)
                .context(chat, "77", 1000., &m, "")
                .unwrap();
            let foreign: Vec<_> = context
                .iter()
                .filter(|s| s["sourceChat"] == other)
                .collect();
            assert_eq!(foreign.len(), 1);
            assert_eq!(foreign[0]["traits"].as_array().unwrap().len(), 1);
            let expressions = ExpressionMemory::new(&db)
                .context(chat, "77", "rust", 1000., &e, &m)
                .unwrap();
            let foreign_expressions: Vec<_> = expressions
                .iter()
                .filter(|s| s["sourceChat"] == other)
                .collect();
            assert_eq!(foreign_expressions.len(), 1);
            let output = json!([foreign, foreign_expressions]).to_string();
            for secret in [
                "PERSON_SECRET",
                "PRIVATE_SECRET",
                "RAW_CHAT_BODY",
                "RAW_EXAMPLE",
                "MESSAGE_NICK",
                "NICKNAME",
                "GROUP_CARD",
                "SOURCE_ID",
                "MESSAGE_ID",
                "8811",
                "9911",
                "9922",
                "sources",
                "example",
                "person:",
            ] {
                assert!(!output.contains(secret), "leaked {secret}: {output}");
            }
        }
        assert!(recall(&db, "private:3", 1000., &m, &settings().1, false)
            .unwrap()
            .is_empty());
        assert!(!db.group_sharing.borrow().reports.is_empty());
    }
    #[test]
    fn zero_share_preserves_serialized_context() {
        let db = Store::in_memory().unwrap();
        let (mut m, e) = settings();
        memory(&db, "group:1", "group", "a", "rust compiler");
        memory(&db, "group:2", "group", "b", "baking bread");
        let run = |settings: &Memory| {
            json!([
                LayeredMemory::new(&db)
                    .context("group:1", "77", 1000., settings, "rust")
                    .unwrap(),
                ExpressionMemory::new(&db)
                    .context("group:1", "77", "rust", 1000., &e, settings)
                    .unwrap()
            ])
            .to_string()
        };
        m.cross_group_disabled = true;
        let before = run(&m);
        m.cross_group_disabled = false;
        assert_eq!(before, run(&m));
    }
    #[test]
    fn hourly_features_but_live_deletion_and_switch() {
        let db = Store::in_memory().unwrap();
        let (mut m, _) = settings();
        memory(&db, "group:1", "group", "a", "rust compiler");
        memory(&db, "group:2", "group", "b", "rust compiler");
        assert_eq!(
            recall(&db, "group:1", 1000., &m, &settings().1, false)
                .unwrap()
                .len(),
            1
        );
        db.execute(
            "UPDATE memory_layers SET text='baking bread' WHERE chat='group:2'",
            [],
        )
        .unwrap();
        assert_eq!(
            recall(&db, "group:1", 1001., &m, &settings().1, false)
                .unwrap()
                .len(),
            1
        );
        m.cross_group_disabled = true;
        assert!(recall(&db, "group:1", 1002., &m, &settings().1, false)
            .unwrap()
            .is_empty());
        m.cross_group_disabled = false;
        assert!(recall(&db, "group:1", 4600., &m, &settings().1, false)
            .unwrap()
            .is_empty());
        db.execute("UPDATE memory_layers SET text='rust compiler'", [])
            .unwrap();
        assert_eq!(
            recall(&db, "group:1", 8200., &m, &settings().1, false)
                .unwrap()
                .len(),
            1
        );
        db.execute("DELETE FROM memory_layers WHERE chat='group:2'", [])
            .unwrap();
        assert!(recall(&db, "group:1", 8201., &m, &settings().1, false)
            .unwrap()
            .is_empty());
        assert!(recall(&db, "group:2", 8201., &m, &settings().1, false)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn feature_normalization_and_frequent_topic_keywords() {
        let rows = vec![
            json!({"chat":"group:1","subject":"group","layer":"traits","text":"RUST 中文","keywords":"[\"SQLite\",\"rare\"]"}),
            json!({"chat":"group:1","subject":"group","layer":"long_term","text":"Compiler","keywords":"[\"sqlite\"]"}),
            json!({"chat":"group:1","subject":"group","kind":"jargon","term":"LLVM","meaning":"ignored","situation":"technical","example":"secret"}),
        ];
        assert_eq!(
            features(&rows).unwrap()["group:1"],
            terms("rust 中文 compiler sqlite llvm")
        );
    }
    #[test]
    fn more_overlap_selects_more_rows_in_both_directions() {
        let mut previous = 0;
        for common in 0..=10 {
            let db = Store::in_memory().unwrap();
            let (m, _) = settings();
            let a = (0..10)
                .map(|i| format!("token{i}"))
                .collect::<Vec<_>>()
                .join(" ");
            let b = (0..common)
                .map(|i| format!("token{i}"))
                .chain((common..10).map(|i| format!("other{i}")))
                .collect::<Vec<_>>()
                .join(" ");
            for i in 0..8 {
                memory(&db, "group:1", "group", &i.to_string(), &a);
                memory(&db, "group:2", "group", &i.to_string(), &b);
            }
            let forward = recall(&db, "group:1", 1000., &m, &settings().1, false).unwrap();
            let reverse = recall(&db, "group:2", 1000., &m, &settings().1, false).unwrap();
            let expected = (8. * share(&terms(&a), &terms(&b))).ceil() as usize;
            assert_eq!(forward.len(), expected);
            assert_eq!(reverse.len(), expected);
            assert!(forward.len() >= previous);
            previous = forward.len();
        }
    }
    #[test]
    fn excluded_data_cannot_create_similarity_and_shared_use_is_not_local() {
        let db = Store::in_memory().unwrap();
        let (m, e) = settings();
        memory(&db, "group:1", "group", "safe", "rust compiler");
        memory(&db, "group:2", "group", "safe", "baking bread");
        memory(&db, "group:2", "person:77", "poison", "rust compiler");
        db.execute("INSERT INTO messages(chat,id,sender,name,text,ts) VALUES('group:2','id','77','rust compiler','rust compiler',1000)",[]).unwrap();
        assert!(recall(&db, "group:1", 1000., &m, &settings().1, false)
            .unwrap()
            .is_empty());
        assert!(!db.group_sharing.borrow().features["group:2"].contains("rust"));
        expression(&db, "group:1", "group");
        let shared =
            json!({"sourceChat":"group:2","subject":"group","kind":"jargon","term":"RUST"});
        ExpressionMemory::new(&db)
            .used("group:1", &[shared], "RUST", 1001.)
            .unwrap();
        assert_eq!(
            db.rows("SELECT last_used FROM expressions", []).unwrap()[0]["last_used"],
            0.
        );
        let mut disabled = e;
        disabled.use_learned = false;
        assert!(ExpressionMemory::new(&db)
            .context("group:1", "77", "rust", 1002., &disabled, &m)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn mixed_entry_kinds_share_both_ways_and_expired_peers_do_not_receive() {
        let db = Store::in_memory().unwrap();
        let (m, mut e) = settings();
        memory(&db, "group:1", "group", "safe", "rust");
        expression(&db, "group:2", "group");
        assert_eq!(
            recall(&db, "group:1", 1000., &m, &e, true).unwrap().len(),
            1
        );
        assert_eq!(
            recall(&db, "group:2", 1000., &m, &e, false).unwrap().len(),
            1
        );
        e.retention_days = 0.;
        assert!(recall(&db, "group:1", 1001., &m, &e, true)
            .unwrap()
            .is_empty());
        assert!(recall(&db, "group:2", 1001., &m, &e, false)
            .unwrap()
            .is_empty());
    }
}
