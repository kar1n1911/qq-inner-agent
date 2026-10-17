//! Temporary kernel shim for the plugin-owned OneBot connection. Remove with the legacy transport path.
pub mod provider;
pub mod provider_transport;

use crate::config::{js_string, Onebot};
use crate::utils::text::js_trim;
use serde_json::{json, Value};
use tokio::sync::mpsc;

pub use crate::engine::io::adapters::onebot::connect::{ConnectConfig, Notification, OneBot, OneBotError, State};
#[cfg(test)]
use crate::engine::io::adapters::onebot::connect::backoff;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio::time::Instant;
#[cfg(test)]
use tokio_tungstenite::tungstenite::Message;

impl From<Onebot> for ConnectConfig {
    fn from(value: Onebot) -> Self {
        Self {
            url: value.url,
            self_id: value.self_id.text,
            self_id_is_truthy: value.self_id.is_truthy,
            heartbeat_seconds: value.heartbeat_seconds,
            request_timeout_seconds: value.request_timeout_seconds,
            reconnect_max_seconds: value.reconnect_max_seconds,
            max_lost_seconds: value.max_lost_seconds,
            degraded_probe_seconds: value.degraded_probe_seconds,
            forward_enabled: value.forward_enabled,
        }
    }
}

pub fn new(config: Onebot, token: String) -> (OneBot, mpsc::UnboundedReceiver<Notification>) {
    OneBot::new(ConnectConfig::from(config), token)
}

pub(crate) fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        _ => {
            let s = js_string(v);
            let s = js_trim(&s);
            if s.is_empty() {
                return 0.0;
            }
            for (lower, upper, radix) in [("0x", "0X", 16), ("0b", "0B", 2), ("0o", "0O", 8)] {
                if let Some(digits) = s.strip_prefix(lower).or_else(|| s.strip_prefix(upper)) {
                    if digits.is_empty() {
                        return f64::NAN;
                    }
                    return digits
                        .chars()
                        .try_fold(0.0, |value, c| {
                            c.to_digit(radix).map(|d| value * radix as f64 + d as f64)
                        })
                        .unwrap_or(f64::NAN);
                }
            }
            match s {
                "Infinity" | "+Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ if s
                    .bytes()
                    .any(|c| c.is_ascii_alphabetic() && c != b'e' && c != b'E') =>
                {
                    f64::NAN
                }
                _ => s.parse().unwrap_or(f64::NAN),
            }
        }
    }
}

/// Typed segments only: model text (including CQ codes) stays inert.
pub fn message_segments(
    chat: &str,
    text: &str,
    face: Option<&str>,
    reply_to: Option<&str>,
    mention: Option<&str>,
) -> Vec<Value> {
    let mut parts = Vec::new();
    if let Some(id) = reply_to {
        parts.push(json!({"type":"reply","data":{"id":id}}));
    }
    if let Some(qq) = mention.filter(|_| chat.starts_with("group:")) {
        parts.push(json!({"type":"at","data":{"qq":qq}}));
    }
    if !text.is_empty() || face.is_none() {
        parts.push(json!({"type":"text","data":{"text":text}}));
    }
    if let Some(id) = face {
        parts.push(json!({"type":"face","data":{"id":id}}));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backoff_cap_jitter_and_strict_reset() {
        let mut delay = 1.0;
        for expected in [1., 2., 4., 8., 16., 32., 60., 60.] {
            assert_eq!(
                backoff(&mut delay, Duration::from_secs(30), 60., 0.5),
                Duration::from_secs_f64(expected + 0.5)
            );
        }
        assert_eq!(
            backoff(&mut delay, Duration::from_millis(30_001), 60., 0.),
            Duration::from_secs(1)
        );
        assert_eq!(delay, 2.);
    }
    #[test]
    fn prolonged_loss_silences_failed_probes_and_zero_disables_degradation() {
        let mut config: Onebot = serde_json::from_value(crate::config::defaults()["onebot"].clone()).unwrap();
        config.max_lost_seconds = 1.0;
        let (bot, mut notices) = crate::transport::new(config.clone(), String::new());
        bot.shared.lock().unwrap().lost_since = Some(Instant::now() - Duration::from_secs(2));
        assert!(bot.state().degraded);
        bot.status("websocket_error");
        bot.status("connect_closed");
        assert!(notices.try_recv().is_err(), "degraded probes must not enqueue failure logs");
        config.max_lost_seconds = 0.0;
        let (disabled, _) = crate::transport::new(config, String::new());
        disabled.shared.lock().unwrap().lost_since = Some(Instant::now() - Duration::from_secs(2));
        assert!(!disabled.state().degraded);
    }
    #[tokio::test]
    async fn successful_handshake_restores_normal_connection_after_degradation() {
        let mut config: Onebot = serde_json::from_value(crate::config::defaults()["onebot"].clone()).unwrap();
        config.max_lost_seconds = 1.0;
        let (bot, mut notices) = crate::transport::new(config, String::new());
        bot.shared.lock().unwrap().lost_since = Some(Instant::now() - Duration::from_secs(2));
        assert!(bot.state().degraded);
        let (tx, mut outgoing) = mpsc::unbounded_channel();
        bot.shared.lock().unwrap().outgoing = Some(tx);
        let responder = bot.clone();
        let task = tokio::spawn(async move {
            for _ in 0..2 {
                let Message::Text(frame) = outgoing.recv().await.unwrap() else { panic!("expected request") };
                let request: Value = serde_json::from_str(&frame).unwrap();
                let data = if request["action"] == "get_login_info" { json!({"user_id":99}) } else { json!({"online":true}) };
                responder.receive(Message::Text(json!({"echo":request["echo"],"status":"ok","retcode":0,"data":data}).to_string()));
            }
        });
        bot.handshake().await.unwrap();
        task.await.unwrap();
        assert!(bot.state().online);
        assert!(!bot.state().degraded);
        assert!(matches!(notices.try_recv(), Ok(Notification::Status(code)) if code == "connected"));
    }
    #[tokio::test]
    async fn send_failure_and_cancellation_clean_pending() {
        let c = serde_json::from_value(crate::config::defaults()["onebot"].clone()).unwrap();
        let (bot, _) = crate::transport::new(c, String::new());
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        bot.shared.lock().unwrap().outgoing = Some(tx);
        let e = bot.call("test", json!({})).await.unwrap_err();
        assert_eq!(e.code, "send_failed");
        assert!(e.uncertain);
        assert!(bot.shared.lock().unwrap().pending.is_empty());
        let (tx, mut rx) = mpsc::unbounded_channel();
        bot.shared.lock().unwrap().outgoing = Some(tx);
        let b = bot.clone();
        let task = tokio::spawn(async move { b.call("cancel", json!({})).await });
        rx.recv().await.unwrap();
        task.abort();
        let _ = task.await;
        assert!(bot.shared.lock().unwrap().pending.is_empty());
    }
}
