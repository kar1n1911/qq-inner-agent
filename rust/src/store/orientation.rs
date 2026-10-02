//! 活动块与观察期的持久化接口；沿用既有 schema，epoch 条件防止旧分析覆盖重新入群。
use super::*;
use rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityRow {
    pub signature: String,
    pub started: f64,
    pub until: f64,
    pub active: i64,
    pub probability: f64,
    pub draw: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrientationRow {
    pub chat: String,
    pub started: f64,
    pub message_count: i64,
    pub status: String,
    pub collected: i64,
    pub sources: Value,
    pub analysis: Value,
    pub retry_at: f64,
    pub error: Option<String>,
    pub epoch: i64,
    pub joined_at: f64,
}
impl Store {
    pub fn activity_state(&self) -> Result<Option<ActivityRow>> {
        self.rows("SELECT * FROM activity_rhythm WHERE id=1", [])?.into_iter().next()
            .map(|v| Ok(serde_json::from_value(v)?)).transpose()
    }
    pub fn save_activity(&self, row: &ActivityRow) -> Result<()> {
        self.execute("INSERT OR REPLACE INTO activity_rhythm VALUES(1,?,?,?,?,?,?)", params![row.signature, row.started, row.until, row.active, row.probability, row.draw])?;
        Ok(())
    }
    pub fn orientation_state(&self, chat: &str) -> Result<Option<OrientationRow>> {
        self.rows("SELECT * FROM group_orientation WHERE chat=?", [chat])?.into_iter().next().map(|mut v| {
            for key in ["sources", "analysis"] { v[key] = serde_json::from_str(v[key].as_str().unwrap_or("{}"))?; }
            Ok(serde_json::from_value(v)?)
        }).transpose()
    }
    pub fn ensure_orientation(&self, chat: &str, now: f64) -> Result<OrientationRow> {
        self.execute("INSERT OR IGNORE INTO group_orientation(chat,started) VALUES(?,?)", params![chat, now])?;
        self.orientation_state(chat)?.ok_or_else(|| anyhow::anyhow!("missing_orientation"))
    }
    pub fn orientation_joined(&self, chat: &str, timestamp: f64, now: f64) -> Result<()> {
        self.ensure_orientation(chat, now)?;
        if timestamp.is_finite() {
            // 只有更新的入群通知递增 epoch；相同/更旧通知不重复重置观察期。
            self.execute("UPDATE group_orientation SET started=?,message_count=0,status='observing',collected=0,sources='{}',analysis='{}',retry_at=0,error=NULL,epoch=epoch+1,joined_at=? WHERE chat=? AND joined_at<?", params![now,timestamp,chat,timestamp])?;
        }
        Ok(())
    }
    pub fn orientation_observe(&self, chat: &str, now: f64) -> Result<()> {
        self.ensure_orientation(chat, now)?;
        self.execute("UPDATE group_orientation SET message_count=message_count+1 WHERE chat=? AND status<>'ready'", [chat])?;
        Ok(())
    }
    pub fn orientation_sources(&self, chat: &str, epoch: i64, sources: &Value) -> Result<bool> {
        Ok(self.execute("UPDATE group_orientation SET collected=1,sources=? WHERE chat=? AND epoch=?", params![sources.to_string(),chat,epoch])? > 0)
    }
    pub fn orientation_ready(&self, chat: &str, epoch: i64, analysis: &Value) -> Result<bool> {
        Ok(self.execute("UPDATE group_orientation SET status='ready',analysis=?,error=NULL,retry_at=0 WHERE chat=? AND epoch=?", params![analysis.to_string(),chat,epoch])? > 0)
    }
    pub fn orientation_failed(&self, chat: &str, epoch: i64, now: f64) -> Result<()> {
        // 失败保留关闭闸门，固定退避 60 秒；不持久化可能含密钥的外部错误。
        self.execute("UPDATE group_orientation SET error='orientation_analysis_failed',retry_at=? WHERE chat=? AND epoch=?", params![now+60.,chat,epoch])?;
        Ok(())
    }
}
