use super::*;
use crate::config::{js_string, truthy};
use rusqlite::{params, Transaction, TransactionBehavior};
pub(crate) use crate::utils::crypto::uuid_v4 as uuid;
// JSON 列使用紧凑 UTF-8 TEXT，与 JSON.stringify/parse 数据格式互通。
// SQL NULL 保留为 JSON null；缺失行用 Option，默认对象不补入不存在的 chat 键。
pub(crate) fn decode(row: &mut Value, keys: &[&str]) -> Result<()> {
    for key in keys {
        if let Some(s) = row[*key].as_str() {
            row[*key] = serde_json::from_str(s)?;
        }
    }
    Ok(())
}
impl Store {
    pub(crate) fn immediate(&self) -> Result<Transaction<'_>> {
        Ok(Transaction::new_unchecked(
            &self.db,
            TransactionBehavior::Immediate,
        )?)
    }
    pub(crate) fn first(&self, sql: &str, args: impl Params) -> Result<Option<Value>> {
        Ok(self.rows(sql, args)?.into_iter().next())
    }
    pub fn recover_deliveries(&self) -> Result<()> {
        // 不确定投递只改状态，启动恢复绝不重发。
        self.execute(
            "UPDATE deliveries SET status='uncertain' WHERE status='pending'",
            [],
        )?;
        Ok(())
    }
    fn record_message_in_transaction(&self, m: &Value, deduplicate: bool) -> Result<(bool, String)> {
        let chat = m["chat"].as_str();
        let id = m["id"].as_str();
        let same = if deduplicate { self.first(
            "SELECT message_key FROM messages WHERE chat IS ? AND id IS ? AND sender IS ? AND text IS ? AND ts IS ? AND self=? ORDER BY rowid DESC LIMIT 1",
            params![chat,id,m["sender"].as_str(),m["text"].as_str(),m["ts"].as_f64(),truthy(&m["self"]) as i32],
        )? } else { None };
        if let Some(row) = same {
            if let Some(received_at) = m["received_at"].as_f64() {
                self.execute("UPDATE messages SET received_at=? WHERE message_key=?",params![received_at,row["message_key"].as_str()])?;
            }
            return Ok((false, row["message_key"].as_str().unwrap().to_owned()));
        }
        self.execute("INSERT INTO message_sequence DEFAULT VALUES", [])?;
        let key = format!("local-message:{}", self.db.last_insert_rowid());
        if self.first("SELECT 1 FROM messages WHERE chat IS ? AND id IS ? LIMIT 1", params![chat,id])?.is_some() {
            let detail = if deduplicate { "same raw id with distinct message evidence" } else { "same raw id on a separate outbound send" };
            self.execute("INSERT INTO message_identity_audit(kind,table_name,chat,raw_id,message_key,detail,created) VALUES('id_collision','messages',?,?,?,?,strftime('%s','now'))",params![chat,id,key,detail])?;
        }
        self.execute(
            "INSERT INTO messages(message_key,chat,id,sender,name,text,ts,self,received_at) VALUES(?,?,?,?,?,?,?,?,?)",
            params![key,chat,id,m["sender"].as_str(),m["name"].as_str(),m["text"].as_str(),m["ts"].as_f64(),truthy(&m["self"]) as i32,m["received_at"].as_f64().unwrap_or(Store::UNKNOWN_RECEIVED_AT)],
        )?;
        Ok((true, key))
    }
    pub fn record_message(&self, m: &Value) -> Result<(bool, String)> {
        if !self.db.is_autocommit() {
            return self.record_message_in_transaction(m, true);
        }
        let tx = self.immediate()?;
        let result = self.record_message_in_transaction(m, true)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn message(&self, m: &Value) -> Result<bool> {
        Ok(self.record_message(m)?.0)
    }
    pub fn record_distinct_message(&self, m: &Value) -> Result<String> {
        let tx = self.immediate()?;
        let (_, key) = self.record_message_in_transaction(m, false)?;
        tx.commit()?;
        Ok(key)
    }
    pub fn outbound_message(&self, m: &Value, delivery_id: &str) -> Result<()> {
        let tx = self.immediate()?;
        if self.first("SELECT 1 FROM outbound_message_facts WHERE delivery_id=?", [delivery_id])?.is_some() {
            tx.commit()?;
            return Ok(());
        }
        let (_, key) = self.record_message_in_transaction(m, false)?;
        self.execute(
            "INSERT INTO outbound_message_facts(chat,id,delivery_id,time_source) VALUES(?,?,?,'local_estimate')",
            params![m["chat"].as_str(), key, delivery_id],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// Reserved for a verified self echo carrying QQ server time.
    pub fn confirm_outbound(&self, chat: &str, id: &str, server_ts: f64) -> Result<bool> {
        let tx = self.immediate()?;
        let changed = self.execute(
            "UPDATE messages SET ts=? WHERE chat=? AND message_key=? AND self=1 AND EXISTS (SELECT 1 FROM outbound_message_facts WHERE chat=? AND id=? AND time_source='local_estimate')",
            params![server_ts, chat, id, chat, id],
        )? > 0;
        if changed {
            self.execute(
                "UPDATE deliveries SET status='confirmed' WHERE id=(SELECT delivery_id FROM outbound_message_facts WHERE chat=? AND id=?) AND status='sent_unconfirmed'",
                params![chat,id],
            )?;
            self.execute(
                "UPDATE outbound_message_facts SET time_source='server' WHERE chat=? AND id=?",
                params![chat,id],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }
    /// Confirm only one unambiguous ACK whose protocol ID, chat, sender and
    /// content all agree with independent OneBot evidence. The message_key is
    /// used for the update; the protocol ID is only a matching field.
    pub fn reconcile_outbound(&self, chat: &str, protocol_id: &str, sender: &str, text: &str, server_ts: f64, received_at: f64) -> Result<bool> {
        ensure!(server_ts.is_finite() && server_ts > 0. && received_at.is_finite() && received_at > 0., "invalid_outbound_evidence_time");
        let tx = self.immediate()?;
        let matches = self.rows(
            "SELECT m.message_key FROM messages m JOIN outbound_message_facts f ON f.chat=m.chat AND f.id=m.message_key JOIN deliveries d ON d.id=f.delivery_id WHERE d.chat=? AND d.message_id=? AND m.id=? AND m.sender=? AND m.text=? AND m.self=1 AND d.status='sent_unconfirmed' AND f.time_source='local_estimate' LIMIT 2",
            params![chat, protocol_id, protocol_id, sender, text],
        )?;
        if matches.len() != 1 {
            tx.commit()?;
            return Ok(false);
        }
        let key = matches[0]["message_key"].as_str().unwrap();
        self.execute("UPDATE messages SET ts=?,received_at=? WHERE message_key=?", params![server_ts,received_at,key])?;
        self.execute("UPDATE deliveries SET status='confirmed' WHERE id=(SELECT delivery_id FROM outbound_message_facts WHERE id=?)", [key])?;
        self.execute("UPDATE outbound_message_facts SET time_source='server' WHERE id=?", [key])?;
        tx.commit()?;
        Ok(true)
    }
    pub fn recent_unconfirmed_outbound(&self, now: f64) -> Result<Vec<Value>> {
        self.rows(
            "SELECT m.message_key,d.chat,d.message_id FROM deliveries d JOIN outbound_message_facts f ON f.delivery_id=d.id JOIN messages m ON m.message_key=f.id WHERE d.status='sent_unconfirmed' AND f.time_source='local_estimate' AND m.ts<=? AND m.ts>? ORDER BY m.ts ASC LIMIT 32",
            params![now - 5., now - 600.],
        )
    }
    /// 同一批入站消息一个事务；返回值逐条对应是否新增，出错整批回滚。
    pub fn messages(&self, messages: &[Value]) -> Result<Vec<bool>> {
        let tx = self.immediate()?;
        let inserted = messages
            .iter()
            .map(|m| self.record_message_in_transaction(m, true).map(|result| result.0))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(inserted)
    }
    /// None 对应 JS 默认 24；0 返回空，负数保留 SQLite 不限条数的语义。
    pub fn history(&self, chat: &str, limit: Option<i64>) -> Result<Vec<Value>> {
        let mut rows = self.rows(
            "SELECT * FROM messages WHERE chat=? ORDER BY ts DESC,rowid DESC LIMIT ?",
            params![chat, limit.unwrap_or(24)],
        )?;
        rows.reverse();
        self.enhance_ocr(&mut rows)?;
        Ok(rows)
    }
    pub fn learning_state(&self, chat: &str) -> Result<Value> {
        let mut row = self
            .first("SELECT * FROM chat_learning WHERE chat=?", [chat])?
            .unwrap_or_else(
                || json!({"style":"","sources":"[]","updated":0,"last_id":"","epoch":0}),
            );
        // JS 此方法返回原始 sources 文本；Rust 按存储接口要求返回解析结构。
        decode(&mut row, &["sources"])?;
        Ok(row)
    }
    pub fn note(&self, chat: &str, text: &str, now: f64) -> Result<()> {
        self.execute(
            "INSERT INTO notes VALUES(?,?,?,?)",
            params![uuid(), chat, text, now],
        )?;
        Ok(())
    }
    pub fn add_thought(&self, chat: &str, thought: &Value, now: f64) -> Result<Value> {
        let id = uuid();
        self.execute(
            "INSERT INTO thoughts(id,chat,text,kind,created,subject) VALUES(?,?,?,?,?,?)",
            params![
                id,
                chat,
                thought["text"].as_str(),
                thought["kind"].as_str(),
                now,
                thought["subject"].as_str()
            ],
        )?;
        Ok(json!({"id":id,"chat":chat,"text":thought["text"],"kind":thought["kind"],"created":now}))
    }
    pub fn reservoir(
        &self,
        chat: &str,
        now: f64,
        ttl: f64,
        limit: i64,
        subject: Option<&str>,
    ) -> Result<Vec<Value>> {
        match subject {
            None=>self.rows("SELECT * FROM thoughts WHERE chat=? AND used=0 AND created>? ORDER BY created DESC LIMIT ?",params![chat,now-ttl,limit]),
            Some(subject)=>self.rows("SELECT * FROM thoughts WHERE chat=? AND subject=? AND used=0 AND created>? ORDER BY created DESC LIMIT ?",params![chat,subject,now-ttl,limit]),
        }
    }
    pub fn score(&self, id: &str, score: f64) -> Result<()> {
        self.execute("UPDATE thoughts SET score=? WHERE id=?", params![score, id])?;
        Ok(())
    }
    pub fn r#use(&self, id: &str) -> Result<()> {
        self.execute("UPDATE thoughts SET used=1 WHERE id=?", [id])?;
        Ok(())
    }
    pub fn decision(
        &self,
        chat: &str,
        action: &str,
        score: f64,
        tags: &Value,
        now: f64,
    ) -> Result<()> {
        self.execute(
            "INSERT INTO decisions VALUES(?,?,?,?,?,?)",
            params![uuid(), chat, now, action, score, tags.to_string()],
        )?;
        Ok(())
    }
    pub fn call_budget(&self, now: f64, max: f64) -> Result<bool> {
        // 滚动 3600 秒；IMMEDIATE 避免共库多写者同时通过准入。
        let tx = self.immediate()?;
        self.execute("DELETE FROM calls WHERE ts<?", [now - 3600.])?;
        let count: i64 = self
            .db
            .prepare_cached("SELECT count(*) FROM calls")?
            .query_row([], |r| r.get(0))?;
        let admitted = (count as f64) < max;
        if admitted {
            self.execute("INSERT INTO calls VALUES(?)", [now])?;
        }
        tx.commit()?;
        Ok(admitted)
    }
    pub fn delivery(&self, chat: &str, proactive: bool, now: f64) -> Result<String> {
        let id = uuid();
        self.execute(
            "INSERT INTO deliveries VALUES(?,?,?,?,?,NULL)",
            params![id, chat, now, proactive as i32, "pending"],
        )?;
        Ok(id)
    }
    pub fn finish_delivery(
        &self,
        id: &str,
        status: &str,
        message_id: Option<&Value>,
    ) -> Result<()> {
        let message_id = message_id.filter(|v| !v.is_null()).map(js_string);
        self.execute(
            "UPDATE deliveries SET status=?,message_id=? WHERE id=?",
            params![status, message_id, id],
        )?;
        Ok(())
    }
    pub fn counts(&self, chat: &str, now: f64) -> Result<Value> {
        Ok(self.first("SELECT count(*) AS total, coalesce(sum(proactive),0) AS proactive, coalesce(max(ts),0) AS last FROM deliveries WHERE chat=? AND ts>? AND status IN ('sent','sent_unconfirmed','confirmed','pending','uncertain','ack_missing')",params![chat,now-3600.])?.unwrap())
    }
    pub fn mark_handled(&self, chat: &str, id: &str, pause: bool) -> Result<()> {
        let key = self.resolve_message_key(chat, id)?.unwrap_or_else(|| id.to_owned());
        ensure!(id.starts_with("local-message:") || self.rows("SELECT message_key FROM messages WHERE chat=? AND id=? LIMIT 2",params![chat,id])?.len() < 2,"ambiguous_message_id");
        self.execute("INSERT INTO handled VALUES(?,?,?) ON CONFLICT(chat) DO UPDATE SET human_id=excluded.human_id,pause_done=excluded.pause_done",params![chat,key,pause as i32])?;
        Ok(())
    }
    pub fn handled(&self, chat: &str) -> Result<Option<Value>> {
        self.first("SELECT * FROM handled WHERE chat=?", [chat])
    }
    pub fn assessment(&self, chat: &str, human_id: &str) -> Result<Option<Value>> {
        let key = self.resolve_message_key(chat, human_id)?.unwrap_or_else(|| human_id.to_owned());
        let mut row = self.first(
            "SELECT * FROM send_assessments WHERE chat=? AND human_id=?",
            params![chat, key],
        )?;
        if let Some(r) = &mut row {
            decode(r, &["details"])?;
        }
        Ok(row)
    }
    pub fn sending_timing(&self, chat: &str, now: f64, fallback_gap: f64) -> Result<Value> {
        let last:Option<f64>=self.db.prepare_cached("SELECT max(ts) FROM deliveries WHERE chat=? AND status IN ('sent','sent_unconfirmed','confirmed','pending','uncertain','ack_missing')")?.query_row([chat],|r|r.get(0))?;
        let humans: i64 = self
            .db
            .prepare_cached("SELECT count(*) FROM messages WHERE chat=? AND self=0 AND ts>=?")?
            .query_row(params![chat, now - 60.], |r| r.get(0))?;
        Ok(json!({"gap":last.map_or(fallback_gap,|ts|(now-ts).max(0.)),"recentHumans":humans}))
    }
    pub fn assess(
        &self,
        chat: &str,
        human_id: &str,
        now: f64,
        status: &str,
        details: &Value,
    ) -> Result<()> {
        self.execute(
            "INSERT INTO send_assessments VALUES(?,?,?,?,?,?)",
            params![uuid(), chat, human_id, now, status, details.to_string()],
        )?;
        Ok(())
    }
    pub fn assessment_status(&self, chat: &str, human_id: &str, status: &str) -> Result<()> {
        self.execute(
            "UPDATE send_assessments SET status=? WHERE chat=? AND human_id=?",
            params![status, chat, human_id],
        )?;
        Ok(())
    }
    pub fn expect(&self, chat: &str, now: f64, seconds: f64, forecast: &Value) -> Result<()> {
        self.execute("INSERT INTO expectations VALUES(?,?,?,?,NULL) ON CONFLICT(chat) DO UPDATE SET ts=excluded.ts,expires=excluded.expires,forecast=excluded.forecast,observation=NULL",params![chat,now,now+seconds,forecast.to_string()])?;
        Ok(())
    }
    pub fn observe(&self, message: &Value, now: f64) -> Result<()> {
        let observation =
            json!({"event":"human_message","addressed":message["hint"]=="self","at":now});
        self.execute("UPDATE expectations SET observation=? WHERE chat=? AND observation IS NULL AND ts<=? AND expires>=?",params![observation.to_string(),message["chat"].as_str(),now,now])?;
        Ok(())
    }
    pub fn expectation(&self, chat: &str, now: f64) -> Result<Option<Value>> {
        let Some(mut row) = self.first(
            "SELECT * FROM expectations WHERE chat=? AND expires>?",
            params![chat, now],
        )?
        else {
            return Ok(None);
        };
        let observed = row["observation"].as_str().is_some_and(|s| !s.is_empty());
        decode(&mut row, &["forecast"])?;
        if observed {
            decode(&mut row, &["observation"])?;
        }
        Ok(Some(
            json!({"forecast":row["forecast"],"elapsedSeconds":(now-row["ts"].as_f64().unwrap()).max(0.),"observation":if observed {row["observation"].clone()} else {json!({"event":"no_message_yet"})}}),
        ))
    }
    pub fn active_chats(&self, since: f64) -> Result<Vec<String>> {
        self.rows(
            "SELECT DISTINCT chat FROM messages WHERE self=0 AND ts>?",
            [since],
        )?
        .into_iter()
        .map(|r| {
            r["chat"]
                .as_str()
                .map(str::to_owned)
                .context("NULL chat in messages")
        })
        .collect()
    }
    pub fn prune(&self, now: f64, retention_days: f64, max_per_chat: i64) -> Result<()> {
        // 天只在此转换成秒；批量清理一个事务，显式 rowid 保持同时间消息的顺序。
        let tx = self.immediate()?;
        let cutoff = now - retention_days * 86400.;
        self.execute("DELETE FROM messages WHERE ts<?", [cutoff])?;
        for row in self.rows("SELECT DISTINCT chat FROM messages", [])? {
            let chat = row["chat"].as_str();
            self.execute("DELETE FROM messages WHERE chat=? AND rowid NOT IN (SELECT rowid FROM messages WHERE chat=? ORDER BY ts DESC,rowid DESC LIMIT ?)",params![chat,chat,max_per_chat])?;
        }
        self.execute("DELETE FROM thoughts WHERE created<?", [now - 86400.])?;
        self.execute(
            "DELETE FROM learned_memories WHERE expires<=? OR created<?",
            params![now, cutoff],
        )?;
        // 分层记忆只按自身 expires 清理；删除自动触发历史版本清理。
        self.execute("DELETE FROM memory_layers WHERE expires<=?", [now])?;
        self.execute("UPDATE group_orientation SET sources=json_remove(sources,'$.history','$.notices') WHERE started<?",[cutoff])?;
        self.execute(
            "UPDATE chat_learning SET style='',sources='[]' WHERE updated<?",
            [cutoff],
        )?;
        for table in [
            "decisions",
            "deliveries",
            "send_assessments",
            "expectations",
        ] {
            self.execute(&format!("DELETE FROM {table} WHERE ts<?"), [cutoff])?;
        }
        self.execute(
            "DELETE FROM handled WHERE chat NOT IN (SELECT DISTINCT chat FROM messages)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }
}
