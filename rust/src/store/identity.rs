use super::*;
use rusqlite::params;

impl Store {
    pub(crate) fn message_key_migration_required_on(db: &Connection) -> Result<bool> {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        let mut statement = db.prepare("PRAGMA table_info(messages)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(!columns.iter().any(|name| name == "message_key"))
    }

    pub(crate) fn requires_message_key_migration(path: impl AsRef<Path>) -> Result<bool> {
        let db = Connection::open(path)?;
        Ok(Self::message_key_migration_required_on(&db)? || Self::reply_migration_required_on(&db)?
            || Self::topic_revisit_migration_required_on(&db)?)
    }

    /// All message references change in one transaction. Missing targets remain as
    /// rows with an isolated unresolved key and an audit entry containing the old id.
    pub fn migrate_message_keys(path: impl AsRef<Path>) -> Result<()> {
        let mut db = Connection::open(path)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !Self::message_key_migration_required_on(&tx)? {
            Self::install_reply_schema(&tx)?;
            Self::install_topic_revisit_schema(&tx)?;
            tx.commit()?;
            return Ok(());
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS message_sequence(id INTEGER PRIMARY KEY AUTOINCREMENT);
             CREATE TABLE IF NOT EXISTS message_identity_audit(id INTEGER PRIMARY KEY,kind TEXT NOT NULL,table_name TEXT NOT NULL,chat TEXT,raw_id TEXT,message_key TEXT,detail TEXT,created REAL);
             CREATE TABLE IF NOT EXISTS migration_message_key_v1(completed INTEGER PRIMARY KEY CHECK(completed=1));
             CREATE TABLE messages_identity_new(message_key TEXT NOT NULL PRIMARY KEY,chat TEXT,id TEXT,sender TEXT,name TEXT,text TEXT,ts REAL,self INTEGER DEFAULT 0,received_at REAL NOT NULL DEFAULT -1);
             INSERT INTO message_sequence(id) SELECT rowid FROM messages ORDER BY rowid;
             INSERT INTO messages_identity_new(rowid,message_key,chat,id,sender,name,text,ts,self,received_at)
               SELECT rowid,printf('local-message:%d',rowid),chat,id,sender,name,text,ts,self,received_at FROM messages ORDER BY rowid;
             DROP TABLE messages;
             ALTER TABLE messages_identity_new RENAME TO messages;
             CREATE INDEX messages_chat_id ON messages(chat,id);
             CREATE INDEX messages_chat_ts ON messages(chat,ts);",
        )?;
        for (table, column, filter) in [
            ("outbound_message_facts", "id", "1=1"),
            ("media_receipts", "message_id", "1=1"),
            ("media_stages", "message_id", "1=1"),
            ("media_contexts", "message_id", "1=1"),
            ("media_ocr", "message_id", "1=1"),
            ("message_ratings", "message_id", "1=1"),
            ("humanize_faces", "id", "1=1"),
            ("media_feedback", "event", "1=1"),
            ("media_pending", "event", "1=1"),
            ("handled", "human_id", "1=1"),
            ("send_assessments", "human_id", "1=1"),
            ("chat_learning", "last_id", "1=1"),
            ("affect_bursts", "human_id", "1=1"),
            ("memory_layers", "slot", "layer='short_term'"),
        ] {
            if !table_exists(&tx, table)? {
                continue;
            }
            tx.execute(&format!(
                "INSERT INTO message_identity_audit(kind,table_name,chat,raw_id,message_key,detail,created)
                 SELECT 'unresolved_reference','{table}',r.chat,r.{column},'unresolved:{table}:'||r.rowid,'no stored message',strftime('%s','now')
                 FROM {table} r WHERE {filter} AND r.{column} IS NOT NULL AND r.{column}<>'' AND NOT EXISTS
                   (SELECT 1 FROM messages m WHERE m.chat=r.chat AND m.id=r.{column})"
            ), [])?;
            tx.execute(&format!(
                "UPDATE {table} SET {column}=COALESCE(
                     (SELECT m.message_key FROM messages m WHERE m.chat={table}.chat AND m.id={table}.{column}),
                     'unresolved:{table}:'||{table}.rowid)
                 WHERE {filter} AND {column} IS NOT NULL AND {column}<>''"
            ), [])?;
        }
        migrate_source_json(&tx)?;
        tx.execute("INSERT OR IGNORE INTO migration_message_key_v1 VALUES(1)", [])?;
        Self::install_reply_schema(&tx)?;
        Self::install_topic_revisit_schema(&tx)?;
        tx.commit()?;
        Ok(())
    }

