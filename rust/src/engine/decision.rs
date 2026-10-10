//! ①基于待处理消息；②基于群状态。初筛不调用模型，③仍由原有流水线处理。
use crate::{
    config::Agent, engine::policy, engine::ChatState, media::media_select, memory::num,
    store::Store,
};
use anyhow::Result;

pub struct Screen {
    pub reply: Option<&'static str>,
    pub topic: Option<&'static str>,
    pub probability: f64,
}

/// 集中列出确定性闸门；每层独立得出阻断原因，调度器只负责回复优先。
pub fn screen(db: &Store, chat: &str, s: &ChatState, a: &Agent, now: f64) -> Result<Screen> {
    screen_inner(db, chat, s, a, now, true)
}

/// 已获 tick 准入的同版本任务复核其余闸门，不被自身刚设置的冷却拦截。
pub(super) fn recheck(
    db: &Store,
    chat: &str,
    s: &ChatState,
    a: &Agent,
    now: f64,
) -> Result<Screen> {
    screen_inner(db, chat, s, a, now, false)
}

fn screen_inner(
    db: &Store,
    chat: &str,
    s: &ChatState,
    a: &Agent,
    now: f64,
    cooldown: bool,
) -> Result<Screen> {
    let counts = db.counts(chat, now)?;
    let common = if policy::quiet(now, a.quiet_hours.as_ref()) {
        // 静默
        Some("quiet")
    } else if num(&counts, "total") >= a.max_messages_per_hour {
        // 总配额
        Some("quota")
    } else if cooldown && now - s.last_think < a.min_think_interval_seconds {
        // 思考冷却
        Some("cooldown")
    } else {
        None
    };
    let proactive = common.or_else(|| {
        if !a.proactive {
            Some("disabled")
        } else if num(&counts, "proactive") >= a.max_proactive_per_hour {
            Some("proactive_quota")
        } else if now - num(&counts, "last") < a.proactive_cooldown_seconds {
            Some("proactive_cooldown")
        } else {
            None
        }
    });
    // ①：所有消息回复共用回复闸门，不消耗主动冷却/配额。
    let reply = common.or(if s.pending {
        None
    } else {
        Some("no_new_message")
    });
    // ②：不用 pending 推导资格；未消费消息及 expectation 属于回应问题。
    let mut topic = proactive.or(if chat.starts_with("group:") {
        None
    } else {
        Some("not_group")
    });
    let mut probability = 0.;
    if topic.is_none() {
        // 想法池为空：不让形成模型凭空为一次主动尝试制造理由。
        if db
            .reservoir(
                chat,
                now,
                a.thought_ttl_seconds,
                a.thought_limit as i64,
                None,
            )?
            .is_empty()
            && !a.topic_source.enabled()
            && !a.relay.enabled
        {
            topic = Some("empty_thoughts");
        } else {
            // 复用群温度与该群作息，包括证据不足时保守关闭的冷启动策略。
            let activity = media_select::group_activity(db, chat, now, a.pause_seconds.max(1.))?;
            if !activity.awake {
                topic = Some("outside_group_schedule");
            } else if !activity.quiet || activity.since_human < a.pause_seconds {
                topic = Some("group_active");
            } else if db.expectation(chat, now)?.is_some() {
                topic = Some("expectation");
            } else if db.handled(chat)?.is_none_or(|h| h["human_id"] != s.last_id) {
                topic = Some("unanswered_message");
            } else {
                probability =
                    topic_probability(&activity, now - num(&counts, "last"), a.pause_seconds);
            }
        }
    }
    Ok(Screen {
        reply,
        topic,
        probability,
    })
}

/// 启发式只承诺区间和方向：沉默更久/距自身发言更久，概率不下降；上限保守。
pub fn topic_probability(g: &media_select::GroupActivity, since_self: f64, pause: f64) -> f64 {
    if !g.awake || !g.quiet || g.since_human < pause {
        return 0.;
    }
    let scale = pause.max(60.);
    0.15 * (g.since_human / (scale * 3.)).clamp(0., 1.) * (since_self / scale).clamp(0., 1.)
}

/// Delivery tiers, not persisted conversation states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicTier {
    Recent,
    Ended,
    Remote,
}

pub fn topic_tier(age: f64, c: &crate::config::TopicLifecycle) -> TopicTier {
    if age <= c.recent_seconds {
        TopicTier::Recent
    } else if age <= c.remote_seconds {
        TopicTier::Ended
    } else {
        TopicTier::Remote
    }
}

#[derive(Debug)]
pub struct TopicDelivery {
    pub tier: TopicTier,
    pub target: Option<String>,
    pub last_activity: Option<f64>,
    pub stage: Option<crate::persona::conversation::Stage>,
    pub allowed: bool,
}

