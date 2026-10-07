//! 只记录结构判断，不把沉默转换成学习信号。
use crate::{engine::policy::Message, text::similarity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub closing_markers: Vec<String>,
    pub gap_seconds: f64,
    pub overlap: f64,
    pub min_turns: usize,
    pub short_chars: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            closing_markers: Vec::new(),
            gap_seconds: 120.,
            overlap: 0.25,
            min_turns: 2,
            short_chars: 24,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Developing,
    Closing,
    NaturalEnd,
    Standalone,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    Continuation,
    Shift,
    Unrelated,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Classification {
    pub stage: Stage,
    pub relation: Relation,
    pub confident: bool,
    pub evidence: Evidence,
}
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Evidence {
    pub previous_similarity: f64,
    pub next_similarity: f64,
    pub alternating_turns: usize,
    pub silent_seconds: f64,
    pub closing_marker: bool,
}
/// target 指待分类消息；后续消息和观察时间用于反推，刚到的消息不假装已有后续证据。
pub fn classify(messages: &[Message], target: usize, now: f64, c: &Config) -> Classification {
    let mut r = Classification {
        stage: Stage::NaturalEnd,
        relation: Relation::Unrelated,
        confident: false,
        evidence: Evidence::default(),
    };
    let Some(m) = messages.get(target) else {
        return r;
    };
    if !now.is_finite()
        || c.gap_seconds <= 0.
        || !c.gap_seconds.is_finite()
        || !(0. ..=1.).contains(&c.overlap)
        || c.overlap == 0.
        || c.min_turns < 2
        || messages
            .iter()
            .any(|x| x.chat != m.chat || !x.ts.is_finite() || x.ts > now)
        || messages.windows(2).any(|w| w[0].ts > w[1].ts)
    {
        return r;
    }
    let previous = target.checked_sub(1).and_then(|i| messages.get(i));
    let next = messages.get(target + 1);
    let prev = previous.map_or(0., |p| similarity(&p.text, &m.text));
    let after = next.map_or(0., |p| similarity(&p.text, &m.text));
    let linked = previous.is_some_and(|p| m.ts - p.ts < c.gap_seconds) && prev >= c.overlap;
    let resumed = messages[target + 1..]
        .iter()
        .any(|p| similarity(&p.text, &m.text) >= c.overlap);
    let observed = now - m.ts >= c.gap_seconds;
    let marker = m.text.chars().count() <= c.short_chars
        && c.closing_markers
            .iter()
            .any(|s| !s.is_empty() && m.text.contains(s));
    let mut turns = 0;
    for pair in messages[..=target].windows(2).rev() {
        if pair[1].ts - pair[0].ts >= c.gap_seconds
            || similarity(&pair[0].text, &pair[1].text) < c.overlap
        {
            break;
        }
        if pair[0].sender != pair[1].sender {
            turns += 1;
        }
    }
    r.evidence = Evidence {
        previous_similarity: prev,
        next_similarity: after,
        alternating_turns: turns,
        silent_seconds: now - m.ts,
        closing_marker: marker,
    };
    r.relation = if linked {
        Relation::Continuation
    } else if prev > 0. {
        Relation::Shift
    } else {
        Relation::Unrelated
    };
    if marker && observed && !resumed {
        r.stage = Stage::Closing;
        r.confident = linked;
    } else if turns >= c.min_turns && !observed {
        r.stage = Stage::Developing;
        r.confident = true;
    } else if observed && !resumed && prev == 0. && after == 0. && !m.text.is_empty() {
        r.stage = Stage::Standalone;
        r.confident = previous.is_some() && next.is_some();
    }
    // 自然终止、转向和缺乏前后证据都是模糊态：必须如实返回 confident:false，不能猜。
    if r.relation == Relation::Shift {
        r.confident = false;
    }
    r
}
