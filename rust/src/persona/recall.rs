//! 下钻只有一次，原文始终作为引用数据；预算包含 id、时间戳及 JSON 转义。
use crate::{
    memory::{num, text},
    store::Store,
};
use anyhow::Result;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Request {
    pub needed: bool,
    pub why: String,
    pub query: String,
    pub around_message_id: String,
    pub window: usize,
}
pub use crate::prompts::RECALL_RULE as RULE;
pub use crate::prompts::RECALL_CONTRACT as CONTRACT;
#[derive(Default)]
pub struct Budget {
    used: bool,
}
impl Budget {
    pub fn retrieve(
        &mut self,
        store: &Store,
        chat: &str,
        request: &Request,
        max_rows: usize,
        max_chars: usize,
    ) -> Result<Vec<Value>> {
        if self.used || !request.needed {
            return Ok(vec![]);
        }
        self.used = true;
        let limit = max_rows.min(20).min(if request.window == 0 {
            20
        } else {
            request.window
        });
        let cap = max_chars.min(4000);
        if limit == 0 || cap < 2 {
            return Ok(vec![]);
        }
        let rows = if !request.around_message_id.is_empty() {
            let anchor = store.rows(
                "SELECT ts FROM messages WHERE chat=? AND id=?",
                params![chat, request.around_message_id],
            )?;
            let Some(anchor) = anchor.first() else {
                return Ok(vec![]);
            };
            store.rows(
                "SELECT id,ts,text FROM messages WHERE chat=? ORDER BY abs(ts-?),ts,id LIMIT ?",
                params![chat, num(anchor, "ts"), limit],
            )?
        } else if !request.query.trim().is_empty() {
            let query: String = request.query.chars().take(200).collect();
            store.rows("SELECT id,ts,text FROM messages WHERE chat=? AND instr(text,?)>0 ORDER BY ts DESC,id LIMIT ?",params![chat,query,limit])?
        } else {
            vec![]
        };
        let mut out = vec![];
        for row in rows {
            let item = json!({"id":row["id"],"timestamp":row["ts"],"text":text(&row,"text")});
            out.push(item);
            if serde_json::to_string(&out)?.chars().count() > cap {
                out.pop();
                break;
            }
        }
        Ok(out)
    }
}
