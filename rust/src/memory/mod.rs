//! 三层记忆。连接由 Store 统一拥有，不创建新 schema、不读取真实 data/。
pub mod ranking;
pub mod text;
pub mod memory_unicode;

use crate::{
    config::{js_string, truthy, Memory},
    memory::ranking::rank_memories,
    store::{decode, uuid, Store},
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
pub(crate) fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub(crate) fn num(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(0.)
}
pub(crate) fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}
pub(crate) fn len(s: &str) -> usize {
    s.encode_utf16().count()
}
pub(crate) fn valid_text(v: &Value, max: usize) -> bool {
    v.as_str()
        .is_some_and(|s| !trim(s).is_empty() && len(s) <= max)
}
pub(crate) fn unit(v: &Value) -> bool {
    v.as_f64().is_some_and(|n| (0. ..=1.).contains(&n))
}
pub fn memory_subjects(chat: &str, sender: &str) -> Result<Vec<String>> {
    let positive = |s: &str| {
        s.starts_with(|c: char| ('1'..='9').contains(&c)) && s.bytes().all(|b| b.is_ascii_digit())
    };
    // Telegram 超级群 id 为负数；群 chat_id 允许前导 `-`，用户/私聊 id 仍为正数。
    let group = |s: &str| {
        s.strip_prefix('-')
            .map_or_else(|| positive(s), positive)
    };
    let (kind, chat_id) = chat.split_once(':').unwrap_or(("", ""));
    ensure!(
        match kind {
            "group" => group(chat_id) && positive(sender),
            "private" => positive(chat_id) && chat_id == sender,
            _ => false,
        },
        "invalid_memory_scope"
    );
    Ok(if kind == "group" {
        vec!["group".into(), format!("person:{sender}")]
    } else {
        vec![format!("person:{sender}")]
    })
}
pub fn parse_memory_updates(
    value: &Value,
    history: &[Value],
    chat: &str,
    sender: &str,
    settings: &Memory,
) -> Result<Vec<Value>> {
    let allowed = memory_subjects(chat, sender)?;
    let humans: HashMap<_, _> = history
        .iter()
        .filter(|m| !truthy(&m["self"]) && m["chat"] == chat)
        .map(|m| (text(m, "id"), m))
        .collect();
    ensure!(
        value.is_array() && array(value).len() <= 4,
        "invalid_memory_updates"
    );
    // Validate the whole batch before accepting even a compact skip record.
    ensure!(
        array(value).iter().all(|v| v.get("verdict").is_none()
            || matches!(text(v, "verdict"), "learn" | "partial" | "skip")),
        "invalid_memory_verdict"
    );
    let mut seen = HashSet::new();
    array(value).iter().map(|v| {
        if v["verdict"] == "skip" {
            ensure!(valid_text(&v["reason"], 500), "invalid_memory_reason");
            return Ok(json!({"verdict":"skip", "reason":trim(text(v,"reason"))}));
        }
        ensure!(allowed.iter().any(|s|v["subject"]==*s)&&matches!(text(v,"layer"),"long_term"|"traits")&&matches!(text(v,"operation"),"upsert"|"forget")&&valid_text(&v["key"],64)&&v["sourceIds"].is_array()&&(1..=6).contains(&array(&v["sourceIds"]).len())&&array(&v["sourceIds"]).iter().all(|id|id.as_str().is_some_and(|id|humans.contains_key(id))),"invalid_memory_updates");
        let mut ids=HashSet::new();
        let sources:Vec<_>=array(&v["sourceIds"]).iter().filter(|id|ids.insert(id.as_str().unwrap())).map(|id|{let m=humans[id.as_str().unwrap()];json!({"id":id,"sender":m["sender"],"ts":m["ts"]})}).collect();
        // 来源必须是本 chat 的人类消息；QQ-ID 归属不依赖昵称，群结论需要多人。
        ensure!(if v["subject"]=="group" {sources.iter().map(|s|s["sender"].to_string()).collect::<HashSet<_>>().len()>=2} else {sources.iter().all(|s|format!("person:{}",js_string(&s["sender"]))==text(v,"subject"))},"memory_author_mismatch");
        let key=trim(text(v,"key"));
        ensure!(seen.insert(format!("{}/{}/{}",text(v,"subject"),text(v,"layer"),key)),"duplicate_memory_update");
        let max=if v["layer"]=="long_term" {500f64.min(settings.long_chars)}else{300f64.min(settings.trait_chars)};
        ensure!(v["operation"]!="upsert" || (valid_text(&v["text"],max as usize)&&unit(&v["importance"])),"invalid_memory_updates");
        let keywords=if v["keywords"].is_null(){json!([])}else{v["keywords"].clone()};
        ensure!(keywords.is_array()&&array(&keywords).len()<=8&&array(&keywords).iter().all(|k|valid_text(k,32)),"invalid_memory_keywords");
        let confidence=if v["confidence"].is_null(){json!(0.6)}else{v["confidence"].clone()};
        ensure!(unit(&confidence),"invalid_memory_confidence");
        let mut seen=HashSet::new();let keywords:Vec<_>=array(&keywords).iter().map(|k|trim(k.as_str().unwrap())).filter(|k|seen.insert(*k)).collect();
        let mut parsed = json!({"keywords":keywords,"confidence":confidence,"subject":v["subject"],"layer":v["layer"],"key":key,"operation":v["operation"],"text":if v["operation"]=="upsert"{trim(text(v,"text"))}else{""},"importance":if v["operation"]=="upsert"{num(v,"importance")}else{0.},"sources":sources});
        if let Some(verdict) = v.get("verdict") {
            parsed["verdict"] = verdict.clone();
        }
        if v["verdict"] == "partial" {
            parsed["confidence"] = json!(num(&parsed, "confidence").min(0.5));
        }
        Ok(parsed)
    }).collect()
}
/// Only cited human messages are exposed to the reviewer, never the surrounding history.
pub fn learning_review_input(updates: &[Value], history: &[Value], existing: &[Value]) -> Value {
    let candidates: Vec<_> = updates
        .iter()
        .enumerate()
        .filter(|(_, v)| v["verdict"] != "skip")
        .map(|(index, v)| json!({"index":index,"candidate":v}))
        .collect();
    let sources: Vec<_> = history
        .iter()
        .filter(|m| {
            !truthy(&m["self"])
                && updates
                    .iter()
                    .any(|v| array(&v["sources"]).iter().any(|s| same_source(s, m)))
        })
        .map(|m| json!({"id":m["id"],"sender":m["sender"],"text":m["text"],"ts":m["ts"]}))
        .collect();
    json!({"candidates":candidates,"sources":sources,"existing":existing})
}

