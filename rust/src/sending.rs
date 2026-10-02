//! 对应 `src/sending.mjs`：发送预测的校验与准入概率。
//!
//! 这里是纯函数：不碰存储、网络或时钟，因此可以与 JS 逐值比对。
//! 浮点乘法不满足结合律，**因子相乘的顺序必须与 JS 的 `Object.values` 插入顺序一致**，
//! 否则可能出现最后一位的差异。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// 与 JS 抛出的 `invalid_forecast` 逐字一致 —— 错误码是日志与前端可见的契约。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ForecastError {
    #[error("invalid_forecast")]
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseMode {
    Answer,
    Ask,
    Acknowledge,
    Wait,
}

impl ResponseMode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "answer" => Some(Self::Answer),
            "ask" => Some(Self::Ask),
            "acknowledge" => Some(Self::Acknowledge),
            "wait" => Some(Self::Wait),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Outcomes {
    pub reply: f64,
    pub silence: f64,
    pub negative: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Forecast {
    #[serde(rename = "shouldSend")]
    pub should_send: bool,
    pub outcomes: Outcomes,
    #[serde(rename = "responseMode")]
    pub response_mode: ResponseMode,
    pub plan: String,
}

/// JS 的 `unit()`：必须是有限数且落在 `[0, 1]`。
///
/// `serde_json` 不会产出 NaN 或 Infinity，但保留 `is_finite` 以对齐语义。
fn unit(n: f64) -> bool {
    n.is_finite() && (0.0..=1.0).contains(&n)
}

/// 取数值；布尔、字符串、null 都不是数字，与 JS 的 `typeof n === 'number'` 一致。
fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// 复刻 `forecastResult`。任何一条不满足都返回 `invalid_forecast`。
pub fn forecast_result(value: &Value) -> Result<Forecast, ForecastError> {
    let (Some(should_send), Some(outcomes), Some(plan)) = (
        value.get("shouldSend").and_then(Value::as_bool),
        value.get("outcomes"),
        value.get("plan").and_then(Value::as_str),
    ) else {
        return Err(ForecastError::Invalid);
    };
    let Some(mode) = value
        .get("responseMode")
        .and_then(Value::as_str)
        .and_then(ResponseMode::parse)
    else {
        return Err(ForecastError::Invalid);
    };
    let (Some(reply), Some(silence), Some(negative)) = (
        number(outcomes.get("reply")),
        number(outcomes.get("silence")),
        number(outcomes.get("negative")),
    ) else {
        return Err(ForecastError::Invalid);
    };
    if !unit(reply) || !unit(silence) || !unit(negative) {
        return Err(ForecastError::Invalid);
    }
    // JS 允许 2% 的舍入余量：abs(sum - 1) > 0.02 才算非法。
    if (reply + silence + negative - 1.0).abs() > 0.02 {
        return Err(ForecastError::Invalid);
    }
    // responseMode 为 wait 时不允许同时要求发送。
    if mode == ResponseMode::Wait && should_send {
        return Err(ForecastError::Invalid);
    }
    let trimmed = plan.trim();
    if trimmed.is_empty() {
        return Err(ForecastError::Invalid);
    }
    // 注意：JS 检查的是 **未 trim 的** value.plan.length，且这是 UTF-16 码元数。
    // 这里同样对原始 plan 计数，并按 UTF-16 计数以匹配 JS 的 .length。
    if plan.encode_utf16().count() > 400 {
        return Err(ForecastError::Invalid);
    }
    Ok(Forecast {
        should_send,
        outcomes: Outcomes {
            reply,
            silence,
            negative,
        },
        response_mode: mode,
        plan: trimmed.to_string(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SendingSettings {
    #[serde(rename = "proactiveProbability")]
    pub proactive_probability: f64,
    #[serde(rename = "addressedProbability")]
    pub addressed_probability: f64,
    #[serde(rename = "settleSeconds")]
    pub settle_seconds: f64,
    #[serde(rename = "recoverySeconds")]
    pub recovery_seconds: f64,
    #[serde(rename = "burstScale")]
    pub burst_scale: f64,
    #[serde(rename = "maxNegativeProbability")]
    pub max_negative_probability: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    pub proactive: bool,
    pub age: f64,
    pub gap: f64,
    #[serde(rename = "recentHumans")]
    pub recent_humans: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Veto {
    #[serde(rename = "forecast_withhold")]
    ForecastWithhold,
    #[serde(rename = "forecast_risk")]
    ForecastRisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Factors {
    pub base: f64,
    pub settle: f64,
    pub recovery: f64,
    pub pace: f64,
    pub motivation: f64,
    pub forecast: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Admission {
    pub factors: Factors,
    pub probability: f64,
    pub veto: Option<Veto>,
}

/// 复刻 `sendingProbability`。
///
/// 注意两处容易搞错的地方：
/// * 非主动回复只把 `base` 换成 `addressedProbability`，其余四个因子**置 1**；
/// * `veto` 与 `proactive` **无关** —— 直接提问同样会被预测否决。
///
/// 与 JS 的已知差异：JS 的 `Math.max(0, NaN)` 得到 NaN，而 Rust 的 `f64::max` 会忽略
/// NaN 返回另一个操作数。`age`/`gap` 由内部计时器产生，实际不会是 NaN。
pub fn sending_probability(
    settings: &SendingSettings,
    timing: &Timing,
    forecast: &Forecast,
) -> Admission {
    let proactive = timing.proactive;
    let factors = Factors {
        base: if proactive {
            settings.proactive_probability
        } else {
            settings.addressed_probability
        },
        settle: if proactive {
            (timing.age.max(0.0) / settings.settle_seconds).min(1.0)
        } else {
            1.0
        },
        recovery: if proactive {
            (timing.gap.max(0.0) / settings.recovery_seconds).min(1.0)
        } else {
            1.0
        },
        pace: if proactive {
            1.0 / (1.0 + timing.recent_humans / settings.burst_scale)
        } else {
            1.0
        },
        motivation: if proactive {
            0.25 + 0.75 * (timing.score.clamp(1.0, 5.0) - 1.0) / 4.0
        } else {
            1.0
        },
        forecast: if proactive {
            1.0 - forecast.outcomes.negative
        } else {
            1.0
        },
    };
    let veto = if !forecast.should_send {
        Some(Veto::ForecastWithhold)
    } else if forecast.outcomes.negative > settings.max_negative_probability {
        Some(Veto::ForecastRisk)
    } else {
        None
    };
    let probability = if veto.is_some() {
        0.0
    } else {
        // 顺序与 JS 的对象插入顺序一致：base, settle, recovery, pace, motivation, forecast。
        factors.base
            * factors.settle
            * factors.recovery
            * factors.pace
            * factors.motivation
            * factors.forecast
    };
    Admission {
        factors,
        probability,
        veto,
    }
}
