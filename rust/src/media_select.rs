//! P6b：默认关闭的素材选择；群温度与自身 activity 完全独立。
use crate::{
    conversation::{self, Classification, Relation, Stage},
    engine::policy::Message,
    media_source::{self, SourceTier},
    store::Store,
    text::{similarity, terms},
};
use anyhow::{ensure, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Drift {
    Subtle,
    Active,
    Scattered,
    Wild,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    Strict,
    Balanced,
    Loose,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reaction {
    Reserved,
    Natural,
    Lively,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Attention {
    pub enabled: bool,
    pub drift_level: Option<Drift>,
    pub anchor_policy: Option<Anchor>,
    pub reaction_style: Option<Reaction>,
}
impl Default for Attention {
    fn default() -> Self {
        Self {
            enabled: true,
            drift_level: None,
            anchor_policy: None,
            reaction_style: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub sharing: bool,
    pub p_active: f64,
    pub p_quiet: f64,
    pub quiet_cap: f64,
    pub silence_seconds: f64,
    pub mildness_threshold: f64,
    pub minimum_match: f64,
    pub attention: Attention,
    pub groups: BTreeMap<String, Attention>,
    pub classification: conversation::Config,
    pub positive_markers: Vec<String>,
    pub negative_markers: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            sharing: false,
            p_active: 0.04,
            p_quiet: 0.02,
            quiet_cap: 0.15,
            silence_seconds: 300.,
            mildness_threshold: 0.65,
            minimum_match: 0.12,
            attention: Attention::default(),
            groups: BTreeMap::new(),
            classification: conversation::Config {
                closing_markers: vec![
                    "好的".into(),
                    "收到".into(),
                    "那先这样".into(),
                    "睡了".into(),
                    "明天见".into(),
                ],
                ..Default::default()
            },
            positive_markers: vec!["可爱".into(), "喜欢".into(), "哈哈".into()],
            negative_markers: vec!["别发".into(), "烦".into(), "恶心".into(), "滚".into()],
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        for v in [
            self.p_active,
            self.p_quiet,
            self.quiet_cap,
            self.mildness_threshold,
            self.minimum_match,
        ] {
            ensure!(
                v.is_finite() && (0. ..=1.).contains(&v),
                "invalid_media_select_probability"
            );
        }
        ensure!(
            self.minimum_match > 0.
                && self.silence_seconds.is_finite()
                && self.silence_seconds > 0.,
            "invalid_media_select_threshold"
        );
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct GroupActivity {
    pub rate: f64,
    pub since_human: f64,
    pub quiet: bool,
    pub awake: bool,
    pub short_density: f64,
    pub media_rate: f64,
}
/// 仅使用该群的人类历史。UTC 小时只是稳定分桶坐标，绝不是全球 quiet 时间表。
pub fn group_activity(db: &Store, chat: &str, now: f64, silence: f64) -> Result<GroupActivity> {
    let rows = db.rows(
        "SELECT ts,text FROM messages WHERE chat=? AND self=0 AND ts<=? AND ts>=? ORDER BY ts",
        params![chat, now, now - 30. * 86400.],
    )?;
    let mut hours = [0usize; 24];
    let mut rate = 0.;
    let mut short = 0;
    for r in &rows {
        let ts = r["ts"].as_f64().unwrap_or(0.);
        let age = now - ts;
        hours[(ts / 3600.).floor().rem_euclid(24.) as usize] += 1;
        if age <= 1800. {
            rate += 0.7 * (-age / 300.).exp() / 5. + 0.3 * (-age / 1800.).exp() / 30.;
        }
        if r["text"].as_str().unwrap_or("").chars().count() <= 24 {
            short += 1;
        }
    }
    let since = rows
        .last()
        .map_or(f64::INFINITY, |r| now - r["ts"].as_f64().unwrap_or(now));
    let hour = (now / 3600.).floor().rem_euclid(24.) as usize;
    let span = rows
        .first()
        .map_or(0., |r| now - r["ts"].as_f64().unwrap_or(now));
    // 冷启动没有作息证据就不开话题；至少三天、二十条人类记录。
    let awake = rows.len() >= 20
        && span >= 3. * 86400.
        && hours[hour] >= 2
        && hours[hour] as f64 >= *hours.iter().max().unwrap() as f64 * 0.1;
    let n=db.first("SELECT count(DISTINCT r.message_id) AS n FROM media_receipts r JOIN messages m ON m.chat=r.chat AND m.id=r.message_id WHERE r.chat=? AND m.self=0 AND m.ts<=? AND m.ts>=?",params![chat,now,now-30.*86400.])?.unwrap()["n"].as_f64().unwrap_or(0.);
    Ok(GroupActivity {
        rate,
        since_human: since,
        quiet: rate < 0.5 || since >= silence * 6.,
        awake,
        short_density: short as f64 / rows.len().max(1) as f64,
        media_rate: (n / (rows.len() as f64 + 20.)).clamp(0., 1.),
    })
}
pub fn attention(
    a: &GroupActivity,
    c: &Attention,
    accepted_wild: bool,
) -> Option<(Drift, Anchor, Reaction)> {
    if !c.enabled {
        return None;
    }
    let mut drift = c.drift_level.unwrap_or(if a.quiet {
        Drift::Scattered
    } else {
        Drift::Subtle
    });
    if drift == Drift::Wild && !(a.quiet && accepted_wild) {
        drift = if a.quiet {
            Drift::Scattered
        } else {
            Drift::Active
        };
    }
    Some((
        drift,
        c.anchor_policy.unwrap_or(if a.quiet {
            Anchor::Loose
        } else {
            Anchor::Strict
        }),
        c.reaction_style.unwrap_or(if a.short_density < 0.25 {
            Reaction::Reserved
        } else if a.short_density > 0.7 {
            Reaction::Lively
        } else {
            Reaction::Natural
        }),
    ))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Positive,
    Negative,
    Silence,
    Neutral,
}
/// 信号剔除：资格是必要条件；告别收束的沉默仍是正确结果，不能反向惩罚。
/// 不确定就不学；回复本身不等于好评，负面互动优先于热闹。
pub fn signal(c: &Classification, outcome: Outcome, awake: bool) -> Option<f64> {
    if !c.confident {
        return None;
    }
    match outcome {
        Outcome::Positive => Some(1.),
        Outcome::Negative => Some(0.),
        Outcome::Silence
            if awake
                && c.stage == Stage::NaturalEnd
                && matches!(c.relation, Relation::Continuation | Relation::Shift) =>
        {
            Some(0.)
        }
        _ => None,
    }
}
pub fn share(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.;
    }
    ((a.intersection(b).count() as f64 / a.len().min(b.len()) as f64 - 0.35) / 0.4).clamp(0., 1.)
}
#[derive(Debug, Clone)]
pub struct Candidate {
    pub source_chat: String,
    pub hash: String,
    kind: String,
    file: String,
    pub score: f64,
}
impl Candidate {
    /// 三条红线通过输出形状落实：只有素材段，没有正文、at、昵称、历史 id 或旧事。
    pub fn segment(&self, root: &Path) -> Result<Value> {
        if self.kind == "face" {
            ensure!(
                !self.file.is_empty()
                    && self.file.len() <= 5
                    && self.file.bytes().all(|b| b.is_ascii_digit()),
                "invalid_face"
            );
            return Ok(json!({"type":"face","data":{"id":self.file}}));
        }
        let root = root.canonicalize()?;
        let path = root.join(&self.file).canonicalize()?;
        ensure!(
            path.starts_with(root.join("media")) && path.is_file(),
            "invalid_media_path"
        );
        Ok(json!({"type":"image","data":{"file":format!("file://{}",path.display())}}))
    }
}
pub fn enable(db: &Store) -> Result<()> {
    db.enable_media()?;
    db.connection().execute_batch("CREATE TABLE IF NOT EXISTS media_fitness(target TEXT,source TEXT,hash TEXT,bucket TEXT,positive REAL NOT NULL DEFAULT 0,total INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(target,source,hash,bucket));
    CREATE TABLE IF NOT EXISTS media_feedback(chat TEXT,event TEXT,source TEXT,hash TEXT,bucket TEXT,classification TEXT,outcome TEXT,signal REAL,PRIMARY KEY(chat,event,source,hash));
    CREATE TABLE IF NOT EXISTS media_sharing(a TEXT,b TEXT,hour INTEGER,strength REAL,summary TEXT,PRIMARY KEY(a,b));
    CREATE TABLE IF NOT EXISTS media_pending(chat TEXT,event TEXT,source TEXT,hash TEXT,bucket TEXT,ts REAL,classification TEXT,wild INTEGER DEFAULT 0,PRIMARY KEY(chat,event,source,hash));
    CREATE TABLE IF NOT EXISTS media_wild(chat TEXT PRIMARY KEY,accepted INTEGER DEFAULT 0);")?;
    Ok(())
}
pub struct Feedback<'a> {
    pub chat: &'a str,
    pub event: &'a str,
    pub source: &'a str,
    pub hash: &'a str,
    pub bucket: &'a str,
    pub classification: &'a Classification,
    pub outcome: Outcome,
    pub awake: bool,
}
pub fn learn(db: &Store, f: Feedback<'_>) -> Result<Option<f64>> {
    let tx = db.immediate()?;
    let value = signal(f.classification, f.outcome, f.awake);
    let inserted = db.execute(
        "INSERT OR IGNORE INTO media_feedback VALUES(?,?,?,?,?,?,?,?)",
        params![
            f.chat,
            f.event,
            f.source,
            f.hash,
            f.bucket,
            serde_json::to_string(f.classification)?,
            serde_json::to_string(&f.outcome)?,
            value
        ],
    )?;
    if inserted > 0 {
        if let Some(v) = value {
            db.execute("INSERT INTO media_fitness VALUES(?,?,?,?,?,1) ON CONFLICT(target,source,hash,bucket) DO UPDATE SET positive=positive+excluded.positive,total=total+1",params![f.chat,f.source,f.hash,f.bucket,v])?;
        }
    }
    tx.commit()?;
    Ok(if inserted > 0 { value } else { None })
}
pub fn fitness(db: &Store, chat: &str, source: &str, hash: &str, bucket: &str) -> Result<f64> {
    let r=db.first("SELECT positive,total FROM media_fitness WHERE target=? AND source=? AND hash=? AND bucket=?",params![chat,source,hash,bucket])?;
    // Beta(1,2) 冷启动保守；救场没有证据时不能过温度门槛。
    Ok(r.map_or(1. / 3., |r| {
        (r["positive"].as_f64().unwrap_or(0.) + 1.) / (r["total"].as_f64().unwrap_or(0.) + 3.)
    }))
}
fn features(db: &Store, chat: &str, now: f64) -> Result<HashSet<String>> {
    let mut keys = HashSet::new();
    // 只取已归纳的群级条目；不读 person、昵称、sources 或原始聊天正文。
    for r in db.rows("SELECT text AS term FROM memory_layers WHERE chat=? AND subject='group' AND layer IN ('traits','long_term') AND (expires IS NULL OR expires>?) UNION SELECT term FROM expressions WHERE chat=? AND subject='group'",params![chat,now,chat])? {keys.extend(terms(r["term"].as_str().unwrap_or("")));}
    Ok(keys)
}
fn sharing(db: &Store, a: &str, b: &str, now: f64) -> Result<(f64, String)> {
    let (a, b) = if a < b { (a, b) } else { (b, a) };
    let hour = (now / 3600.).floor() as i64;
    if let Some(r) = db.first(
        "SELECT strength,summary FROM media_sharing WHERE a=? AND b=? AND hour=?",
        params![a, b, hour],
    )? {
        return Ok((
            r["strength"].as_f64().unwrap_or(0.),
            r["summary"].as_str().unwrap_or("").into(),
        ));
    }
    let x = features(db, a, now)?;
    let y = features(db, b, now)?;
    let strength = share(&x, &y);
    let mut common: Vec<_> = x.intersection(&y).cloned().collect();
    common.sort();
    let summary = common.join(" ");
    db.execute("INSERT INTO media_sharing VALUES(?,?,?,?,?) ON CONFLICT(a,b) DO UPDATE SET hour=excluded.hour,strength=excluded.strength,summary=excluded.summary",params![a,b,hour,strength,summary])?;
    Ok((strength, summary))
}
/// 跨群来源闸门在构造候选前执行；不得先加入再靠抽签过滤。
pub fn candidates(
    db: &Store,
    chat: &str,
    query: &str,
    now: f64,
    c: &Config,
    quiet: bool,
) -> Result<Vec<Candidate>> {
    if !c.enabled {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for r in db.rows(
        "SELECT chat,hash,kind,file,source_tier FROM media_assets ORDER BY chat,hash",
        [],
    )? {
        let source = r["chat"].as_str().unwrap();
        let hash = r["hash"].as_str().unwrap();
        let tier =
            serde_json::from_value::<SourceTier>(r["source_tier"].clone()).unwrap_or_default();
        if !media_source::can_use(tier, source, chat) {
            continue;
        }
        let score = if source == chat {
            let mut best = 0f64;
            for context in db.rows("SELECT m.text FROM media_contexts c JOIN messages m ON m.chat=c.chat AND m.id=c.message_id WHERE c.chat=? AND c.hash=? AND m.self=0",params![chat,hash])? {best=best.max(similarity(query,context["text"].as_str().unwrap_or("")));}
            best
        } else {
            if !c.sharing {
                continue;
            }
            let (strength, summary) = sharing(db, source, chat, now)?;
            if strength <= 0. {
                continue;
            }
            // 跨群只带素材引用和归纳交集，不将来源聊天正文当成目标群情景。
            similarity(query, &summary) * strength
        };
        if score < c.minimum_match {
            continue;
        }
        // 温度是硬门槛：包括 wild，抽签必中也不能越过。
        if quiet && fitness(db, chat, source, hash, "quiet_rescue")? < c.mildness_threshold {
            continue;
        }
        result.push(Candidate {
            source_chat: source.into(),
            hash: hash.into(),
            kind: r["kind"].as_str().unwrap().into(),
            file: r["file"].as_str().unwrap().into(),
            score,
        });
    }
    Ok(result)
}
pub fn probability(a: &GroupActivity, stage: &Classification, c: &Config, waiting: bool) -> f64 {
    if !c.enabled
        || waiting
        || !a.awake
        || a.since_human < c.silence_seconds
        || !matches!(stage.stage, Stage::Closing | Stage::NaturalEnd)
    {
        return 0.;
    }
    let p = if a.quiet {
        (c.p_quiet * a.since_human / c.silence_seconds).min(c.quiet_cap)
    } else {
        c.p_active
    };
    p * a.media_rate * if stage.confident { 1. } else { 0.05 }
}
pub struct Selection {
    pub candidate: Candidate,
    pub classification: Classification,
    pub bucket: &'static str,
    pub wild: bool,
}
pub fn select(
    db: &Store,
    chat: &str,
    now: f64,
    c: &Config,
    draw: f64,
) -> Result<Option<Selection>> {
    if !c.enabled {
        return Ok(None);
    }
    let history = messages(db, chat)?;
    let Some(last) = history.last() else {
        return Ok(None);
    };
    if last.is_self {
        return Ok(None);
    }
    let a = group_activity(db, chat, now, c.silence_seconds)?;
    let stage = conversation::classify(&history, history.len() - 1, now, &c.classification);
    let waiting = db.expectation(chat, now)?.is_some()
        || last.hint == crate::engine::policy::Hint::SelfChat
        || last.text.trim_end().ends_with(['?', '？']);
    let p = probability(&a, &stage, c, waiting);
    if p <= 0. {
        return Ok(None);
    }
    let accepted = db
        .first("SELECT accepted FROM media_wild WHERE chat=?", [chat])?
        .is_some_and(|r| r["accepted"].as_i64().unwrap_or(0) > 0);
    let att = attention(&a, c.groups.get(chat).unwrap_or(&c.attention), accepted);
    let mut target: f64 = if a.quiet { 0.3 } else { 0.8 };
    if let Some((drift, anchor, _)) = att {
        target = match drift {
            Drift::Subtle => 0.85,
            Drift::Active => 0.65,
            Drift::Scattered => 0.3,
            Drift::Wild => 0.15,
        };
        target = match anchor {
            Anchor::Strict => target.max(0.65),
            Anchor::Balanced => target.max(0.4),
            Anchor::Loose => target,
        };
    }
    let mut items = candidates(db, chat, &last.text, now, c, a.quiet)?;
    items.sort_by(|x, y| {
        (x.score - target)
            .abs()
            .total_cmp(&(y.score - target).abs())
    });
    // 所有硬门槛先于抽签，检索无匹配绝不补一个随机素材。
    if !draw.is_finite() || draw < 0. || draw >= p {
        return Ok(None);
    }
    Ok(items.into_iter().next().map(|candidate| Selection {
        candidate,
        classification: stage,
        bucket: if a.quiet { "quiet_rescue" } else { "active" },
        wild: att.is_some_and(|(d, _, _)| d == Drift::Wild),
    }))
}
fn messages(db: &Store, chat: &str) -> Result<Vec<Message>> {
    db.history(chat, Some(100))?
        .into_iter()
        .map(|mut r| {
            r["self"] = json!(r["self"] == 1);
            r["hint"] = json!("open");
            Ok(serde_json::from_value(r)?)
        })
        .collect()
}
/// 观察窗关闭后再学习，重复 tick/重启不会重复计数；负面词优先，普通回复只记录。
pub fn observe(db: &Store, chat: &str, now: f64, c: &Config) -> Result<()> {
    if !c.enabled {
        return Ok(());
    }
    let history = messages(db, chat)?;
    let awake = group_activity(db, chat, now, c.silence_seconds)?.awake;
    for (i, m) in history.iter().enumerate() {
        if m.is_self || now - m.ts < c.classification.gap_seconds {
            continue;
        }
        let prior = history[..i].iter().rev().find(|x| !x.is_self);
        let quiet = prior.is_some_and(|p| m.ts - p.ts >= c.silence_seconds);
        // 冷清期评价插入前的场合，避免把温和的新话题误当“无关单发”扣分。
        let stage = if quiet && i > 0 {
            conversation::classify(&history[..i], i - 1, m.ts, &c.classification)
        } else {
            conversation::classify(&history, i, now, &c.classification)
        };
        for r in db.rows(
            "SELECT hash FROM media_contexts WHERE chat=? AND message_id=? AND role='usage'",
            params![chat, m.id],
        )? {
            let hash = r["hash"].as_str().unwrap();
            let learned = learn(
                db,
                Feedback {
                    chat,
                    event: &m.id,
                    source: chat,
                    hash,
                    bucket: if quiet { "quiet_rescue" } else { "active" },
                    classification: &stage,
                    outcome: outcome(&history, m.ts, c),
                    awake,
                },
            )?;
            // wild 的历史接受证据来自人类低相关话题获明确正反馈，不靠配置自我授予。
            if learned == Some(1.)
                && quiet
                && prior.is_some_and(|p| similarity(&p.text, &m.text) < 0.12)
            {
                db.execute("INSERT INTO media_wild VALUES(?,1) ON CONFLICT(chat) DO UPDATE SET accepted=accepted+1",[chat])?;
            }
        }
    }
    for r in db.rows(
        "SELECT * FROM media_pending WHERE chat=? AND ts<=?",
        params![chat, now - c.classification.gap_seconds],
    )? {
        let event = r["event"].as_str().unwrap();
        let stage: Classification =
            serde_json::from_value(serde_json::from_str(r["classification"].as_str().unwrap())?)?;
        let outcome = outcome(&history, r["ts"].as_f64().unwrap(), c);
        let learned = learn(
            db,
            Feedback {
                chat,
                event,
                source: r["source"].as_str().unwrap(),
                hash: r["hash"].as_str().unwrap(),
                bucket: r["bucket"].as_str().unwrap(),
                classification: &stage,
                outcome,
                awake,
            },
        )?;
        if learned == Some(1.) && r["wild"] == 1 {
            db.execute("INSERT INTO media_wild VALUES(?,1) ON CONFLICT(chat) DO UPDATE SET accepted=accepted+1",[chat])?;
        }
        db.execute(
            "DELETE FROM media_pending WHERE chat=? AND event=? AND source=? AND hash=?",
            params![chat, event, r["source"].as_str(), r["hash"].as_str()],
        )?;
    }
    Ok(())
}
fn outcome(history: &[Message], ts: f64, c: &Config) -> Outcome {
    let replies: Vec<_> = history
        .iter()
        .filter(|m| !m.is_self && m.ts > ts && m.ts <= ts + c.classification.gap_seconds)
        .collect();
    if replies.iter().any(|m| {
        c.negative_markers
            .iter()
            .any(|x| !x.is_empty() && m.text.contains(x))
    }) {
        Outcome::Negative
    } else if replies.iter().any(|m| {
        c.positive_markers
            .iter()
            .any(|x| !x.is_empty() && m.text.contains(x))
    }) {
        Outcome::Positive
    } else if replies.is_empty() {
        Outcome::Silence
    } else {
        Outcome::Neutral
    }
}