    /// Raw ids are accepted only when they uniquely identify an existing row.
    pub fn resolve_message_key(&self, chat: &str, id: &str) -> Result<Option<String>> {
        if id.starts_with("local-message:") {
            return Ok(self.first("SELECT message_key FROM messages WHERE chat=? AND message_key=?", params![chat,id])?
                .and_then(|row| row["message_key"].as_str().map(str::to_owned)));
        }
        let rows = self.rows("SELECT message_key FROM messages WHERE chat=? AND id=? ORDER BY rowid DESC LIMIT 2", params![chat,id])?;
        if rows.len() > 1 {
            self.execute("INSERT INTO message_identity_audit(kind,table_name,chat,raw_id,detail,created) VALUES('ambiguous_lookup','messages',?,?,'raw id names multiple messages',strftime('%s','now'))",params![chat,id])?;
            return Ok(None);
        }
        Ok(rows.first().and_then(|row| row["message_key"].as_str().map(str::to_owned)))
    }
}

fn table_exists(db: &Connection, name: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)", [name], |row| row.get(0))?)
}

fn migrate_source_json(db: &Connection) -> Result<()> {
    for table in ["memory_layers", "memory_revisions", "chat_learning", "learned_memories", "expressions", "affect_state"] {
        if !table_exists(db, table)? {
            continue;
        }
        let query = if table == "memory_revisions" {
            "SELECT r.rowid,l.chat,r.sources FROM memory_revisions r LEFT JOIN memory_layers l ON l.id=r.memory_id".to_owned()
        } else {
            format!("SELECT rowid,chat,sources FROM {table}")
        };
        let mut statement = db.prepare(&query)?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?))
        })?.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        for (rowid, chat, raw) in rows {
            let Some(raw) = raw else { continue };
            let Some(chat) = chat else {
                db.execute("INSERT INTO message_identity_audit(kind,table_name,detail,created) VALUES('unresolved_reference','memory_revisions',?,strftime('%s','now'))",[format!("revision row {rowid} has no parent chat")])?;
                continue;
            };
            let Ok(mut value) = serde_json::from_str::<Value>(&raw) else {
                db.execute("INSERT INTO message_identity_audit(kind,table_name,chat,detail,created) VALUES('unresolved_reference',?,?, 'invalid source JSON',strftime('%s','now'))",params![table,chat])?;
                continue;
            };
            let original = value.clone();
            map_source_ids(db, table, &chat, &mut value)?;
            if value != original {
                db.execute(&format!("UPDATE {table} SET sources=? WHERE rowid=?"), params![value.to_string(),rowid])?;
            }
        }
    }
    Ok(())
}

fn map_source_ids(db: &Connection, table: &str, chat: &str, value: &mut Value) -> Result<()> {
    match value {
        Value::Array(values) => for value in values { map_source_ids(db, table, chat, value)?; },
        Value::Object(fields) => {
            if let Some(Value::String(raw)) = fields.get("id") {
                let raw = raw.clone();
                if raw.is_empty() {
                    for value in fields.values_mut() { map_source_ids(db, table, chat, value)?; }
                    return Ok(());
                }
                let key = db.query_row("SELECT message_key FROM messages WHERE chat=? AND id=?",params![chat,raw],|row|row.get::<_, String>(0));
                match key {
                    Ok(key) => { fields.insert("id".into(), Value::String(key)); },
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        db.execute("INSERT INTO message_identity_audit(kind,table_name,chat,raw_id,detail,created) VALUES('unresolved_reference',?,?,?,'source JSON has no stored message',strftime('%s','now'))",params![table,chat,raw])?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            for value in fields.values_mut() { map_source_ids(db, table, chat, value)?; }
        }
        _ => {}
    }
    Ok(())
}
