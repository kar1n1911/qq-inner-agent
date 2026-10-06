//! 身份自治：高风险修改仅生成建议，必须经过主人私聊确认。
use crate::{config::Identity, store::Store};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    pub chat: String,
    pub nickname: String,
    pub group_card: String,
    // 当前规则不生成图片；只有明确的文件提案才能调用头像 API。
    pub avatar: Option<String>,
    #[serde(default)]
    pub completed: Vec<String>,
}

/// 延迟建表，默认关闭时保持原数据库结构和 parity 快照不变。
pub fn init(store: &Store) -> Result<()> {
    store.execute("CREATE TABLE IF NOT EXISTS identity_proposal(id INTEGER PRIMARY KEY CHECK(id=1), pending TEXT, applied INTEGER NOT NULL DEFAULT 0)", [])?;
    store.execute("INSERT OR IGNORE INTO identity_proposal(id) VALUES(1)", [])?;
    Ok(())
}
pub fn pending(store: &Store) -> Result<Option<Proposal>> {
    init(store)?;
    let rows = store.rows("SELECT pending FROM identity_proposal WHERE id=1", [])?;
    rows[0]["pending"]
        .as_str()
        .map(serde_json::from_str)
        .transpose()
        .map_err(Into::into)
}
pub fn applied(store: &Store) -> Result<bool> {
    init(store)?;
    Ok(store.rows("SELECT applied FROM identity_proposal WHERE id=1", [])?[0]["applied"] == 1)
}
pub fn save(store: &Store, proposal: &Proposal) -> Result<()> {
    init(store)?;
    store.execute(
        "UPDATE identity_proposal SET pending=? WHERE id=1",
        [serde_json::to_string(proposal)?],
    )?;
    Ok(())
}
pub fn clear(store: &Store, applied: bool) -> Result<()> {
    init(store)?;
    store.execute(
        "UPDATE identity_proposal SET pending=NULL,applied=max(applied,?) WHERE id=1",
        [applied],
    )?;
    Ok(())
}

pub fn enough(store: &Store, chat: &str, now: f64, cfg: &Identity) -> Result<bool> {
    if !chat.starts_with("group:")
        || !now.is_finite()
        || !cfg.min_age_days.is_finite()
        || cfg.min_age_days < 0.
    {
        return Ok(false);
    }
    // 优先使用当前入群/观察周期，缺少观察记录时以最早的人类群消息兜底。
    let started = if let Some(row) = store.orientation_state(chat)? {
        Some(if row.joined_at > 0. {
            row.joined_at
        } else {
            row.started
        })
    } else {
        store.rows(
            "SELECT min(ts) AS started FROM messages WHERE chat=? AND self=0",
            [chat],
        )?[0]["started"]
            .as_f64()
    };
    let count = store.rows("SELECT count(*) AS n FROM memory_layers WHERE chat=? AND subject='group' AND layer='traits' AND (expires IS NULL OR expires>?)", params![chat,now])?[0]["n"].as_u64().unwrap_or(0);
    Ok(
        started.is_some_and(|t| now - t >= cfg.min_age_days * 86400.)
            && count >= cfg.min_traits as u64,
    )
}

/// 简单确定性归纳：persona 短语 + 证据来源最多的群 trait；同频按 slot 排序。
pub fn propose(store: &Store, chat: &str, persona: &str, now: f64) -> Result<Proposal> {
    ensure!(
        chat.strip_prefix("group:")
            .is_some_and(|s| s.parse::<u64>().is_ok_and(|id| id > 0)),
        "invalid_identity_group"
    );
    let mut traits = store.rows("SELECT text,sources,slot FROM memory_layers WHERE chat=? AND subject='group' AND layer='traits' AND (expires IS NULL OR expires>?) ORDER BY slot", params![chat,now])?;
    traits.sort_by_cached_key(|v| {
        let count = serde_json::from_str::<Value>(v["sources"].as_str().unwrap_or("[]"))
            .ok()
            .and_then(|s| s.as_array().map(Vec::len))
            .unwrap_or(0);
        std::cmp::Reverse(count)
    });
    let top = traits
        .first()
        .and_then(|v| v["text"].as_str())
        .unwrap_or("群友");
    fn short(text: &str, limit: usize) -> String {
        text.chars()
            .filter(|c| !c.is_control() && !c.is_whitespace())
            .take(limit)
            .collect()
    }
    let persona = short(persona, 6);
    let persona = if persona.is_empty() {
        "群友"
    } else {
        &persona
    };
    Ok(Proposal {
        chat: chat.into(),
        nickname: format!("{}·{}", persona, short(top, 6)),
        group_card: format!("{}·{}", persona, short(top, 12)),
        avatar: None,
        completed: vec![],
    })
}

impl Proposal {
    pub fn notice(&self) -> String {
        let avatar = self
            .avatar
            .as_ref()
            .map(|file| format!(" / 头像 {file}"))
            .unwrap_or_default();
        format!(
            "建议改名 {} / 群名片 {}（{}）{},回复 /同意改名 或 /忽略",
            self.nickname, self.group_card, self.chat, avatar
        )
    }
    pub fn actions(&self, cfg: &Identity, self_id: &str) -> Vec<(&'static str, Value)> {
        let mut actions = Vec::new();
        if cfg.allow_group_card {
            actions.push(("set_group_card", serde_json::json!({"group_id":self.chat.trim_start_matches("group:"),"user_id":self_id,"card":self.group_card})));
        }
        if cfg.allow_nickname {
            actions.push((
                "set_qq_profile",
                serde_json::json!({"nickname":self.nickname}),
            ));
        }
        if let Some(file) = self.avatar.as_ref().filter(|_| cfg.allow_avatar) {
            actions.push(("set_qq_avatar", serde_json::json!({"file":file})));
        }
        actions.retain(|(name, _)| !self.completed.iter().any(|done| done == name));
        actions
    }
}
