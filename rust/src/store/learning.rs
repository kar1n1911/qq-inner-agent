//! P1b 延后的事务接口；JSON 列一律返回解析结构，写回仍是兼容 JS 的 TEXT。
use super::*;
use crate::{
    config::{Expression, Learning, Memory},
    expression::ExpressionMemory,
    memory::{array, len, text, LayeredMemory},
    ranking::rank_memories,
};
use rusqlite::params;
#[derive(Default)]
pub struct ScopedOptions {
    pub enabled: Option<bool>,
    pub exclude_ids: Vec<String>,
    pub limit: Option<usize>,
}
pub struct LayeredUpdate<'a> {
    pub updates: &'a [Value],
    pub settings: &'a Memory,
    pub expressions: Option<&'a [Value]>,
    pub expression_settings: &'a Expression,
}
// SQLite 回滚不会回滚 Rust 计数；失败时失效缓存，下一次入站重新读取人数。
struct MemoryIndexGuard<'a> {
    store: &'a Store,
    chat: &'a str,
    committed: bool,
}
impl Drop for MemoryIndexGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.store.memory_pending.borrow_mut().remove(self.chat);
        }
    }
}
impl Store {
    #[allow(clippy::too_many_arguments)]
    pub fn retrieve_scoped(
        &self,
        chat: &str,
        sender: &str,
        query: &str,
        now: f64,
        settings: &Memory,
        options: &ScopedOptions,
    ) -> Result<Vec<Value>> {
        let scope = if chat.starts_with("group:") {
            String::from("group")
        } else {
            format!("person:{sender}")
        };
        let mut rows=self.rows("SELECT id,text,created AS updated FROM notes WHERE chat=? ORDER BY created DESC LIMIT 50",[chat])?;
        for r in &mut rows {
            r["subject"] = json!(scope);
            r["layer"] = json!("owner_note");
            r["sources"] = json!([]);
        }
        if options.enabled != Some(false) {
            rows.extend(LayeredMemory::new(self).short(
                chat,
                sender,
                now,
                settings,
                &options.exclude_ids,
            )?);
        }
        let mut chars = 0;
        let mut result: Vec<_> = rank_memories(&rows, query, now, settings, true)
            .into_iter()
            .filter(|r| {
                let n = len(text(r, "text"));
                if (chars + n) as f64 > settings.recall_chars {
                    false
                } else {
                    chars += n;
                    true
                }
            })
            .collect();
        result.truncate(options.limit.unwrap_or(6));
        Ok(result)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn learn(
        &self,
        chat: &str,
        update: &Value,
        now: f64,
        last_id: &str,
        settings: &Learning,
        epoch: i64,
        layered: Option<LayeredUpdate<'_>>,
    ) -> Result<bool> {
        // IMMEDIATE 覆盖 epoch 检查、revision 归档及全部写入；异常由 RAII 回滚。
        let mut index_guard = MemoryIndexGuard {
            store: self,
            chat,
            committed: false,
        };
        let tx = self.immediate()?;
        let old = self.learning_state(chat)?;
        if old["epoch"].as_i64() != Some(epoch) {
            return Ok(false);
        }
        if let Some(layered) = layered {
            LayeredMemory::new(self).apply(chat, layered.updates, now, layered.settings)?;
            if let Some(expressions) = layered.expressions {
                ExpressionMemory::new(self).apply(
                    chat,
                    expressions,
                    now,
                    layered.expression_settings,
                )?;
            }
        }
        let style = update.get("style").filter(|v| crate::config::truthy(v));
        let content = style
            .and_then(|v| v["text"].as_str())
            .unwrap_or(text(&old, "style"));
        let sources = style
            .map(|s| s["sources"].clone())
            .unwrap_or_else(|| old["sources"].clone());
        self.execute("INSERT INTO chat_learning VALUES(?,?,?,?,?,?) ON CONFLICT(chat) DO UPDATE SET style=excluded.style,sources=excluded.sources,updated=excluded.updated,last_id=excluded.last_id",params![chat,content,sources.to_string(),now,last_id,epoch])?;
        for id in array(&update["forgetIds"]) {
            self.execute(
                "DELETE FROM learned_memories WHERE chat=? AND id=?",
                params![chat, id.as_str()],
            )?;
        }
        for m in array(&update["memories"]) {
            self.execute(
                "DELETE FROM learned_memories WHERE chat=? AND text=?",
                params![chat, text(m, "text")],
            )?;
            self.execute(
                "INSERT INTO learned_memories VALUES(?,?,?,?,?,?)",
                params![
                    uuid(),
                    chat,
                    text(m, "text"),
                    m["sources"].to_string(),
                    now,
                    now + settings.memory_days * 86400.
                ],
            )?;
        }
        self.execute("DELETE FROM learned_memories WHERE chat=? AND (expires<=? OR id NOT IN (SELECT id FROM learned_memories WHERE chat=? ORDER BY created DESC,rowid DESC LIMIT ?))",params![chat,now,chat,settings.max_memories.floor() as i64])?;
        tx.commit()?;
        index_guard.committed = true;
        Ok(true)
    }
    pub fn reset_learning(&self, chat: &str, now: f64, subject: Option<&str>) -> Result<()> {
        let mut index_guard = MemoryIndexGuard {
            store: self,
            chat,
            committed: false,
        };
        let tx = self.immediate()?;
        let history = self.history(chat, Some(100))?;
        let last = history
            .iter()
            .rev()
            .find(|m| !crate::config::truthy(&m["self"]))
            .map(|m| text(m, "id"))
            .unwrap_or("");
        self.execute("INSERT INTO chat_learning VALUES(?,'','[]',?,?,1) ON CONFLICT(chat) DO UPDATE SET style='',sources='[]',updated=excluded.updated,last_id=excluded.last_id,epoch=epoch+1",params![chat,now,last])?;
        // JS 的旧式 style/learned_memories 为 chat 级清理；三层和表达仅删指定 subject。
        self.execute("DELETE FROM learned_memories WHERE chat=?", [chat])?;
        LayeredMemory::new(self).reset(chat, subject)?;
        ExpressionMemory::new(self).reset(chat, subject)?;
        tx.commit()?;
        index_guard.committed = true;
        Ok(())
    }
}
