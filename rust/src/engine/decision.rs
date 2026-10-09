//! ①基于待处理消息；②基于群状态。初筛不调用模型，③仍由原有流水线处理。
use crate::{
    config::Agent,
    engine::policy::{self, Hint},
    engine::ChatState,
    media::media_select,
    memory::num,
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
    // ①：无新消息不能回复；点名保留对主动配额/主动冷却的豁免。
    let reply = (if s.hint == Hint::SelfChat {
        common
    } else {
        proactive
    })
    .or(if s.pending {
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
