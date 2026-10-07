//! 三指标正交：评分不是奖励，也不进入内容；只导出离散行为。
use crate::{
    memory::{num, text},
    store::Store,
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Angry,
    Withdrawn,
    Scrutinizing,
    Supportive,
}
/// 零归入正轴；二维坐标独立，agreement 不参与四象限。
pub fn disposition(valence: f64, rationality: f64) -> Disposition {
    match (valence < 0., rationality < 0.) {
        (true, true) => Disposition::Angry,
        (true, false) => Disposition::Withdrawn,
        (false, false) => Disposition::Scrutinizing,
        (false, true) => Disposition::Supportive,
    }
}
impl Disposition {
    pub fn motivation(self) -> f64 {
        match self {
            Self::Angry => 1.4,
            Self::Withdrawn => 0.2,
            Self::Scrutinizing => 0.7,
            Self::Supportive => 1.2,
        }
    }
    pub fn length(self) -> &'static str {
        match self {
            Self::Angry | Self::Supportive => "short",
            Self::Withdrawn => "medium",
            Self::Scrutinizing => "long",
        }
    }
    pub fn rule(self) -> &'static str {
        match self {
            Self::Angry => "直接、少修饰，不编造事实，不泄露内部信息。",
            Self::Withdrawn => "倾向沉默，只在确有必要时回应。",
            Self::Scrutinizing => "克制严谨，加强事实自检，宁可不说。",
            Self::Supportive => "温和支持，不用玩笑或黑话。",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Mood,
    Rationality,
    Affinity,
    Agreement,
}
impl Dimension {
    fn key(self) -> &'static str {
        match self {
            Self::Mood => "mood",
            Self::Rationality => "rationality",
            Self::Affinity => "affinity",
            Self::Agreement => "agreement",
        }
    }
    fn half_life(self) -> f64 {
        if self == Self::Affinity {
            7. * 86400.
        } else {
            4. * 3600.
        }
    }
}
/// 纯函数：时钟回拨不反向放大状态。
pub fn decay(value: f64, baseline: f64, elapsed_seconds: f64, half_life: f64) -> f64 {
    assert!(
        value.is_finite()
            && baseline.is_finite()
            && elapsed_seconds.is_finite()
            && half_life.is_finite()
            && half_life > 0.
    );
    baseline + (value - baseline) * 0.5_f64.powf(elapsed_seconds.max(0.) / half_life)
}
pub fn bounded_step(signal: f64, confidence: f64) -> f64 {
    assert!(
        signal.is_finite() && (-1. ..=1.).contains(&signal) && (0. ..=1.).contains(&confidence)
    );
    (signal * if signal < 0. { 0.20 } else { 0.10 } * confidence).clamp(-0.15, 0.15)
}
pub fn enable(store: &Store) -> Result<()> {
    store
        .connection()
        .execute_batch(include_str!("../store/affect_schema.sql"))?;
    Ok(())
}
fn unit(v: f64) -> Result<()> {
    ensure!(v.is_finite() && (-1. ..=1.).contains(&v), "invalid_affect");
    Ok(())
}
pub fn rate(
    store: &Store,
    chat: &str,
    id: &str,
    mood: f64,
    agreement: f64,
    confidence: f64,
    now: f64,
) -> Result<()> {
    unit(mood)?;
    unit(agreement)?;
    unit(confidence)?;
    ensure!(confidence >= 0. && now.is_finite(), "invalid_affect");
    ensure!(
        !store
            .rows(
                "SELECT id FROM messages WHERE chat=? AND id=? AND self=0",
                params![chat, id]
            )?
            .is_empty(),
        "invalid_affect_source"
    );
    store.execute(
        "INSERT OR REPLACE INTO message_ratings VALUES(?,?,?,?,?,?)",
        params![id, chat, mood, agreement, confidence, now],
    )?;
    Ok(())
}
/// 读取时按 baseline + (v0-baseline)*0.5^((t-updated)/half_life) 衰减并写回。
pub fn read(
    store: &Store,
    chat: &str,
    subject: &str,
    dimension: Dimension,
    now: f64,
) -> Result<f64> {
    ensure!(now.is_finite(), "invalid_affect_time");
    let rows = store.rows(
        "SELECT * FROM affect_state WHERE chat=? AND subject=? AND dimension=?",
        params![chat, subject, dimension.key()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(0.);
    };
    let elapsed = (now - num(row, "updated")).max(0.);
    let baseline = num(row, "baseline");
    let value = decay(num(row, "value"), baseline, elapsed, dimension.half_life());
    store.execute(
        "UPDATE affect_state SET value=?,updated=? WHERE chat=? AND subject=? AND dimension=?",
        params![
            value,
            now.max(num(row, "updated")),
            chat,
            subject,
            dimension.key()
        ],
    )?;
    Ok(value)
}
/// 单向约束在写入边界断言，不能用认同更新任何持续情感；没有奖励目标接口。
#[allow(clippy::too_many_arguments)]
pub fn update(
    store: &Store,
    chat: &str,
    subject: &str,
    target: Dimension,
    source: Dimension,
    signal: f64,
    confidence: f64,
    id: &str,
    now: f64,
) -> Result<()> {
    assert!(
        source != Dimension::Agreement && target != Dimension::Agreement,
        "agreement must never aggregate into affinity or affect"
    );
    ensure!(
        source == target || (source == Dimension::Mood && target == Dimension::Affinity),
        "invalid_affect_direction"
    );
    unit(signal)?;
    unit(confidence)?;
    ensure!(confidence >= 0., "invalid_affect_confidence");
    let evidence = store.rows(
        "SELECT sender FROM messages WHERE chat=? AND id=? AND self=0",
        params![chat, id],
    )?;
    ensure!(
        evidence
            .first()
            .is_some_and(|r| subject == format!("person:{}", text(r, "sender"))
                || (subject == "group" && target != Dimension::Affinity)),
        "invalid_affect_scope"
    );
    let old = read(store, chat, subject, target, now)?;
    // 先置信缩放再硬限幅：负性非对称不突破单步 ±0.15。
    let step = bounded_step(signal, confidence);
    store.execute("INSERT INTO affect_state VALUES(?,?,?,?,0,?,?,?) ON CONFLICT(chat,subject,dimension) DO UPDATE SET value=excluded.value,confidence=excluded.confidence,updated=excluded.updated,sources=excluded.sources",params![chat,subject,target.key(),(old+step).clamp(-1.,1.),confidence,now,json!([id]).to_string()])?;
    Ok(())
}
pub fn reset(store: &Store, chat: &str, subject: &str) -> Result<()> {
    store.execute(
        "DELETE FROM affect_state WHERE chat=? AND subject=?",
        params![chat, subject],
    )?;
    store.execute("DELETE FROM message_ratings WHERE chat=? AND message_id IN (SELECT id FROM messages WHERE chat=? AND (?='group' OR 'person:'||sender=?))",params![chat,chat,subject,subject])?;
    store.execute("DELETE FROM affect_bursts WHERE chat=?", [chat])?;
    Ok(())
}
/// 熔断按 chat 与最后人类消息持久化；无新回应最多三次，预留在传输前完成。
pub fn burst_allowed(store: &Store, chat: &str, human_id: &str) -> Result<bool> {
    let rows = store.rows(
        "SELECT count FROM affect_bursts WHERE chat=? AND human_id=?",
        params![chat, human_id],
    )?;
    Ok(rows.first().is_none_or(|r| num(r, "count") < 3.))
}
pub fn reserve_burst(store: &Store, chat: &str, human_id: &str) -> Result<()> {
    ensure!(burst_allowed(store, chat, human_id)?, "affect_circuit_open");
    store.execute("INSERT INTO affect_bursts VALUES(?,?,1) ON CONFLICT(chat) DO UPDATE SET count=CASE WHEN human_id=excluded.human_id THEN count+1 ELSE 1 END,human_id=excluded.human_id",params![chat,human_id])?;
    Ok(())
}
pub const CONTRACT:&str="可选 affect 字段为 {mood,agreement,rationality,affinity,confidence}，各值独立判断，前三轴和 affinity 在 [-1,1]，confidence 在 [0,1]。只评最后一条真实人类消息；affinity 是独立关系信号，绝不能由 agreement 推导。不得以提升任何指标为目标，不得在回复中复述评分或借情绪施压。";
pub fn apply(store: &Store, chat: &str, last: &Value, value: &Value, now: f64) -> Result<()> {
    if !store
        .rows(
            "SELECT message_id FROM message_ratings WHERE chat=? AND message_id=?",
            params![chat, text(last, "id")],
        )?
        .is_empty()
    {
        return Ok(());
    }
    let Some(v) = value.as_object() else {
        return Ok(());
    };
    let get = |key: &str| -> Result<f64> {
        v.get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::anyhow!("invalid_affect"))
    };
    let mood = get("mood")?;
    let agreement = get("agreement")?;
    let rationality = get("rationality")?;
    let affinity = get("affinity")?;
    let confidence = get("confidence")?;
    for n in [mood, agreement, rationality, affinity, confidence] {
        unit(n)?;
    }
    ensure!(confidence >= 0., "invalid_affect");
    let id = text(last, "id");
    let subject = format!("person:{}", text(last, "sender"));
    rate(store, chat, id, mood, agreement, confidence, now)?;
    for (d, n) in [
        (Dimension::Mood, mood),
        (Dimension::Rationality, rationality),
        (Dimension::Affinity, affinity),
    ] {
        update(store, chat, &subject, d, d, n, confidence, id, now)?;
    }
    Ok(())
}
/// 状态无 Serialize，模型只收到离散语气准则；拒绝显式复述内部评分的输出。
pub fn content_allowed(text: &str) -> bool {
    let lower = text.to_lowercase();
    ![
        "affinity",
        "agreement",
        "rationality",
        "valence",
        "心情值",
        "好感值",
        "好感度",
        "认同度",
        "情绪评分",
        "mood",
    ]
    .iter()
    .any(|s| lower.contains(s))
}

/// 不实现 Serialize：连续评分不能进入提示词、候选奖励或回复正文。
#[derive(Default)]
pub struct Behavior {
    pub mood: f64,
    pub affinity: f64,
    pub disposition: Option<Disposition>,
    pub burst: bool,
}
pub fn behavior(
    store: &Store,
    settings: &Settings,
    chat: &str,
    last: &Value,
    now: f64,
) -> Result<Behavior> {
    if !settings.enabled {
        return Ok(Behavior::default());
    }
    let subject = format!("person:{}", text(last, "sender"));
    let mood = read(store, chat, &subject, Dimension::Mood, now)?;
    let affinity = read(store, chat, &subject, Dimension::Affinity, now)?;
    let rationality = read(store, chat, &subject, Dimension::Rationality, now)?;
    let disposition = Some(disposition(mood, rationality));
    let burst =
        disposition == Some(Disposition::Angry) && burst_allowed(store, chat, text(last, "id"))?;
    Ok(Behavior {
        mood,
        affinity,
        disposition,
        burst,
    })
}