/// A malformed or incomplete review rejects the entire learning batch before any writes.
pub fn apply_learning_review(
    updates: &[Value],
    response: &Value,
    settings: &Memory,
) -> Result<Vec<Value>> {
    let expected = updates.iter().filter(|v| v["verdict"] != "skip").count();
    ensure!(
        response["reviews"].is_array() && array(&response["reviews"]).len() == expected,
        "invalid_learning_review"
    );
    let mut result = updates.to_vec();
    let mut seen = HashSet::new();
    for r in array(&response["reviews"]) {
        let index = r["index"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|i| *i < updates.len())
            .ok_or_else(|| anyhow::anyhow!("invalid_learning_review"))?;
        ensure!(
            seen.insert(index)
                && updates[index]["verdict"] != "skip"
                && valid_text(&r["reason"], 500),
            "invalid_learning_review"
        );
        match text(r, "action") {
            "keep" => {}
            "drop" => {
                result[index]["review"] = r.clone();
            }
            "rewrite" => {
                let max = if updates[index]["layer"] == "long_term" {
                    settings.long_chars.min(500.)
                } else {
                    settings.trait_chars.min(300.)
                };
                ensure!(
                    updates[index]["operation"] == "upsert" && valid_text(&r["text"], max as usize),
                    "invalid_learning_review"
                );
                result[index]["review"] = r.clone();
                result[index]["text"] = json!(trim(text(r, "text")));
            }
            _ => anyhow::bail!("invalid_learning_review"),
        }
    }
    Ok(result)
}

