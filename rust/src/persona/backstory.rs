//! 永久、仅追加的虚构自身过往；与真实记忆分开存储和标注。
use crate::store::Store;
use anyhow::{ensure, Result};
use rusqlite::params;
use serde_json::{json, Value};

pub use crate::prompts::BACKSTORY_RULE as RULE;

pub fn init(store: &Store) -> Result<()> {
    store.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS persona_backstory(id TEXT PRIMARY KEY NOT NULL, chat TEXT NOT NULL, text TEXT NOT NULL, created REAL NOT NULL);
         CREATE TABLE IF NOT EXISTS persona_backstory_detail(id TEXT PRIMARY KEY NOT NULL, backstory_id TEXT NOT NULL REFERENCES persona_backstory(id), text TEXT NOT NULL, created REAL NOT NULL);
         CREATE INDEX IF NOT EXISTS persona_backstory_chat ON persona_backstory(chat,created);
         CREATE INDEX IF NOT EXISTS persona_backstory_parent ON persona_backstory_detail(backstory_id,created);"
    )?;
    for table in ["persona_backstory", "persona_backstory_detail"] {
        for operation in ["UPDATE", "DELETE"] {
            store.execute(&format!("CREATE TRIGGER IF NOT EXISTS {table}_{operation} BEFORE {operation} ON {table} BEGIN SELECT RAISE(ABORT,'immutable_backstory'); END"), [])?;
        }
        // REPLACE 的隐式删除可能不触发 DELETE trigger，单独拦截重复 id。
        store.execute(&format!("CREATE TRIGGER IF NOT EXISTS {table}_replace BEFORE INSERT ON {table} WHEN EXISTS(SELECT 1 FROM {table} WHERE id=NEW.id) BEGIN SELECT RAISE(ABORT,'immutable_backstory'); END"), [])?;
    }
    store.execute("CREATE TRIGGER IF NOT EXISTS persona_backstory_parent_check BEFORE INSERT ON persona_backstory_detail WHEN NOT EXISTS(SELECT 1 FROM persona_backstory WHERE id=NEW.backstory_id AND created<=NEW.created) BEGIN SELECT RAISE(ABORT,'invalid_backstory_parent'); END", [])?;
    Ok(())
}
fn validate(text: &str, now: f64) -> Result<()> {
    ensure!(
        now.is_finite() && !text.trim().is_empty() && text.chars().count() <= 2000,
        "invalid_backstory"
    );
    Ok(())
}
pub fn create(store: &Store, chat: &str, text: &str, now: f64) -> Result<String> {
    validate(text, now)?;
    ensure!(
        chat.strip_prefix("group:").is_some_and(|id| !id.is_empty()),
        "invalid_backstory_chat"
    );
    init(store)?;
    let id = crate::store::uuid();
    store.execute(
        "INSERT INTO persona_backstory(id,chat,text,created) VALUES(?,?,?,?)",
        params![id, chat, text, now],
    )?;
    Ok(id)
}
pub fn add_detail(store: &Store, backstory_id: &str, text: &str, now: f64) -> Result<String> {
    validate(text, now)?;
    init(store)?;
    let id = crate::store::uuid();
    store.execute(
        "INSERT INTO persona_backstory_detail(id,backstory_id,text,created) VALUES(?,?,?,?)",
        params![id, backstory_id, text, now],
    )?;
    Ok(id)
}
pub fn recall(store: &Store, chat: &str, now: f64, limit: usize) -> Result<Vec<Value>> {
    ensure!(now.is_finite(), "invalid_backstory_time");
    init(store)?;
    let mut rows = store.rows("SELECT id,chat,text,created FROM persona_backstory WHERE chat=? AND created<=? ORDER BY created DESC,id LIMIT ?", params![chat,now,i64::try_from(limit)?])?;
    for row in &mut rows {
        row["details"] = json!(store.rows("SELECT id,text,created FROM persona_backstory_detail WHERE backstory_id=? AND created<=? ORDER BY created,id", params![row["id"].as_str(),now])?);
    }
    Ok(rows)
}

