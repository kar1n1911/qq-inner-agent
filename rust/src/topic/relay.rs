//! 群外话题的纯决策模块；调用方负责提供本群未过期 short_term 与可信审核结果。
//! 不负责抓取、审核模型调用或发送；来源正文始终是不可信引用数据。
use crate::topic::{self, Item};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub mod links;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct RelaySettings {
    pub enabled: bool,
    pub allow_high_risk: bool,
    pub owner_uin: String,
    pub threshold: f64,
    pub high_threshold: f64,
    pub max_merged_messages: usize,
}
impl Default for RelaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_high_risk: false,
            owner_uin: String::new(),
            threshold: 0.15,
            high_threshold: 0.65,
            max_merged_messages: 3,
        }
    }
}
impl RelaySettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.threshold.is_finite()
                && self.threshold > 0.
                && self.threshold <= 1.
                && self.high_threshold.is_finite()
                && self.high_threshold >= self.threshold
                && self.high_threshold <= 1.
                && (2..=10).contains(&self.max_merged_messages),
            "invalid relay thresholds or message budget"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayKind {
    LowRisk,
    HighRisk,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginKind {
    External,
    ForwardedCard,
    Link,
    AgentMerged,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelfReview {
    Keep,
    Drop,
    Rewrite,
    #[default]
    Pending,
}
/// 仅允许调用方从真实群消息构造；独立性同时检查消息、作者和正文。
#[derive(Clone, Debug)]
pub struct Evidence {
    pub message_id: String,
    pub author: String,
    pub text: String,
}
#[derive(Clone, Debug)]
pub struct Origin {
    pub kind: OriginKind,
    pub item: Item,
    /// 原始来源链接或聊天记录标识；不得由生成模型补造。
    pub source: String,
    /// 群来源必须为 group:*；None 仅适用于外部来源，私聊不能参与。
    pub source_chat: Option<String>,
    pub evidence: Vec<Evidence>,
    /// 调用方从已验证的直接指令取发送者身份，不能从引用正文推导。
    pub command_sender: Option<String>,
    pub self_review: SelfReview,
    /// 调用方责任线审核确认是可分享的群级话题，不含个人身份或原话外溢。
    pub group_level: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct RelayDecision {
    pub kind: RelayKind,
    pub item: Item,
    pub source: String,
    /// 保留已有消息引用，禁止在这里生成新的合并正文。
    pub message_ids: Vec<String>,
}
fn independent_evidence(origin: &Origin) -> bool {
    let mut ids = BTreeSet::new();
    let mut authors = BTreeSet::new();
    let mut bodies = BTreeSet::new();
    let mut count = 0;
    for e in &origin.evidence {
        let body = normalize(&e.text);
        if e.message_id.trim().is_empty() || e.author.trim().is_empty() || body.is_empty() {
            continue;
        }
        if ids.contains(&e.message_id) || authors.contains(&e.author) || bodies.contains(&body) {
            continue;
        }
        ids.insert(&e.message_id);
        authors.insert(&e.author);
        bodies.insert(body);
        count += 1;
    }
    count >= 2
}
pub fn classify(
    origin: &Origin,
    interests: &BTreeSet<String>,
    cfg: &RelaySettings,
) -> Option<RelayKind> {
    if !cfg.enabled || cfg.validate().is_err() {
        return None;
    }
    let score = topic::relevance(&origin.item, interests).0;
    if origin.kind != OriginKind::AgentMerged {
        return (score >= cfg.threshold).then_some(RelayKind::LowRisk);
    }
    if !cfg.allow_high_risk
        || origin.evidence.len() < 2
        || origin.evidence.len() > cfg.max_merged_messages
    {
        return None;
    }
    let owner = !cfg.owner_uin.is_empty()
        && origin.command_sender.as_deref() == Some(cfg.owner_uin.as_str());
    (owner
        || (independent_evidence(origin)
            && score >= cfg.high_threshold
            && origin.self_review == SelfReview::Keep))
        .then_some(RelayKind::HighRisk)
}
fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap(),
            _ => c,
        })
        .filter(|c| !c.is_whitespace() && !matches!(c, '\u{200B}' | '\u{FEFF}'))
        .flat_map(char::to_lowercase)
        .collect()
}
pub fn is_duplicate(short_term_texts: &[String], url_or_text: &str) -> bool {
    let needle = normalize(url_or_text);
    !needle.is_empty()
        && short_term_texts
            .iter()
            .any(|s| normalize(s).contains(&needle))
}
/// 保守的关键词底线，不替代语义审核；命中即拒绝，owner 也不能豁免。
pub fn safety_gate(item_text: &str) -> bool {
    let text = normalize(item_text);
    ![
        "违法",
        "非法",
        "未授权",
        "未经授权",
        "无证发射",
        "绕过法规",
        "绕过无线电法规",
        "绕过安全",
        "危险操作",
        "制作炸弹",
        "自制炸药",
        "侵权",
        "unauthorized",
        "illegal",
        "transmit without a license",
        "unlicensed transmission",
        "bypass radio regulations",
        "bypass safety",
        "build a bomb",
    ]
    .iter()
    .any(|word| text.contains(&normalize(word)))
}
pub fn decide(
    cfg: &RelaySettings,
    origin: &Origin,
    interests: &BTreeSet<String>,
    short_term: &[String],
) -> Option<RelayDecision> {
    // 安全闸门比“感兴趣”更靠前：相关度不等于安全，所有风险档均须明确 keep。
    if !cfg.enabled
        || origin.self_review != SelfReview::Keep
        || !origin.group_level
        || origin.source.trim().is_empty()
    {
        return None;
    }
    match (&origin.kind, &origin.source_chat) {
        (OriginKind::External, None) => {}
        (OriginKind::External, Some(_)) => return None,
        (_, Some(chat)) if chat.starts_with("group:") && chat.len() > 6 => {}
        _ => return None,
    }
    if origin.kind == OriginKind::Link && !independent_evidence(origin) {
        return None;
    }
    let body = format!(
        "{} {} {}",
        origin.item.title,
        origin.item.description,
        origin.item.tags.join(" ")
    );
    if !safety_gate(&format!("{} {} {}", body, origin.item.url, origin.source))
        || origin.evidence.iter().any(|e| !safety_gate(&e.text))
    {
        return None;
    }
    if matches!(origin.kind, OriginKind::External | OriginKind::Link)
        && !(origin.item.url.starts_with("https://") || origin.item.url.starts_with("http://"))
    {
        return None;
    }
    if matches!(
        origin.kind,
        OriginKind::ForwardedCard | OriginKind::AgentMerged
    ) && (origin.evidence.is_empty()
        || origin
            .evidence
            .iter()
            .any(|e| e.message_id.trim().is_empty()))
    {
        return None;
    }
    if is_duplicate(short_term, &origin.item.url)
        || is_duplicate(short_term, &body)
        || is_duplicate(short_term, &origin.source)
    {
        return None;
    }
    let kind = classify(origin, interests, cfg)?;
    Some(RelayDecision {
        kind,
        item: origin.item.clone(),
        source: origin.source.clone(),
        message_ids: origin
            .evidence
            .iter()
            .map(|e| e.message_id.clone())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cfg() -> RelaySettings {
        RelaySettings {
            enabled: true,
            owner_uin: "42".into(),
            ..Default::default()
        }
    }
    fn interests() -> BTreeSet<String> {
        topic::interests("esp32", &[])
    }
    fn origin(kind: OriginKind) -> Origin {
        Origin {
            kind,
            item: Item {
                title: "esp32".into(),
                description: String::new(),
                tags: vec![],
                url: "https://example.org/esp32".into(),
            },
            source: "https://example.org/rss".into(),
            source_chat: if kind == OriginKind::External {
                None
            } else {
                Some("group:1".into())
            },
            evidence: vec![
                Evidence {
                    message_id: "1".into(),
                    author: "10".into(),
                    text: "esp32 board".into(),
                },
                Evidence {
                    message_id: "2".into(),
                    author: "20".into(),
                    text: "esp32 tools".into(),
                },
            ],
            command_sender: None,
            self_review: SelfReview::Keep,
            group_level: true,
        }
    }
    #[test]
    fn low_risk_sources_use_normal_relevance_and_preserve_source() {
        for kind in [
            OriginKind::External,
            OriginKind::ForwardedCard,
            OriginKind::Link,
        ] {
            let mut o = origin(kind);
            let d = decide(&cfg(), &o, &interests(), &[]).unwrap();
            assert_eq!(d.kind, RelayKind::LowRisk);
            assert_eq!(d.source, o.source);
            assert_eq!(d.item.url, o.item.url);
            o.item.title = "cats".into();
            assert!(decide(&cfg(), &o, &interests(), &[]).is_none());
        }
    }
    #[test]
    fn high_risk_default_off_owner_and_independent_evidence() {
        let mut o = origin(OriginKind::AgentMerged);
        assert!(decide(&cfg(), &o, &interests(), &[]).is_none());
        let c = RelaySettings {
            allow_high_risk: true,
            ..cfg()
        };
        assert!(decide(&c, &o, &interests(), &[]).is_some());
        o.evidence[1].author = "10".into();
        assert!(decide(&c, &o, &interests(), &[]).is_none());
        o.command_sender = Some("99".into());
        assert!(decide(&c, &o, &interests(), &[]).is_none());
        o.command_sender = Some("42".into());
        assert!(decide(&c, &o, &BTreeSet::new(), &[]).is_some());
        for review in [SelfReview::Drop, SelfReview::Rewrite, SelfReview::Pending] {
            o.self_review = review;
            assert!(decide(&c, &o, &interests(), &[]).is_none());
        }
    }
    #[test]
    fn high_risk_requires_distinct_content_high_score_and_bounded_references() {
        let c = RelaySettings {
            allow_high_risk: true,
            ..cfg()
        };
        let mut o = origin(OriginKind::AgentMerged);
        o.evidence[1].text = o.evidence[0].text.clone();
        assert!(decide(&c, &o, &interests(), &[]).is_none());
        o = origin(OriginKind::AgentMerged);
        o.evidence[1].message_id = o.evidence[0].message_id.clone();
        assert!(decide(&c, &o, &interests(), &[]).is_none());
        o = origin(OriginKind::AgentMerged);
        o.item.title = "esp32 cats dogs birds fish trees".into();
        assert!(topic::relevance(&o.item, &interests()).0 >= c.threshold);
        assert!(decide(&c, &o, &interests(), &[]).is_none());
        o = origin(OriginKind::AgentMerged);
        o.command_sender = Some("42".into());
        o.evidence.extend(o.evidence.clone());
        assert!(decide(&c, &o, &interests(), &[]).is_none());
    }
    #[test]
    fn dedup_url_content_and_source() {
        let o = origin(OriginKind::External);
        assert!(!is_duplicate(&[], &o.item.url));
        assert!(!is_duplicate(&["anything".into()], ""));
        for old in [&o.item.url, &o.item.title, &o.source] {
            assert!(decide(&cfg(), &o, &interests(), &[format!("previous: {old}")]).is_none());
        }
        assert!(is_duplicate(&["ＥＳＰ３２ 工具".into()], "esp32工具"));
        assert!(decide(&cfg(), &o, &interests(), &["cats".into()]).is_some());
    }
    #[test]
    fn safety_precedes_relevance_and_owner_cannot_override() {
        for bad in [
            "鼓励违法",
            "未授权无线电发射",
            "无证发射",
            "危险操作",
            "transmit without a license",
            "ＵＮＡＵＴＨＯＲＩＺＥＤ transmission",
        ] {
            assert!(!safety_gate(bad));
            let mut o = origin(OriginKind::AgentMerged);
            o.command_sender = Some("42".into());
            o.item.description = bad.into();
            assert!(decide(
                &RelaySettings {
                    allow_high_risk: true,
                    ..cfg()
                },
                &o,
                &interests(),
                &[]
            )
            .is_none());
        }
        let mut o = origin(OriginKind::External);
        o.evidence[0].text = "非法发射".into();
        assert!(decide(&cfg(), &o, &interests(), &[]).is_none());
    }
    #[test]
    fn fail_closed_privacy_provenance_review_and_defaults() {
        let mut o = origin(OriginKind::Link);
        assert!(decide(&RelaySettings::default(), &o, &interests(), &[]).is_none());
        o.source_chat = Some("private:42".into());
        assert!(decide(&cfg(), &o, &interests(), &[]).is_none());
        o.source_chat = Some("group:1".into());
        o.source.clear();
        assert!(decide(&cfg(), &o, &interests(), &[]).is_none());
        for review in [SelfReview::Pending, SelfReview::Rewrite, SelfReview::Drop] {
            let mut o = origin(OriginKind::External);
            o.self_review = review;
            assert!(decide(&cfg(), &o, &interests(), &[]).is_none());
        }
        assert!(RelaySettings::default().validate().is_ok());
        assert!(RelaySettings {
            threshold: f64::NAN,
            ..cfg()
        }
        .validate()
        .is_err());
        let mut config = crate::config::defaults();
        config["agent"]["relay"] = serde_json::json!({"enabled":true,"highThreshold":0.01});
        assert!(crate::config::validate(&config).is_err());
    }
}
