//! 有证据的表达学习与白名单装饰；不实现未落地的 affect 或学习分诊。
use crate::{
    config::{js_string, truthy, Agent, Emoji, Expression, Memory},
    memory::{
        array, memory_subjects, merge_sources, newest, num, same_source, text, trim, unit,
        valid_text,
    },
    memory::ranking::rank_memories,
    store::{decode, Store},
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
pub fn parse_expressions(
    value: &Value,
    history: &[Value],
    chat: &str,
    sender: &str,
) -> Result<Vec<Value>> {
    ensure!(
        value.is_array() && array(value).len() <= 4,
        "invalid_expressions"
    );
    let subjects = memory_subjects(chat, sender)?;
    let evidence: HashMap<_, _> = history
        .iter()
        .filter(|m| !truthy(&m["self"]) && m["chat"] == chat)
        .map(|m| (text(m, "id"), m))
        .collect();
    array(value)
        .iter()
        .map(|v| {
            ensure!(
                subjects.iter().any(|s| v["subject"] == *s)
                    && matches!(text(v, "kind"), "jargon" | "expression")
                    && ["term", "meaning", "situation", "example"]
                        .iter()
                        .all(|k| valid_text(&v[k], if *k == "term" { 40 } else { 160 }))
                    && unit(&v["confidence"])
                    && v["sourceIds"].is_array()
                    && (1..=6).contains(&array(&v["sourceIds"]).len()),
                "invalid_expressions"
            );
            let mut seen = HashSet::new();
            let mut sources = Vec::new();
            for id in array(&v["sourceIds"]) {
                let m = id.as_str().and_then(|id| evidence.get(id));
                ensure!(m.is_some(), "expression_author_mismatch");
                let m = *m.unwrap();
                // 每个 sourceId 必须归属本 chat；person 还必须逐条归属同一个 QQ-ID。
                ensure!(
                    v["subject"] == "group"
                        || format!("person:{}", js_string(&m["sender"])) == text(v, "subject"),
                    "expression_author_mismatch"
                );
                if seen.insert(id.as_str().unwrap()) {
                    sources.push(m);
                }
            }
            // 以现行 JS 为准：jargon 的 term 每条命中，example 至少一条命中；expression 的 example 每条命中。
            ensure!(
                sources
                    .iter()
                    .any(|m| text(m, "text").contains(text(v, "example")))
                    && sources.iter().all(|m| text(m, "text").contains(text(
                        v,
                        if v["kind"] == "jargon" {
                            "term"
                        } else {
                            "example"
                        }
                    ))),
                "expression_evidence_missing"
            );
            let mut out = v.clone();
            out["sources"] = json!(sources
                .iter()
                .map(|m| json!({"id":m["id"],"sender":m["sender"],"ts":m["ts"]}))
                .collect::<Vec<_>>());
            Ok(out)
        })
        .collect()
}
pub struct ExpressionMemory<'a> {
    store: &'a Store,
}
impl<'a> ExpressionMemory<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }
    pub fn apply(
        &self,
        chat: &str,
        updates: &[Value],
        now: f64,
        settings: &Expression,
    ) -> Result<()> {
        for v in updates {
            let old = self.store.first(
                "SELECT * FROM expressions WHERE chat=? AND subject=? AND kind=? AND term=?",
                params![chat, text(v, "subject"), text(v, "kind"), text(v, "term")],
            )?;
            let previous: Vec<Value> = old
                .as_ref()
                .map(|r| serde_json::from_str(text(r, "sources")))
                .transpose()?
                .unwrap_or_default();
            let sources = array(&v["sources"]);
            // 教学没有人类时间戳，允许主人再次改写，且来源保持为唯一教学标记。
            let teaching = crate::owner_teaching::sources(sources);
            if !teaching
                && old.is_some()
                && newest(sources, f64::NEG_INFINITY) < newest(&previous, f64::NEG_INFINITY)
            {
                continue;
            }
            let fresh = sources
                .iter()
                .any(|s| !previous.iter().any(|p| same_source(p, s)));
            if old.is_some() && !fresh && !teaching {
                continue;
            }
            let revised = old.as_ref().is_some_and(|r| r["meaning"] != v["meaning"]);
            let merged = merge_sources(if revised || teaching { &[] } else { &previous }, sources);
            self.store.execute("INSERT INTO expressions VALUES(?,?,?,?,?,?,?,?,?,?,0) ON CONFLICT(chat,subject,kind,term) DO UPDATE SET meaning=excluded.meaning,situation=excluded.situation,example=excluded.example,confidence=excluded.confidence,sources=excluded.sources,updated=excluded.updated",params![chat,text(v,"subject"),text(v,"kind"),text(v,"term"),text(v,"meaning"),text(v,"situation"),text(v,"example"),num(v,"confidence"),json!(merged).to_string(),now])?;
        }
        self.prune(now, settings)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn context(
        &self,
        chat: &str,
        sender: &str,
        query: &str,
        now: f64,
        settings: &Expression,
        memory_settings: &Memory,
    ) -> Result<Vec<Value>> {
        if !settings.use_learned {
            return Ok(vec![]);
        }
        let mut rows = Vec::new();
        for subject in memory_subjects(chat, sender)? {
            for mut r in self.store.rows("SELECT * FROM expressions WHERE chat=? AND subject=? AND updated>? AND confidence>=? AND (last_used=0 OR last_used<=?)",params![chat,subject,now-settings.retention_days*86400.,settings.min_confidence,now-settings.reuse_seconds])? {
                decode(&mut r,&["sources"])?;
                if (!crate::owner_teaching::sources(array(&r["sources"])) && array(&r["sources"]).len()<2)||(r["subject"]=="group"&&array(&r["sources"]).iter().map(|s|s["sender"].to_string()).collect::<HashSet<_>>().len()<2){continue;}
                r["id"]=json!(json!([r["subject"],r["kind"],r["term"]]).to_string());r["layer"]=r["kind"].clone();r["text"]=json!(format!("{}：{}；适用：{}",text(&r,"term"),text(&r,"meaning"),text(&r,"situation")));r["importance"]=json!(0.5);rows.push(r);
            }
        }
        let mut ranking = memory_settings.clone();
        ranking.min_confidence = settings.min_confidence;
        let mut result = rank_memories(&rows, query, now, &ranking, true);
        result.truncate(settings.max_per_reply as usize);
        Ok(result)
    }
    pub fn used(&self, chat: &str, rows: &[Value], content: &str, now: f64) -> Result<()> {
        for r in rows {
            if content.contains(text(
                r,
                if r["kind"] == "jargon" {
                    "term"
                } else {
                    "example"
                },
            )) {
                self.store.execute("UPDATE expressions SET last_used=? WHERE chat=? AND subject=? AND kind=? AND term=?",params![now,chat,text(r,"subject"),text(r,"kind"),text(r,"term")])?;
            }
        }
        Ok(())
    }
    pub fn prune(&self, now: f64, settings: &Expression) -> Result<()> {
        self.store.execute(
            "DELETE FROM expressions WHERE updated<=?",
            [now - settings.retention_days * 86400.],
        )?;
        for r in self
            .store
            .rows("SELECT DISTINCT chat FROM expressions", [])?
        {
            self.store.execute("DELETE FROM expressions WHERE chat=? AND rowid NOT IN (SELECT rowid FROM expressions WHERE chat=? ORDER BY updated DESC,rowid DESC LIMIT ?)",params![text(&r,"chat"),text(&r,"chat"),settings.max_entries as i64])?;
        }
        Ok(())
    }
    pub fn reset(&self, chat: &str, subject: Option<&str>) -> Result<()> {
        self.store.execute(
            "DELETE FROM expressions WHERE chat=? AND (? IS NULL OR subject=?)",
            params![chat, subject, subject],
        )?;
        Ok(())
    }
}
pub fn personality_context(agent: &Agent, mut random: impl FnMut() -> f64) -> Value {
    let p = &agent.personality;
    let variant = if !p.variants.is_empty() && random() < p.variant_probability {
        p.variants
            .get((random() * p.variants.len() as f64).floor() as usize)
            .cloned()
    } else {
        None
    };
    json!({"identity":agent.persona.text,"behavior":p.behavior,"replyStyle":p.reply_style,"interests":p.interests,"variant":variant})
}
pub fn decoration_choices(
    store: &Store,
    chat: &str,
    now: f64,
    settings: &Emoji,
    mut random: impl FnMut() -> f64,
) -> Result<Value> {
    let last = store.first("SELECT ts FROM decoration_usage WHERE chat=?", [chat])?;
    // P6c 门控新功能：原概率保留为上限；关闭时不查询历史、不多消耗随机数。
    let probability = if settings.learn_frequency && chat.starts_with("group:") {
        crate::humanize::face_probability(store, chat, now)?.min(settings.probability)
    } else {
        settings.probability
    };
    if !settings.enabled
        || last.is_some_and(|r| now - num(&r, "ts") < settings.cooldown_seconds)
        || random() >= probability
    {
        return Ok(json!({"symbols":[],"faceIds":[]}));
    }
    Ok(json!({"symbols":settings.symbols,"faceIds":settings.face_ids}))
}
pub fn decorate(response: &Value, choices: &Value, max_characters: usize) -> Value {
    // 装饰预算按码点，不按 UTF-8 字节或 UTF-16 码元；复合 emoji 可占多个码点。
    let candidate = text(response, "emoji");
    let emoji = if array(&choices["symbols"]).contains(&response["emoji"])
        && candidate.chars().count() + 1 < max_characters
    {
        candidate
    } else {
        ""
    };
    let face = if emoji.is_empty() && array(&choices["faceIds"]).contains(&response["faceId"]) {
        response["faceId"].clone()
    } else {
        Value::Null
    };
    let suffix = if !emoji.is_empty() && !text(response, "text").contains(emoji) {
        format!(" {emoji}")
    } else {
        String::new()
    };
    let output = trim(text(response, "text"))
        .chars()
        .take(max_characters.saturating_sub(suffix.chars().count()))
        .collect::<String>()
        + &suffix;
    json!({"text":output,"faceId":face,"decorated":!emoji.is_empty()||truthy(&face)})
}