// 精确匹配完整请求，只允许抽象、独处的思考情景；不接收任意模型输出或拼接用户文本。
fn candidate(request: &str) -> Option<&'static str> {
    let request = request.trim();
    let request = if let Some(rest) = request.strip_prefix("[@") {
        match rest.split_once(']') {
            Some((id, text)) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
                text.trim()
            }
            _ => return None,
        }
    } else {
        request
    };
    let request = request
        .trim()
        .trim_end_matches(['。', '？', '?', '！', '!']);
    match request {
        "请基于经历评价反复修改" | "基于经历评价反复修改" | "你以前是否反复修改过表达" => Some("虚构的自身过往（无法查验）：我曾在独自梳理表达时反复调整措辞，后来把注意力放回要表达的意思，才觉得清楚比修饰更重要。"),
        "请基于经历评价耐心" | "基于经历评价耐心" | "你以前是否有过耐心思考的经历" => Some("虚构的自身过往（无法查验）：我曾在独自梳理一个抽象问题时急于下结论，停下来逐步整理思路后，才体会到耐心的价值。"),
        _ => None,
    }
}

// 与精确请求白名单配套的主题词；命中一个即保守视为相关，不依赖学习/召回开关。
fn topic(text: &str) -> (&'static [&'static str], &'static str) {
    if text.contains("调整措辞") {
        (&["修改", "措辞", "表达", "修饰", "改写", "润色"], "虚构情景的追加细节：当时我先保留要表达的核心意思，再逐句删去重复的修饰。")
    } else {
        (&["耐心", "思考", "思路", "结论", "急躁", "沉住气"], "虚构情景的追加细节：当时我把尚未想清楚的部分逐项列出，再按顺序慢慢梳理。")
    }
}

/// 在引擎已确认本轮有效后调用；真实记忆优先，与 learning/memoryRecall 开关无关。
pub fn prepare(store: &Store, chat: &str, request: &str, now: f64) -> Result<Vec<Value>> {
    if !chat.starts_with("group:") {
        return Ok(vec![]);
    }
    init(store)?;
    let tx = store.immediate()?;
    if let Some(text) = candidate(request) {
        let (keywords, detail) = topic(text);
        // 有效的持久化记忆、笔记和人类历史原文都是证据；自身发言和经历询问不是经历证据。
        let evidence = store.rows(
            "SELECT text FROM memory_layers WHERE chat=?1 AND (expires IS NULL OR expires>?2) AND (created IS NULL OR created<=?2)
             UNION ALL SELECT text FROM learned_memories WHERE chat=?1 AND (expires IS NULL OR expires>?2) AND (created IS NULL OR created<=?2)
             UNION ALL SELECT text FROM notes WHERE chat=?1 AND (created IS NULL OR created<=?2)
             UNION ALL SELECT text FROM messages WHERE chat=?1 AND self=0 AND (ts IS NULL OR ts<=?2)",
            params![chat, now],
        )?;
        let relevant = evidence.iter().filter_map(|row| row["text"].as_str()).any(|value| {
            candidate(value).is_none() && keywords.iter().any(|key| value.contains(key))
        });
        if !relevant {
            let prior = store.rows(
                "SELECT id FROM persona_backstory WHERE chat=? AND text=? AND created<=? ORDER BY created,id LIMIT 1",
                params![chat, text, now],
            )?;
            if let Some(row) = prior.first() {
                let id = row["id"].as_str().expect("backstory id");
                // 仅再次明确询问同一主题时追加一次；不在普通聊天或每次召回时制造细节。
                if store.rows("SELECT id FROM persona_backstory_detail WHERE backstory_id=? AND text=? LIMIT 1", params![id, detail])?.is_empty() {
                    add_detail(store, id, detail, now)?;
                }
            } else {
                create(store, chat, text, now)?;
            }
        }
    }
    let rows = recall(store, chat, now, 8)?;
    tx.commit()?;
    Ok(rows)
}