// Persist triage metadata as one reserved object in the existing keywords JSON array.
// Model keywords are strictly strings; legacy rows/schema and ordinary learn output stay unchanged.
fn triage_state(keywords: &[Value]) -> Option<&Value> {
    keywords.iter().find(|k| k["pending"] == true)
}

pub(crate) fn merge_sources(previous: &[Value], sources: &[Value]) -> Vec<Value> {
    let mut merged = previous.to_vec();
    for s in sources {
        if let Some(i) = merged
            .iter()
            .position(|p| source_identity(p) == source_identity(s))
        {
            merged[i] = s.clone();
        } else {
            merged.push(s.clone());
        }
    }
    merged.sort_by(|a, b| num(b, "ts").total_cmp(&num(a, "ts")));
    merged.truncate(12);
    merged
}
pub(crate) fn source_identity(s: &Value) -> String {
    format!("{}:{}", js_string(&s["sender"]), js_string(&s["id"]))
}
pub(crate) fn same_source(a: &Value, b: &Value) -> bool {
    a["sender"] == b["sender"] && a["id"] == b["id"]
}
pub(crate) fn newest(sources: &[Value], floor: f64) -> f64 {
    sources.iter().map(|s| num(s, "ts")).fold(floor, f64::max)
}
#[derive(Default)]
pub(crate) struct Pending {
    added: usize,
    since: f64,
    people: HashSet<String>,
}
/// 计数由 Store 共享；启动及配置改变调用 configure，运行时每秒可调用 maintain。
/// 普通 capture 只做两次定点 put 和 O(1) 计数；每 chat 累积 shortLimit 次追加后批清理。
pub struct LayeredMemory<'a> {
    store: &'a Store,
}
impl<'a> LayeredMemory<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &self,
        chat: &str,
        subject: &str,
        layer: &str,
        slot: &str,
        content: &str,
        sources: &[Value],
        importance: f64,
        now: f64,
        expires: f64,
        metadata: &Value,
        revision_limit: usize,
    ) -> Result<()> {
        let old = self.store.first(
            "SELECT * FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND slot=?",
            params![chat, subject, layer, slot],
        )?;
        let previous: Vec<Value> = old
            .as_ref()
            .map(|r| serde_json::from_str(text(r, "sources")))
            .transpose()?
            .unwrap_or_default();
        if old.is_some()
            && layer != "short_term"
            && !sources.is_empty()
            && newest(sources, f64::NEG_INFINITY) < newest(&previous, 0.)
        {
            return Ok(());
        }
        let fresh = sources.iter().any(|s| {
            !previous
                .iter()
                .any(|p| source_identity(p) == source_identity(s))
        });
        if let Some(old) = &old {
            if !fresh && old["text"] == content {
                return Ok(());
            }
            // 文本真正改变时先归档旧版本；相同证据的改写不延长 updated/expires。
            if layer != "short_term" && old["text"] != content {
                self.store.execute(
                    "INSERT OR REPLACE INTO memory_revisions VALUES(?,?,?,?,?,?)",
                    params![
                        text(old, "id"),
                        old["revision"].as_i64(),
                        text(old, "text"),
                        text(old, "sources"),
                        num(old, "updated"),
                        now
                    ],
                )?;
                self.store.execute("DELETE FROM memory_revisions WHERE memory_id=? AND revision NOT IN (SELECT revision FROM memory_revisions WHERE memory_id=? ORDER BY revision DESC LIMIT ?)",params![text(old,"id"),text(old,"id"),revision_limit as i64])?;
            }
        }
        let updated = old
            .as_ref()
            .filter(|_| !fresh)
            .map(|r| num(r, "updated"))
            .unwrap_or(now);
        let expiry = old
            .as_ref()
            .filter(|_| !fresh)
            .map(|r| num(r, "expires"))
            .unwrap_or(expires);
        self.store.execute("INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,importance,created,updated,expires,revision,keywords,confidence) VALUES(?,?,?,?,?,?,?,?,?,?,?,1,?,?) ON CONFLICT(chat,subject,layer,slot) DO UPDATE SET text=excluded.text,sources=excluded.sources,importance=excluded.importance,updated=excluded.updated,expires=excluded.expires,revision=revision+1,keywords=excluded.keywords,confidence=excluded.confidence",params![uuid(),chat,subject,layer,slot,content,json!(merge_sources(&previous,sources)).to_string(),importance,now,updated,expiry,metadata.get("keywords").filter(|v|truthy(v)).cloned().unwrap_or(json!([])).to_string(),metadata["confidence"].as_f64().unwrap_or(0.6)])?;
        Ok(())
    }
    pub fn capture(&self, message: &Value, now: f64, settings: &Memory) -> Result<()> {
        if truthy(&message["self"]) {
            return Ok(());
        }
        let chat = text(message, "chat");
        let subjects = memory_subjects(chat, text(message, "sender"))?;
        // 首次接入已有库时只初始化一次人数索引；正常启动 configure 已完成此步骤。
        if !self.store.memory_pending.borrow().contains_key(chat) {
            self.refresh_people(chat, now, true)?;
        }
        let content = format!(
            "{} ({}): {}",
            text(message, "name"),
            text(message, "sender"),
            text(message, "text")
        );
        // JS slice 按 UTF-16；截断代理对时 Rust 用替代字符保存，见 MEMORY.md。
        let content = String::from_utf16_lossy(
            &content
                .encode_utf16()
                .take(settings.short_chars as usize)
                .collect::<Vec<_>>(),
        );
        for subject in subjects {
            self.put(
                chat,
                &subject,
                "short_term",
                text(message, "id"),
                &content,
                &[json!({"id":message["id"],"sender":message["sender"],"ts":message["ts"]})],
                0.5,
                now,
                now + settings.short_hours * 3600.,
                &json!({"confidence":1}),
                3,
            )?;
        }
        let due = {
            let mut pending = self.store.memory_pending.borrow_mut();
            let p = pending.get_mut(chat).expect("initialized chat");
            p.added += 1;
            p.people
                .insert(format!("person:{}", text(message, "sender")));
            p.added >= settings.short_limit.max(1.) as usize
                || p.people.len() > settings.max_people as usize
                || now - p.since >= 3600.
        };
        if due {
            self.enforce(chat, now, settings, None)?;
        }
        Ok(())
    }
    pub fn apply(&self, chat: &str, updates: &[Value], now: f64, settings: &Memory) -> Result<()> {
        ensure!(
            updates.iter().all(|v| v.get("verdict").is_none()
                || matches!(text(v, "verdict"), "learn" | "partial" | "skip")),
            "invalid_memory_verdict"
        );
        for original in updates {
            if original["verdict"] == "skip" {
                self.store.decision(
                    chat,
                    "skipped",
                    0.,
                    &json!({"reason":original["reason"]}),
                    now,
                )?;
                continue;
            }
            if let Some(review) = original.get("review") {
                self.store.decision(
                    chat,
                    text(review, "action"),
                    0.,
                    &json!({"reason":review["reason"],"subject":original["subject"],
                        "layer":original["layer"],"key":original["key"],"text":original["text"]}),
                    now,
                )?;
                if review["action"] == "drop" {
                    continue;
                }
            }
            let mut v = original.clone();
            if v["operation"] == "upsert" {
                let old = self.store.first(
                    "SELECT * FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND slot=? AND expires>?",
                    params![chat,text(&v,"subject"),text(&v,"layer"),text(&v,"key"),now])?;
                let old_keywords: Vec<Value> = old
                    .as_ref()
                    .map(|r| serde_json::from_str(text(r, "keywords")))
                    .transpose()?
                    .unwrap_or_default();
                let prior = triage_state(&old_keywords);
                if prior.is_some() || v["verdict"] == "partial" {
                    let mut state = prior.cloned().unwrap_or_else(|| json!({"pending":true,
                        "since":newest(array(&v["sources"]),0.),"baseline":array(&v["sources"]).iter().map(source_identity).collect::<Vec<_>>(),"evidence":[]}));
                    // Only distinct source identities newer than the initial evidence count.
                    // Retain identities independently of the rolling 12-source display window.
                    let mut evidence: HashSet<String> = array(&state["evidence"])
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    for source in array(&v["sources"]) {
                        if num(source, "ts") > num(&state, "since")
                            && !array(&state["baseline"])
                                .iter()
                                .any(|id| id.as_str() == Some(source_identity(source).as_str()))
                        {
                            evidence.insert(source_identity(source));
                        }
                    }
                    let promoted = evidence.len() >= settings.partial_evidence.max(1);
                    let mut keywords = array(&v["keywords"]).to_vec();
                    if promoted {
                        v["confidence"] = json!(num(&v, "confidence").max(0.6));
                    } else {
                        let mut evidence: Vec<_> = evidence.into_iter().collect();
                        evidence.sort();
                        state["evidence"] = json!(evidence);
                        keywords.push(state);
                        v["confidence"] = json!(num(&v, "confidence").min(0.5));
                    }
                    v["keywords"] = json!(keywords);
                }
            }
            let v = &v;
            if v["operation"] == "forget" {
                self.store.execute(
                    "DELETE FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND slot=?",
                    params![chat, text(v, "subject"), text(v, "layer"), text(v, "key")],
                )?;
            } else {
                self.put(
                    chat,
                    text(v, "subject"),
                    text(v, "layer"),
                    text(v, "key"),
                    text(v, "text"),
                    array(&v["sources"]),
                    num(v, "importance"),
                    now,
                    now + if v["layer"] == "long_term" {
                        settings.long_days
                    } else {
                        settings.trait_days
                    } * 86400.,
                    v,
                    settings.revision_limit as usize,
                )?;
            }
        }
        let subjects: Vec<_> = updates
            .iter()
            .map(|v| text(v, "subject").to_owned())
            .collect();
        self.enforce(chat, now, settings, Some(&subjects))
    }
    pub fn rows(&self, chat: &str, subject: &str, layer: &str, now: f64) -> Result<Vec<Value>> {
        let mut rows=self.store.rows("SELECT * FROM memory_layers WHERE chat=? AND subject=? AND layer=? AND expires>? ORDER BY importance DESC,updated DESC,rowid DESC",params![chat,subject,layer,now])?;
        for r in &mut rows {
            decode(r, &["sources", "keywords"])?;
            if triage_state(array(&r["keywords"])).is_some() {
                r["pending"] = json!(true);
                r["verdict"] = json!("partial");
                r["keywords"] = json!(array(&r["keywords"])
                    .iter()
                    .filter(|k| k.is_string())
                    .collect::<Vec<_>>());
            }
        }
        Ok(rows)
    }
    pub fn short(
        &self,
        chat: &str,
        sender: &str,
        now: f64,
        settings: &Memory,
        excluded: &[String],
    ) -> Result<Vec<Value>> {
        let mut seen: HashSet<_> = excluded.iter().cloned().collect();
        let mut output = Vec::new();
        // person 优先去重，且所有 SQL 同时限定 chat/subject，不能跨群或私聊。
        for subject in memory_subjects(chat, sender)?.iter().rev() {
            for r in self
                .rows(chat, subject, "short_term", now)?
                .into_iter()
                .filter(|r| num(r, "updated") + settings.short_hours * 3600. > now)
                .take(settings.short_limit as usize)
            {
                if seen.insert(text(&r, "slot").into()) {
                    output.push(r);
                }
            }
        }
        Ok(output)
    }
    pub fn context(
        &self,
        chat: &str,
        sender: &str,
        now: f64,
        settings: &Memory,
        query: &str,
    ) -> Result<Vec<Value>> {
        let subjects = memory_subjects(chat, sender)?;
        let mut out: Vec<_> = subjects
            .iter()
            .map(|s| json!({"subject":s,"long_term":[],"traits":[]}))
            .collect();
        let mut rows = Vec::new();
        for subject in &subjects {
            for layer in ["long_term", "traits"] {
                rows.extend(
                    self.rows(chat, subject, layer, now)?
                        .into_iter()
                        .filter(|r| {
                            num(r, "updated")
                                + if layer == "long_term" {
                                    settings.long_days
                                } else {
                                    settings.trait_days
                                } * 86400.
                                > now
                        }),
                );
            }
        }
        let mut used = 0;
        for r in rank_memories(&rows, query, now, settings, false) {
            let target = out
                .iter_mut()
                .find(|s| s["subject"] == r["subject"])
                .unwrap()[text(&r, "layer")]
            .as_array_mut()
            .unwrap();
            let size = len(text(&r, "text"));
            let limit = if r["layer"] == "long_term" {
                settings.long_chars
            } else {
                settings.trait_chars
            };
            if (used + size) as f64 > settings.recall_chars
                || target.len() >= 24
                || (target.iter().map(|r| len(text(r, "text"))).sum::<usize>() + size) as f64
                    > limit
            {
                continue;
            }
            target.push(r);
            used += size;
        }
        Ok(out)
    }
    pub fn bounded(rows: &[Value], chars: usize) -> Vec<Value> {
        let mut used = 0;
        let mut result = Vec::new();
        for r in rows {
            let n = len(text(r, "text"));
            if result.len() < 24 && used + n <= chars {
                used += n;
                result.push(r.clone());
            }
        }
        result
    }
    pub fn enforce(
        &self,
        chat: &str,
        now: f64,
        settings: &Memory,
        subjects: Option<&[String]>,
    ) -> Result<()> {
        self.store.execute("UPDATE memory_layers SET expires=min(expires,updated+CASE layer WHEN 'short_term' THEN ? WHEN 'long_term' THEN ? ELSE ? END) WHERE chat=?",params![settings.short_hours*3600.,settings.long_days*86400.,settings.trait_days*86400.,chat])?;
        self.store.execute(
            "DELETE FROM memory_layers WHERE chat=? AND expires<=?",
            params![chat, now],
        )?;
        // 批量 SQL 替代每条记忆一次 revision DELETE，窗口排名保持原先语义。
        self.store.execute("DELETE FROM memory_revisions WHERE (memory_id,revision) IN (SELECT memory_id,revision FROM (SELECT r.memory_id,r.revision,row_number() OVER (PARTITION BY r.memory_id ORDER BY r.revision DESC) AS n FROM memory_revisions r JOIN memory_layers m ON m.id=r.memory_id WHERE m.chat=?) WHERE n>?)",params![chat,settings.revision_limit])?;
        for row in self.store.rows(
            "SELECT DISTINCT subject FROM memory_layers WHERE chat=?",
            [chat],
        )? {
            let subject = text(&row, "subject");
            if subjects.is_some_and(|s| !s.iter().any(|s| s == subject)) {
                continue;
            }
            for layer in ["short_term", "long_term", "traits"] {
                let rows = self.rows(chat, subject, layer, now)?;
                let keep = if layer == "short_term" {
                    rows.iter()
                        .take(settings.short_limit as usize)
                        .cloned()
                        .collect()
                } else {
                    Self::bounded(
                        &rows,
                        if layer == "long_term" {
                            settings.long_chars
                        } else {
                            settings.trait_chars
                        } as usize,
                    )
                };
                let ids: HashSet<_> = keep.iter().map(|r| text(r, "id")).collect();
                for r in &rows {
                    if !ids.contains(text(r, "id")) {
                        self.store
                            .execute("DELETE FROM memory_layers WHERE id=?", [text(r, "id")])?;
                    }
                }
            }
        }
        self.store.execute("DELETE FROM memory_layers WHERE chat=? AND subject IN (SELECT subject FROM memory_layers WHERE chat=? AND subject<>'group' GROUP BY subject ORDER BY max(updated) DESC,subject LIMIT -1 OFFSET ?)",params![chat,chat,settings.max_people as i64])?;
        self.refresh_people(chat, now, subjects.is_none())?;
        Ok(())
    }
    fn refresh_people(&self, chat: &str, now: f64, cleared: bool) -> Result<()> {
        let people = self
            .store
            .rows(
                "SELECT DISTINCT subject FROM memory_layers WHERE chat=? AND subject<>'group'",
                [chat],
            )?
            .iter()
            .map(|r| text(r, "subject").to_owned())
            .collect();
        let mut pending = self.store.memory_pending.borrow_mut();
        let p = pending.entry(chat.into()).or_insert(Pending {
            added: 0,
            since: now,
            people: HashSet::new(),
        });
        p.people = people;
        if cleared {
            p.added = 0;
            p.since = now;
        }
        Ok(())
    }
    pub fn configure(&self, now: f64, settings: &Memory) -> Result<()> {
        for r in self
            .store
            .rows("SELECT DISTINCT chat FROM memory_layers", [])?
        {
            self.enforce(text(&r, "chat"), now, settings, None)?;
        }
        self.store.memory_last_maintenance.set(Some(now));
        Ok(())
    }
    pub fn maintain(&self, now: f64, settings: &Memory) -> Result<()> {
        // 即使没有新消息，也清理冷 chat 的过期行；与 configure 共用全量实现。
        if self
            .store
            .memory_last_maintenance
            .get()
            .is_none_or(|last| now - last >= 3600.)
        {
            self.configure(now, settings)?;
        }
        Ok(())
    }
    pub fn reset(&self, chat: &str, subject: Option<&str>) -> Result<()> {
        self.store.execute(
            "DELETE FROM memory_layers WHERE chat=? AND (? IS NULL OR subject=?)",
            params![chat, subject, subject],
        )?;
        self.refresh_people(chat, 0., false)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::memory_subjects;

    #[test]
    fn memory_scope_accepts_qq_groups_telegram_supergroups_and_private() {
        assert_eq!(
            memory_subjects("group:123", "456").unwrap(),
            vec!["group".to_string(), "person:456".to_string()]
        );
        // Telegram 超级群 id 为负数。
        assert_eq!(
            memory_subjects("group:-1001234567890", "123456789").unwrap(),
            vec!["group".to_string(), "person:123456789".to_string()]
        );
        assert_eq!(
            memory_subjects("private:123456789", "123456789").unwrap(),
            vec!["person:123456789".to_string()]
        );
    }

    #[test]
    fn memory_scope_rejects_invalid_negative_and_mismatched_ids() {
        for (chat, sender) in [
            ("group:-0", "1"),
            ("group:--1", "1"),
            ("group:-100abc", "1"),
            ("group:0123", "1"),
            ("private:-123456789", "-123456789"),
            ("private:1", "2"),
            ("channel:1", "1"),
            ("group:-1001234567890", "-1"),
        ] {
            assert!(
                memory_subjects(chat, sender).is_err(),
                "{chat} / {sender} must be rejected"
            );
        }
    }
}
