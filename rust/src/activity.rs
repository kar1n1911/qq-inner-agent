//! 对应 `src/activity.mjs` 的 `activityProbability`。
//!
//! 这是一条**有界的高斯形「静默」曲线，而不是归一化的概率密度**：正中间最可能休息，
//! 越靠近静默窗口两端越可能活跃。活动块复用既有 Store 表持久化。
//!
//! 曲线按**静默区间**定位：从 `inactiveStart` 到 `activeStart`（跨午夜时回绕）。
//! 注意 JS 用的本地时间含**秒**（`hour*60 + minute + second/60`），所以这里不能用
//! 只看分钟的换算。
use crate::config::{Rhythm, Schedule};
use crate::policy::{active_at, local_minute_of_day, parse_hhmm};

/// 复刻 `activityProbability(now, schedule, rhythm)`。
pub fn activity_probability(now: f64, schedule: &Schedule, rhythm: &Rhythm) -> f64 {
    // 时间表未启用、或此刻本来就在活跃窗口内 —— 两者都直接返回白天的基准概率。
    if !schedule.enabled || active_at(now, schedule) {
        return rhythm.day_probability;
    }
    let Some(minute) = local_minute_of_day(now, &schedule.timezone) else {
        return rhythm.day_probability;
    };
    let (Some(inactive_start), Some(active_start)) = (
        parse_hhmm(&schedule.inactive_start),
        parse_hhmm(&schedule.active_start),
    ) else {
        return rhythm.day_probability;
    };
    let start = inactive_start as f64;
    let end = active_start as f64;
    let duration = (end - start + 1440.0) % 1440.0;
    let x = ((minute - start + 1440.0) % 1440.0) / duration;

    let sigma = rhythm.sigma;
    let edge = (-0.5 * (0.5 / sigma).powi(2)).exp();
    let gaussian = (-0.5 * ((x - 0.5) / sigma).powi(2)).exp();
    // JS 写的是 Math.max(0, Math.min(1, ...))，这里用 clamp；两者对 NaN 的行为一致（都得到 NaN）。
    let dip = ((gaussian - edge) / (1.0 - edge)).clamp(0.0, 1.0);
    rhythm.edge_probability - (rhythm.edge_probability - rhythm.center_probability) * dip
}

use crate::store::{ActivityRow, Store};
use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySnapshot {
    pub enabled: bool,
    pub active: bool,
    pub started: Option<f64>,
    pub until: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probability: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_probability: Option<f64>,
}

/// 时钟由 snapshot 参数注入；随机源必须显式传入，测试不依赖真实时间/随机。
pub struct ActivityRhythm<'a, R> {
    store: &'a Store,
    schedule: &'a Schedule,
    rhythm: &'a Rhythm,
    signature: String,
    random: R,
}
impl<'a, R: FnMut() -> f64> ActivityRhythm<'a, R> {
    pub fn new(store: &'a Store, schedule: &'a Schedule, rhythm: &'a Rhythm, random: R) -> Self {
        // 按 JS defaults 的插入顺序编码，并去掉整数浮点的 .0，兼容共库重启。
        fn object(value: serde_json::Value, keys: &[&str]) -> String {
            format!(
                "{{{}}}",
                keys.iter()
                    .map(|k| {
                        let v = &value[*k];
                        let encoded = if v.is_number() {
                            crate::config::js_string(v)
                        } else {
                            v.to_string()
                        };
                        format!("\"{k}\":{encoded}")
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        let s = object(
            serde_json::to_value(schedule).unwrap(),
            &["enabled", "activeStart", "inactiveStart", "timezone"],
        );
        let r = object(
            serde_json::to_value(rhythm).unwrap(),
            &[
                "enabled",
                "dayProbability",
                "edgeProbability",
                "centerProbability",
                "sigma",
                "activeMinSeconds",
                "activeMaxSeconds",
                "restMinSeconds",
                "restMaxSeconds",
            ],
        );
        Self {
            store,
            schedule,
            rhythm,
            signature: format!("[{s},{r}]"),
            random,
        }
    }
    pub fn snapshot(&mut self, now: f64) -> Result<ActivitySnapshot> {
        if !self.rhythm.enabled {
            return Ok(ActivitySnapshot {
                enabled: false,
                active: active_at(now, self.schedule),
                started: None,
                until: None,
                probability: None,
                draw: None,
                current_probability: None,
            });
        }
        let old = self.store.activity_state()?;
        // 配置签名变化、时钟回拨、到期均重抽；不强制交替，相邻活跃块可连续。
        let row = match old {
            Some(row)
                if row.signature == self.signature && now >= row.started && now < row.until =>
            {
                row
            }
            _ => {
                // 本地静默进度与 DST 均复用已有 Zone；不把当地一天假定成固定 UTC 日。
                let probability = activity_probability(now, self.schedule, self.rhythm);
                let draw = (self.random)();
                let active = draw < probability;
                let (min, max) = if active {
                    (
                        self.rhythm.active_min_seconds,
                        self.rhythm.active_max_seconds,
                    )
                } else {
                    (self.rhythm.rest_min_seconds, self.rhythm.rest_max_seconds)
                };
                let row = ActivityRow {
                    signature: self.signature.clone(),
                    started: now,
                    until: now + min + ((self.random)() * (max - min + 1.)).floor(),
                    active: i64::from(active),
                    probability,
                    draw,
                };
                self.store.save_activity(&row)?;
                row
            }
        };
        Ok(ActivitySnapshot {
            enabled: true,
            active: row.active != 0,
            started: Some(row.started),
            until: Some(row.until),
            probability: Some(row.probability),
            draw: Some(row.draw),
            current_probability: Some(activity_probability(now, self.schedule, self.rhythm)),
        })
    }
}
