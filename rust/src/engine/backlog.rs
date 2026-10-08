//! 积压简读：仅抽取头尾和中间样本，不把省略消息伪装成已经读过的上下文。
use crate::store::Store;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub threshold: usize,
    pub head_count: usize,
    pub tail_count: usize,
    pub sample_max: usize,
    pub sample_percent: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 80,
            head_count: 4,
            tail_count: 8,
            sample_max: 20,
            sample_percent: 10.,
        }
    }
}

impl Settings {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=1_000_000).contains(&self.threshold)
                && (1..=100).contains(&self.head_count)
                && (1..=100).contains(&self.tail_count)
                && self.sample_max <= 100
                && self.sample_percent.is_finite()
                && (0. ..=100.).contains(&self.sample_percent),
            "Invalid observation.backlogDigest limits"
        );
        Ok(())
    }
}

pub const INSTRUCTIONS: &str = "当前 history 是积压简读，不是连续完整对话。结合头部、尾部和中间随机抽样，判断话题是否仍在延续、当前是否有重要或未解决的问题值得接话；以尾部的最新状态为准。不要把抽样相邻当作原对话相邻，不推断省略部分的内容、共识或已解决状态。沿用 allocation/candidates 输出：值得接时只围绕当前相关问题给出候选，不值得接或证据不足时允许空 candidates；评价候选时用现有 relevance、coherence、urgency 等标准体现话题延续性和重要性，不必额外输出推理或新增字段。";

pub struct Digest {
    pub messages: Vec<Value>,
    pub context: Value,
}

// 计数、全读与采样共享同一边界；不以 timestamp 或 handled 状态替代自身发言。
const UNREAD: &str = "chat=?1 AND self=0 AND rowid > \
    COALESCE((SELECT MAX(rowid) FROM messages WHERE chat=?1 AND self=1),0)";

pub fn unread_count(db: &Store, chat: &str) -> Result<i64> {
    let rows = db.rows(
        &format!("SELECT COUNT(*) AS n FROM messages WHERE {UNREAD}"),
        [chat],
    )?;
    Ok(rows[0]["n"].as_i64().unwrap_or_default())
}

/// 保留 history 的时间戳/rowid 排序及最近上下文，同时完整包含入库边界后的积压。
pub fn full_history(db: &Store, chat: &str, minimum: i64) -> Result<Vec<Value>> {
    let limit = minimum.max(unread_count(db, chat)?);
    // db.history 取时间上最后 N 条；晚到的旧时间戳消息可能落在 N 条之外。
    // 并入未读区间以保证不漏，通常仍是 limit 条，仅乱序时可能多于该下限。
    let mut rows = db.rows(
        &format!(
            "SELECT * FROM messages WHERE rowid IN \
             (SELECT rowid FROM messages WHERE chat=?1 ORDER BY ts DESC,rowid DESC LIMIT ?2) \
             OR ({UNREAD}) ORDER BY ts,rowid"
        ),
        rusqlite::params![chat, limit],
    )?;
    db.enhance_ocr(&mut rows)?;
    Ok(rows)
}

// 固定 FNV-1a 加 avalanche，避免运行时随机种子或顺序消息 id 的局部聚集。
// 相同 chat/id 得到相同排名；并列时按原位置，跨进程、平台可重现。
fn rank(chat: &str, id: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in chat.bytes().chain([0]).chain(id.bytes()) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d049bb133111eb);
    hash ^ (hash >> 31)
}

/// 积压 = 同 chat 最后一次自身发言入库之后的非自身消息；没有自身发言则从保留历史起算。
/// rowid 代表入库顺序，避免同秒时间戳、乱序到达或时钟回拨破坏边界。
pub fn build(db: &Store, chat: &str, settings: &Settings) -> Result<Option<Digest>> {
    if !settings.enabled {
        return Ok(None);
    }
    // 只扫描 id，未选中的正文不载入；不会把几百条完整正文送给模型。
    let ids = db.rows(
        &format!("SELECT id FROM messages WHERE {UNREAD} ORDER BY rowid"),
        [chat],
    )?;
    let total = ids.len();
    if total <= settings.threshold {
        return Ok(None);
    }
    let head_end = settings.head_count.min(total);
    let tail_start = total.saturating_sub(settings.tail_count).max(head_end);
    let mut sample: Vec<_> = (head_end..tail_start).collect();
    let sample_count = if settings.sample_percent == 0. {
        settings.sample_max
    } else {
        settings
            .sample_max
            .min((sample.len() as f64 * settings.sample_percent / 100.).round() as usize)
    };
    sample.sort_unstable_by_key(|&i| (rank(chat, ids[i]["id"].as_str().unwrap_or_default()), i));
    sample.truncate(sample_count);
    sample.sort_unstable();
    let positions: Vec<_> = (0..head_end)
        .chain(sample.iter().copied())
        .chain(tail_start..total)
        .collect();
    let omitted = total - positions.len();
    let mut messages = Vec::with_capacity(positions.len());
    for &i in &positions {
        messages.extend(db.rows(
            "SELECT * FROM messages WHERE chat=? AND id=?",
            rusqlite::params![chat, ids[i]["id"].as_str()],
        )?);
    }
    db.enhance_ocr(&mut messages)?;
    Ok(Some(Digest {
        messages,
        context: json!({
            "totalMessages":total,
            "omittedMessages":omitted,
            "headIds":ids[..head_end].iter().map(|row| &row["id"]).collect::<Vec<_>>(),
            "tailIds":ids[tail_start..].iter().map(|row| &row["id"]).collect::<Vec<_>>(),
            "sampleIds":sample.iter().map(|&i| &ids[i]["id"]).collect::<Vec<_>>(),
            "notice":format!("中间省略了 {omitted} 条，以下为随机抽样：sampleIds 所列的 {} 条消息；history 按入库顺序展示头部、抽样和尾部。", sample.len()),
            "instructions":INSTRUCTIONS
        }),
    }))
}
