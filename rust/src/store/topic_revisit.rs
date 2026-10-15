use super::*;
use rusqlite::params;

pub struct TopicClassification<'a> {
    pub chat: &'a str,
    pub topic_key: &'a str,
    pub topic: &'a str,
    pub stage: &'a str,
    pub category: &'a str,
    pub evidence: &'a str,
    pub now: f64,
}

// The topic key is an internal message_key. The human-readable topic is only a
// lookup label; protocol IDs and free-form model categories are never keys.
const TOPIC_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS topic_revisit_topics(
    chat TEXT NOT NULL, topic_key TEXT NOT NULL, topic TEXT NOT NULL,
    stage TEXT NOT NULL, category_version INTEGER NOT NULL,
    category TEXT NOT NULL, category_evidence TEXT NOT NULL,
    first_observed REAL NOT NULL, PRIMARY KEY(chat,topic_key), UNIQUE(chat,topic));
CREATE INDEX IF NOT EXISTS topic_revisit_label ON topic_revisit_topics(chat,topic);
CREATE TABLE IF NOT EXISTS topic_revisit_sends(
    message_key TEXT PRIMARY KEY, chat TEXT NOT NULL, topic_key TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS topic_revisit_feedback(
    source_message_key TEXT PRIMARY KEY, chat TEXT NOT NULL, topic_key TEXT NOT NULL,
    stage TEXT NOT NULL, category_version INTEGER NOT NULL, category TEXT NOT NULL,
    signal REAL NOT NULL, weight REAL NOT NULL, kind TEXT NOT NULL,
    observed_at REAL NOT NULL);
CREATE VIEW IF NOT EXISTS topic_revisit_status AS
    SELECT t.chat,t.topic,t.topic_key,t.stage,t.category_version,t.category,
           (1.0+COALESCE(SUM(f.signal*f.weight),0.0)) /
           (3.0+COALESCE(SUM(f.weight),0.0)) AS revisitability,
           COALESCE(SUM(CASE WHEN f.kind='positive' THEN 1 ELSE 0 END),0) AS positives
    FROM topic_revisit_topics t LEFT JOIN topic_revisit_feedback f
      ON f.chat=t.chat AND f.topic_key=t.topic_key AND f.stage=t.stage
    GROUP BY t.chat,t.topic_key;
CREATE VIEW IF NOT EXISTS topic_revisit_category_status AS
    SELECT chat,category_version,category,stage,
           (1.0+SUM(signal*weight))/(3.0+SUM(weight)) AS revisitability,
           SUM(CASE WHEN kind='positive' THEN 1 ELSE 0 END) AS positives
    FROM topic_revisit_feedback WHERE category<>'unknown'
    GROUP BY chat,category_version,category,stage;";

impl Store {
    pub(crate) fn topic_revisit_migration_required_on(db: &Connection) -> Result<bool> {
        let messages: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages')",
            [], |row| row.get(0),
        )?;
        if !messages { return Ok(false); }
        for (kind,name) in [
            ("table","topic_revisit_topics"), ("table","topic_revisit_sends"),
            ("table","topic_revisit_feedback"),
            ("view","topic_revisit_status"), ("view","topic_revisit_category_status"),
        ] {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=? AND name=?)",
                [kind,name], |row| row.get(0),
            )?;
            if !exists { return Ok(true); }
        }
        Ok(false)
    }

    pub(crate) fn install_topic_revisit_schema(db: &Connection) -> Result<()> {
        db.execute_batch(TOPIC_SCHEMA)?;
        Ok(())
    }

    /// Only a live, resolved inbound reply to a known outbound send is evidence.
    pub fn learn_topic_reply(&self, source_key: &str, now: f64, stage: &str) -> Result<bool> {
        let Some(edge) = self.first(
            "SELECT m.chat,m.text,COALESCE(s.topic_key,t.message_key) AS topic_key,
                    COALESCE(top.topic,t.text) AS topic,t.ts AS sent_at,
                    r.target_message_key
             FROM inbound_reply_relations r
             JOIN messages m ON m.message_key=r.source_message_key AND m.self=0
             JOIN messages t ON t.message_key=r.target_message_key AND t.chat=m.chat AND t.self=1
             JOIN outbound_message_facts o ON o.chat=t.chat AND o.id=t.message_key
             JOIN deliveries d ON d.id=o.delivery_id AND d.status IN ('sent_unconfirmed','confirmed','sent')
             LEFT JOIN topic_revisit_sends s ON s.chat=t.chat AND s.message_key=t.message_key
             LEFT JOIN topic_revisit_topics top ON top.chat=t.chat AND top.topic_key=s.topic_key
             WHERE r.source_message_key=? AND r.status='resolved'",
            [source_key],
        )? else { return Ok(false); };
        let chat = edge["chat"].as_str().unwrap();
        let topic_key = edge["topic_key"].as_str().unwrap();
        let topic = edge["topic"].as_str().unwrap();
        let reply = edge["text"].as_str().unwrap_or("");
        let (kind,signal) = if ["别再", "别发", "无聊", "别提"].iter().any(|s| reply.contains(s)) {
            ("negative",0.0)
        } else if ["真好笑", "再来", "喜欢", "有意思"].iter().any(|s| reply.contains(s)) {
            ("positive",1.0)
        } else { return Ok(false); };
        let tx = self.immediate()?;
        // A first observation freezes both the topic key and its category.
        self.execute("INSERT OR IGNORE INTO topic_revisit_topics VALUES(?,?,?, ?,1,'unknown','unclassified',?)",
            params![chat,topic_key,topic,stage,now])?;
        let saved = self.first("SELECT topic_key,stage,category_version,category FROM topic_revisit_topics WHERE chat=? AND topic=? ORDER BY first_observed LIMIT 1",params![chat,topic])?.unwrap();
        self.execute("INSERT OR IGNORE INTO topic_revisit_feedback VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![source_key,chat,saved["topic_key"].as_str(),saved["stage"].as_str(),saved["category_version"].as_i64(),saved["category"].as_str(),signal,1.0,kind,now])?;
        tx.commit()?;
        Ok(true)
    }

    /// Existing ACK-only sends may provide weak silence evidence, but never a
    /// fabricated historical reply. A topic must have been observed live first.
    pub fn observe_topic_silence(&self, chat: &str, now: f64, window: f64) -> Result<()> {
        let tx = self.immediate()?;
        for row in self.rows(
            "SELECT t.topic_key,t.stage,t.category_version,t.category,m.message_key,m.ts
             FROM topic_revisit_topics t
             JOIN messages m ON m.chat=t.chat AND m.self=1
             LEFT JOIN topic_revisit_sends s ON s.chat=m.chat AND s.message_key=m.message_key
             JOIN outbound_message_facts o ON o.chat=m.chat AND o.id=m.message_key
             JOIN deliveries d ON d.id=o.delivery_id AND d.status IN ('sent_unconfirmed','confirmed','sent')
             WHERE t.chat=? AND (s.topic_key=t.topic_key OR (s.message_key IS NULL AND m.text=t.topic))
               AND m.ts>=t.first_observed AND m.ts<=?
               AND NOT EXISTS(SELECT 1 FROM inbound_reply_relations r
                   WHERE r.target_message_key=m.message_key AND r.status='resolved')",
            params![chat,now-window],
        )? {
            self.execute("INSERT OR IGNORE INTO topic_revisit_feedback VALUES(?,?,?,?,?, ?,0.0,0.2,'silence',?)",
                params![row["message_key"].as_str(),chat,row["topic_key"].as_str(),row["stage"].as_str(),row["category_version"].as_i64(),row["category"].as_str(),now])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn topic_revisit(&self, chat: &str, topic: &str) -> Result<Option<Value>> {
        self.first("SELECT topic_key,stage,category,revisitability,positives FROM topic_revisit_status WHERE chat=? AND topic=? ORDER BY positives DESC,topic_key LIMIT 1",params![chat,topic])
    }

    pub fn topic_category_revisit(&self, chat: &str, category: &str, stage: &str) -> Result<Option<Value>> {
        self.first("SELECT revisitability,positives FROM topic_revisit_category_status
            WHERE chat=? AND category_version=1 AND category=? AND stage=?",
            params![chat,category,stage])
    }

    pub fn classify_topic_once(&self, c: TopicClassification<'_>) -> Result<()> {
        self.execute("INSERT OR IGNORE INTO topic_revisit_topics VALUES(?,?,?,?,1,?,?,?)",
            params![c.chat,c.topic_key,c.topic,c.stage,c.category,c.evidence,c.now])?;
        Ok(())
    }

    pub fn link_topic_send(&self, chat: &str, delivery_id: &str, topic_key: &str) -> Result<()> {
        self.execute("INSERT OR IGNORE INTO topic_revisit_sends(message_key,chat,topic_key)
            SELECT o.id,o.chat,t.topic_key FROM outbound_message_facts o
            JOIN topic_revisit_topics t ON t.chat=o.chat AND t.topic_key=?
            WHERE o.chat=? AND o.delivery_id=?",
            params![topic_key,chat,delivery_id])?;
        Ok(())
    }
}
