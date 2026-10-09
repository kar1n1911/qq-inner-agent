//! Gateways 路由与通知合并的离线单测：不访问真实 QQ/Telegram 网络。
use super::*;
use crate::config::{self, Config};
use crate::engine::EngineTransport;
use crate::transport::Notification;
use serde_json::json;
use std::sync::Arc;

fn config_with(extra: Value) -> Config {
    let value = config::merge(
        &config::defaults(),
        &config::merge(
            &json!({"apiKey":"","onebotToken":"","telegramToken":"TEST:TOKEN","dataDir":"unused"}),
            &extra,
        ),
    );
    Config::from_value(&value).unwrap()
}

fn gateways(extra: Value) -> (Arc<Gateways>, mpsc::UnboundedReceiver<GatewayNotice>) {
    let config = config_with(extra);
    let dir = std::env::temp_dir().join(format!(
        "qq-gateway-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    Gateways::new(&config, dir, Arc::new(|_, _| {}))
}

fn telegram_extra() -> Value {
    json!({
        "onebot": {"selfId": "111"},
        "telegram": {"enabled": true, "allowedGroups": ["-100"], "allowedUsers": ["7"]},
    })
}

#[tokio::test]
async fn routes_send_and_media_by_chat_owner() {
    let (gw, _rx) = gateways(telegram_extra());
    assert!(gw.telegram().is_some());
    // Telegram 归属：未连接时返回 telegram_offline，绝不落到 OneBot。
    assert_eq!(
        gw.send("group:-100", "hi", None).await.unwrap_err().code,
        "telegram_offline"
    );
    assert_eq!(
        gw.send("private:7", "hi", None).await.unwrap_err().code,
        "telegram_offline"
    );
    // OneBot 归属保持原样。
    assert_eq!(
        gw.send("group:123", "hi", None).await.unwrap_err().code,
        "qq_offline"
    );
    assert_eq!(
        gw.send("private:8", "hi", None).await.unwrap_err().code,
        "qq_offline"
    );
    // 媒体在 Telegram 侧一期不可用。
    assert_eq!(
        gw.send_media("group:-100", json!({"type":"face","data":{"id":"1"}}))
            .await
            .unwrap_err()
            .code,
        "telegram_media_unsupported"
    );
}

#[tokio::test]
async fn routes_calls_by_action_family_without_network() {
    let (gw, _rx) = gateways(telegram_extra());
    // Telegram 群 action 归 Telegram；unsupported_action 在发请求前返回，测试不需要网络。
    assert_eq!(
        gw.call("get_group_msg_history", json!({"group_id": -100}))
            .await
            .unwrap_err()
            .to_string(),
        "unsupported_action"
    );
    assert_eq!(
        gw.call("get_group_member_list", json!({"group_id": "-100"}))
            .await
            .unwrap_err()
            .to_string(),
        "unsupported_action"
    );
    // OneBot 群 action 归 OneBot。
    assert_eq!(
        gw.call("get_group_msg_history", json!({"group_id": 123}))
            .await
            .unwrap_err()
            .to_string(),
        "not_connected"
    );
    // OneBot 未连接时 get_login_info 回退 Telegram（只验证路由，不发起请求）。
    assert_eq!(gw.owns_call("get_login_info", &json!({})), Route::Telegram);
    // 无法判定的 action 仍优先 OneBot。
    assert_eq!(
        gw.call("get_forward_msg", json!({"id":"x"}))
            .await
            .unwrap_err()
            .to_string(),
        "not_connected"
    );
}

#[test]
fn login_info_prefers_onebot_when_telegram_disabled() {
    let (gw, _rx) = gateways(json!({"onebot": {"selfId": "111"}}));
    // Telegram 关闭时 get_login_info 仍归 OneBot。
    assert_eq!(gw.owns_call("get_login_info", &json!({})), Route::OneBot);
}

#[test]
fn history_capability_follows_chat_owner() {
    let (gw, _rx) = gateways(telegram_extra());
    // Telegram 归属群不提供历史；QQ 群仍可回填。
    assert!(!gw.can_fetch_history("-100"));
    assert!(gw.can_fetch_history("123"));
    // Telegram 在线也不能让 OneBot 历史回填可用（覆盖 state().connected 聚合语义）。
    gw.telegram()
        .unwrap()
        .set_identity_for_test("222", "test_bot");
    assert!(gw.state().connected);
    assert!(!gw.history_available());
    // Telegram 关闭时负数群仍归 OneBot。
    let (gw, _rx) = gateways(json!({"onebot": {"selfId": "111"}}));
    assert!(gw.can_fetch_history("-100"));
    assert!(!gw.history_available());
}

#[test]
fn event_filter_requires_source_chat_ownership() {
    let filter = EventFilter {
        telegram: true,
        groups: vec!["-100".into()],
        users: vec!["7".into()],
    };
    let telegram_msg = |kind: &str, id: i64| {
        let mut event = json!({
            "post_type":"message","message_type":kind,
            "message_id":1,"user_id":7,"self_id":"222","time":1,
            "sender":{"nickname":"A"},"message":[{"type":"text","data":{"text":"hi"}}],
        });
        if kind == "group" {
            event["group_id"] = json!(id);
        } else {
            event["user_id"] = json!(id);
        }
        GatewayNotice::Telegram(Notification::Event(event))
    };
    // Telegram 来源且命中 Telegram 白名单：转发。
    assert!(filter.allows(&telegram_msg("group", -100)));
    assert!(filter.allows(&telegram_msg("private", 7)));
    // Telegram 来源但只命中 QQ 白名单：丢弃，避免回复发到同号 QQ。
    assert!(!filter.allows(&telegram_msg("group", 123)));
    assert!(!filter.allows(&telegram_msg("private", 8)));
    // OneBot 来源命中 Telegram 白名单：丢弃，避免 QQ 事件被 Telegram 回复。
    let onebot_msg = |event: Value| GatewayNotice::OneBot(Notification::Event(event));
    assert!(!filter.allows(&onebot_msg(json!({
        "post_type":"message","message_type":"group","group_id":-100,"user_id":8,"self_id":"111"
    }))));
    assert!(filter.allows(&onebot_msg(json!({
        "post_type":"message","message_type":"group","group_id":123,"user_id":8,"self_id":"111"
    }))));
    // 非 message/notice 事件不受影响（message_sent 不触发回复）。
    assert!(filter.allows(&onebot_msg(json!({
        "post_type":"message_sent","message_type":"private","user_id":7,"self_id":"111"
    }))));
    assert!(filter.allows(&onebot_msg(
        json!({"post_type":"meta_event","meta_event_type":"lifecycle"})
    )));
    assert!(filter.allows(&GatewayNotice::Telegram(Notification::Status(
        "connected".into()
    ))));
}

#[test]
fn identity_resolves_per_event_and_chat() {
    let (gw, _rx) = gateways(telegram_extra());
    gw.telegram()
        .unwrap()
        .set_identity_for_test("222", "test_bot");
    // self_id 优先 OneBot。
    assert_eq!(gw.self_id(), "111");
    assert_eq!(gw.event_self_id(&json!({"self_id": "111"})), "111");
    assert_eq!(gw.event_self_id(&json!({"self_id": "222"})), "222");
    // 未知 self_id 回退到聚合 self_id，交由下游 normalize 拒绝。
    assert_eq!(gw.event_self_id(&json!({"self_id": "999"})), "111");
    assert_eq!(gw.event_self_id(&json!({})), "111");
    // chat 归属决定 self_id。
    assert_eq!(gw.chat_self_id("group:-100"), "222");
    assert_eq!(gw.chat_self_id("private:7"), "222");
    assert_eq!(gw.chat_self_id("group:123"), "111");
    assert_eq!(gw.chat_self_id("private:8"), "111");
    // 聚合状态：任一连接即 connected/online，self_id 优先 OneBot。
    gw.telegram()
        .unwrap()
        .set_identity_for_test("222", "test_bot");
    let state = gw.state();
    assert!(state.connected && state.online);
    assert_eq!(state.self_id, "111");
}

#[tokio::test]
async fn onebot_only_delegates_unchanged() {
    let (gw, _rx) = gateways(json!({"onebot": {"selfId": "111"}}));
    assert!(gw.telegram().is_none());
    assert_eq!(gw.self_id(), "111");
    // Telegram 关闭时负数群 id 仍走 OneBot 默认路径。
    assert_eq!(gw.chat_self_id("group:-100"), "111");
    assert_eq!(
        gw.send("group:-100", "hi", None).await.unwrap_err().code,
        "qq_offline"
    );
    assert_eq!(
        gw.send_media("group:-100", json!({"type":"face","data":{"id":"1"}}))
            .await
            .unwrap_err()
            .code,
        "qq_offline"
    );
    assert_eq!(
        gw.call("get_group_msg_history", json!({"group_id": -100}))
            .await
            .unwrap_err()
            .to_string(),
        "not_connected"
    );
    assert!(!gw.status()["telegramConnected"].as_bool().unwrap());
}

#[tokio::test]
async fn merged_notices_forward_and_stop() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (ntx, nrx) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        pipe(
            nrx,
            tx,
            GatewayNotice::OneBot,
            EventFilter {
                telegram: false,
                groups: Vec::new(),
                users: Vec::new(),
            },
            std::future::pending::<()>(),
        )
        .await;
    });
    ntx.send(Notification::Status("connected".into())).unwrap();
    match rx.recv().await.unwrap() {
        GatewayNotice::OneBot(Notification::Status(state)) => assert_eq!(state, "connected"),
        other => panic!("unexpected notice: {other:?}"),
    }
    // 后端结束时转发任务退出（dropped sender -> recv None）。
    drop(ntx);
    task.await.unwrap();
}