/// The selected thought is evidence, not a clock reset. Resolve the response object
/// first, then find the last activity related to that object in this chat.
pub fn topic_delivery(
    db: &Store,
    chat: &str,
    candidate: &policy::Candidate,
    response: &serde_json::Value,
    now: f64,
    settings: &crate::config::TopicLifecycle,
    classification: &crate::persona::conversation::Config,
) -> Result<TopicDelivery> {
    use crate::{
        memory::{text, text::similarity},
        persona::conversation::{self, Stage},
    };
    let rows = db.rows(
        "SELECT * FROM messages WHERE chat=? ORDER BY ts,rowid",
        [chat],
    )?;
    let messages: Vec<policy::Message> = rows
        .iter()
        .map(|r| policy::Message {
            chat: chat.into(),
            id: text(r, "id").into(),
            sender: text(r, "sender").into(),
            name: text(r, "name").into(),
            text: text(r, "text").into(),
            ts: num(r, "ts"),
            is_self: r["self"] == 1,
            hint: policy::Hint::Open,
        })
        .collect();
    // Same validity window as targeting::validate; a valid model target wins.
    let explicit = response["replyTo"].as_str().and_then(|id| {
        messages
            .iter()
            .enumerate()
            .rev()
            .take(100)
            .find(|(_, m)| m.id == id)
            .map(|(i, _)| i)
    });
    let find_target = |content: &str| {
        let mut matches: Vec<_> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.is_self)
            .map(|(i, m)| (i, similarity(content, &m.text)))
            .filter(|(_, score)| *score >= classification.overlap)
            .collect();
        matches.sort_by(|a, b| b.1.total_cmp(&a.1));
        // Ambiguous equal matches do not establish an exact response object.
        matches
            .first()
            .map(|best| (best.0, matches.get(1).is_none_or(|next| best.1 > next.1)))
    };
    let anchor = explicit
        .map(|i| (i, true))
        .or_else(|| find_target(text(response, "text")))
        .or_else(|| find_target(&candidate.text));
    let Some((anchor, precise)) = anchor else {
        let thought = db.rows(
            "SELECT created FROM thoughts WHERE chat=? AND id=?",
            [chat, &candidate.id],
        )?;
        let age = thought
            .first()
            .map_or(f64::INFINITY, |r| now - num(r, "created"));
        // Unrelated fresh ideas are allowed; retained ungrounded ideas fail closed.
        let tier = topic_tier(age, settings);
        return Ok(TopicDelivery {
            tier,
            target: None,
            last_activity: None,
            stage: None,
            allowed: tier == TopicTier::Recent,
        });
    };
    let related: Vec<_> = messages
        .iter()
        .enumerate()
        .filter(|(i, m)| {
            *i == anchor || similarity(&messages[anchor].text, &m.text) >= classification.overlap
        })
        .collect();
    let last = related.last().unwrap().0;
    let last_activity = messages[last].ts;
    let age = (now - last_activity).max(0.);
    let mut classification = classification.clone();
    // A configurable short recent window also shortens the observation horizon.
    classification.gap_seconds = classification.gap_seconds.min(settings.recent_seconds);
    let stage = conversation::classify(&messages, last, now, &classification).stage;
    let tier = topic_tier(age, settings);
    // Reuse Stage; stale Standalone also needs a quote, never silently becomes live.
    let ended = matches!(
        stage,
        Stage::Closing | Stage::NaturalEnd | Stage::Standalone
    );
    let necessary = candidate.relevance >= 4.
        && candidate.originality >= 4.
        && ["urgency", "information_gap"]
            .iter()
            .any(|tag| candidate.for_tags.iter().any(|t| t == tag))
        && !candidate.against_tags.iter().any(|t| {
            matches!(
                t.as_str(),
                "relevance" | "coherence" | "expected_impact" | "urgency"
            )
        });
    let quotable = anchor >= messages.len().saturating_sub(100);
    Ok(TopicDelivery {
        tier,
        target: precise.then(|| messages[anchor].id.clone()),
        last_activity: Some(last_activity),
        stage: Some(stage),
        allowed: tier == TopicTier::Recent
            || (ended && precise && quotable && (tier != TopicTier::Remote || necessary)),
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::{config::TopicLifecycle, persona::conversation};
    use serde_json::json;

    #[test]
    fn tiers_include_exact_thresholds_and_accept_configuration() {
        // Design: elapsed time, inclusive upper bounds; no second state machine.
        for c in [
            TopicLifecycle::default(),
            TopicLifecycle {
                recent_seconds: 10.,
                remote_seconds: 20.,
            },
        ] {
            assert_eq!(topic_tier(0., &c), TopicTier::Recent);
            assert_eq!(topic_tier(c.recent_seconds, &c), TopicTier::Recent);
            assert_eq!(topic_tier(c.recent_seconds + 0.001, &c), TopicTier::Ended);
            assert_eq!(topic_tier(c.remote_seconds, &c), TopicTier::Ended);
            assert_eq!(topic_tier(c.remote_seconds + 0.001, &c), TopicTier::Remote);
        }
        for value in [
            json!({"recentSeconds":0}),
            json!({"recentSeconds":1800}),
            json!({"remoteSeconds":-1}),
        ] {
            let c = crate::config::merge(
                &crate::config::defaults(),
                &json!({"agent":{"topicLifecycle":value}}),
            );
            assert!(crate::config::validate(&c).is_err());
        }
    }

    #[test]
    fn topic_clock_uses_related_message_not_new_thought_or_unrelated_chat_activity() -> Result<()> {
        let db = Store::in_memory()?;
        for (id, content, ts) in [
            ("a", "汉堡 好吃", 0.),
            ("b", "汉堡 好吃 再见", 10.),
            ("other", "完全无关", 2000.),
        ] {
            db.message(&json!({"chat":"group:1","id":id,"text":content,"sender":"20","ts":ts,"self":false}))?;
        }
        let c = policy::Candidate {
            id: "c".into(),
            kind: policy::CandidateKind::System2,
            text: "汉堡 好吃".into(),
            motivation: 5.,
            relevance: 5.,
            originality: 5.,
            for_tags: vec![],
            against_tags: vec![],
        };
        let result = topic_delivery(
            &db,
            "group:1",
            &c,
            &json!({}),
            2000.,
            &Default::default(),
            &conversation::Config::default(),
        )?;
        assert_eq!(result.last_activity, Some(10.));
        assert_eq!(result.target.as_deref(), Some("a"));
        assert_eq!(result.tier, TopicTier::Remote);
        assert!(!result.allowed); // Motivation alone cannot revive a remote topic.
        assert_eq!(result.stage, Some(conversation::Stage::NaturalEnd));
        let closing = conversation::Config {
            closing_markers: vec!["再见".into()],
            ..Default::default()
        };
        let result = topic_delivery(
            &db,
            "group:1",
            &c,
            &json!({}),
            2000.,
            &Default::default(),
            &closing,
        )?;
        assert_eq!(result.stage, Some(conversation::Stage::Closing));
        assert!(!result.allowed);
        let mut necessary = c.clone();
        necessary.for_tags = vec!["urgency".into()];
        assert!(
            topic_delivery(
                &db,
                "group:1",
                &necessary,
                &json!({}),
                2000.,
                &Default::default(),
                &closing
            )?
            .allowed
        );
        necessary.against_tags = vec!["expected_impact".into()];
        assert!(
            !topic_delivery(
                &db,
                "group:1",
                &necessary,
                &json!({}),
                2000.,
                &Default::default(),
                &closing
            )?
            .allowed
        );
        Ok(())
    }
    #[test]
    fn stale_target_must_be_unambiguous_scoped_and_quotable() -> Result<()> {
        // Design: no guessed @ fallback, and the same recent-100 scope as targeting.
        let db = Store::in_memory()?;
        let thought =
            db.add_thought("group:1", &json!({"text":"汉堡 好吃","kind":"system2"}), 0.)?;
        let c = policy::Candidate {
            id: thought["id"].as_str().unwrap().into(),
            kind: policy::CandidateKind::System2,
            text: "汉堡 好吃".into(),
            motivation: 5.,
            relevance: 5.,
            originality: 5.,
            for_tags: vec!["information_gap".into()],
            against_tags: vec![],
        };
        let check = |response| {
            topic_delivery(
                &db,
                "group:1",
                &c,
                &response,
                2000.,
                &Default::default(),
                &conversation::Config::default(),
            )
        };
        assert!(!check(json!({}))?.allowed); // Retained, ungrounded candidate.
        for (chat, id, ts) in [
            ("group:1", "a", 0.),
            ("group:1", "b", 1.),
            ("group:2", "foreign", 1999.),
        ] {
            db.message(
                &json!({"chat":chat,"id":id,"sender":"20","text":"汉堡 好吃","ts":ts,"self":false}),
            )?;
        }
        assert!(!check(json!({"replyTo":"foreign"}))?.allowed); // Equal targets are ambiguous.
        let exact = check(json!({"replyTo":"a"}))?;
        assert!(exact.allowed);
        assert_eq!(exact.target.as_deref(), Some("a"));
        for i in 0..100 {
            db.message(&json!({"chat":"group:1","id":format!("new{i}"),"sender":"21","text":"天气晴朗","ts":1900.,"self":false}))?;
        }
        assert!(!check(json!({"replyTo":"a"}))?.allowed);
        db.message(&json!({"chat":"group:1","id":"resumed","sender":"20","text":"汉堡 好吃","ts":1999.,"self":false}))?;
        let resumed = check(json!({"replyTo":"resumed"}))?;
        assert_eq!(resumed.last_activity, Some(1999.));
        assert_eq!(resumed.tier, TopicTier::Recent);
        assert!(resumed.allowed);
        Ok(())
    }
}
