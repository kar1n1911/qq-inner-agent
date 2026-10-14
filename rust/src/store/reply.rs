use super::*;
use rusqlite::Connection;

/// An inbound segment's protocol ID is not a user ID or an internal key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyInput {
    Target(String),
    MissingId,
    InvalidId,
    NoReply,
}

const REPLY_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS inbound_reply_relations(
    source_message_key TEXT PRIMARY KEY,
    raw_target_id TEXT,
    target_message_key TEXT,
    status TEXT NOT NULL CHECK(status IN ('resolved','unresolved')),
    reason TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS inbound_reply_target ON inbound_reply_relations(target_message_key);
CREATE VIEW IF NOT EXISTS inbound_reply_status AS
    SELECT m.message_key AS source_message_key, r.raw_target_id, r.target_message_key,
           COALESCE(r.status,'unknown') AS status, r.reason
    FROM messages m LEFT JOIN inbound_reply_relations r ON r.source_message_key=m.message_key;
CREATE TRIGGER IF NOT EXISTS inbound_reply_cleanup AFTER DELETE ON messages BEGIN
    DELETE FROM inbound_reply_relations WHERE source_message_key=OLD.message_key;
    UPDATE inbound_reply_relations SET target_message_key=NULL,status='unresolved',reason='target_pruned'
      WHERE target_message_key=OLD.message_key;
END;";

impl Store {
    pub(crate) fn reply_migration_required_on(db: &Connection) -> Result<bool> {
        let messages: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages')",
            [], |row| row.get(0),
        )?;
        if !messages { return Ok(false); }
        for (kind, name) in [("table", "inbound_reply_relations"), ("view", "inbound_reply_status"), ("trigger", "inbound_reply_cleanup")] {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=? AND name=?)",
                [kind, name], |row| row.get(0),
            )?;
            if !exists { return Ok(true); }
        }
        Ok(false)
    }

    pub(crate) fn install_reply_schema(db: &Connection) -> Result<()> {
        db.execute_batch(REPLY_SCHEMA)?;
        Ok(())
    }
}
