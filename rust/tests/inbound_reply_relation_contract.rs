//! R acceptance tests: observe the stored result of real inbound events.
//!
//! Query surface assumed by these independent tests: an operational read-only
//! `inbound_reply_status` view, with one row per stored inbound message and
//! `source_message_key`, `raw_target_id`, `target_message_key`, `status`, `reason`.
//! Its name/column layout is a test-author choice, not an existing design.
//! The implementation may expose an equivalent query API instead; in that case
//! adapt only `observed_reply`, preserving the assertions below.

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{orientation::{OrientationProvider, OrientationTransport}, Engine, EngineTransport, Options},
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct NoNetwork;

impl OrientationProvider for NoNetwork {
    fn json<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected model call") })
    }
}

impl OrientationTransport for NoNetwork {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected transport call") })
    }
}

impl EngineTransport for NoNetwork {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "99".into(), ..State::default() }
    }
    fn send<'a>(
        &'a self, _: &'a str, _: &'a str, _: Option<&'a str>, _: Option<&'a str>, _: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async { panic!("unexpected send") })
    }
}

fn setup() -> Result<(Arc<Engine>, Arc<Mutex<Store>>)> {
    let config = Config::from_value(&merge(&defaults(), &json!({
        "apiKey": "test-only", "onebotToken": "", "dataDir": "/tmp/reply-contract-test",
        "agent": {
            "allowedGroups": ["10", "11"],
            "observation": {"enabled": false},
            "learning": {"enabled": false},
            "identity": {"enabled": false},
            "emoji": {"learnFrequency": false}
        }
    })))?;
    let store = Arc::new(Mutex::new(Store::in_memory()?));
    let engine = Engine::new(
        config, store.clone(), Arc::new(NoNetwork), Arc::new(NoNetwork),
        Options { now: Arc::new(|| 1001.0), ..Options::default() },
    )?;
    Ok((engine, store))
}

fn target(store: &Store, chat: &str, protocol_id: &str, sender: &str, is_self: bool) -> Result<String> {
    let (_, key) = store.record_message(&json!({
        "chat": chat, "id": protocol_id, "sender": sender, "name": "target",
        "text": "earlier message", "ts": 1000.0, "self": is_self
    }))?;
    Ok(key)
}

fn event(group: &str, message_id: &str, message: Value) -> Value {
    json!({
        "post_type": "message", "message_type": "group", "group_id": group,
        "self_id": "99", "user_id": "20", "message_id": message_id,
        "time": 1000, "sender": {"nickname": "friend"}, "message": message
    })
}

fn received_key(store: &Store, chat: &str, protocol_id: &str) -> Result<String> {
    let rows = store.rows(
        "SELECT message_key FROM messages WHERE chat=? AND id=? AND sender='20'",
        [chat, protocol_id],
    )?;
    anyhow::ensure!(rows.len() == 1, "incoming message must have one stored internal key");
    Ok(rows[0]["message_key"].as_str().unwrap().to_owned())
}

fn observed_reply(store: &Store, source_key: &str) -> Result<Value> {
    let rows = store.rows(
        "SELECT r.raw_target_id, r.target_message_key, r.status, r.reason, \
                m.sender AS target_sender, m.self AS target_self \
         FROM inbound_reply_status r \
         LEFT JOIN messages m ON m.message_key=r.target_message_key \
         WHERE r.source_message_key=?",
        [source_key],
    ).context("inbound reply state must be queryable by source message_key")?;
    anyhow::ensure!(rows.len() == 1, "every stored inbound message must have one queryable reply state");
    Ok(rows.into_iter().next().unwrap())
}

#[test]
fn array_and_cq_reply_both_resolve_to_the_bot_message_key() -> Result<()> {
    let (engine, shared) = setup()?;
    let bot_key = target(&shared.lock().unwrap(), "group:10", "1313390833", "99", true)?;
    engine.ingest(&event("10", "2001", json!([
        {"type":"reply","data":{"id":"1313390833"}},
        {"type":"text","data":{"text":"first"}}
    ])))?;
    engine.ingest(&event("10", "2002", json!("[CQ:reply,id=1313390833] second")))?;
    let store = shared.lock().unwrap();
    for id in ["2001", "2002"] {
        let source_key = received_key(&store, "group:10", id)?;
        let reply = observed_reply(&store, &source_key)?;
        assert_eq!(reply["status"], "resolved", "format for message {id}");
        assert_eq!(reply["raw_target_id"], "1313390833");
        assert_eq!(reply["target_message_key"], bot_key);
        assert_eq!(reply["target_sender"], "99");
        assert_eq!(reply["target_self"], 1);
    }
    Ok(())
}

#[test]
fn identical_protocol_ids_are_scoped_to_the_chat_and_not_treated_as_user_ids() -> Result<()> {
    let (engine, shared) = setup()?;
    let bot_key = target(&shared.lock().unwrap(), "group:10", "1313390833", "99", true)?;
    let human_key = target(&shared.lock().unwrap(), "group:11", "1313390833", "30", false)?;
    // The protocol ID here equals the bot's user ID, but the target sender is human.
    let user_id_collision_key = target(&shared.lock().unwrap(), "group:11", "99", "30", false)?;
    for (id, reply_id) in [("2101", "1313390833"), ("2102", "99")] {
        engine.ingest(&event("11", id, json!([
            {"type":"reply","data":{"id":reply_id}},
            {"type":"text","data":{"text":"response"}}
        ])))?;
    }
    engine.ingest(&event("10", "2103", json!([
        {"type":"reply","data":{"id":"99"}},
        {"type":"text","data":{"text":"only another group has this ID"}}
    ])))?;
    let store = shared.lock().unwrap();
    let first = observed_reply(&store, &received_key(&store, "group:11", "2101")?)?;
    assert_eq!(first["status"], "resolved");
    assert_eq!(first["target_message_key"], human_key);
    assert_ne!(first["target_message_key"], bot_key);
    assert_eq!(first["target_sender"], "30");
    assert_eq!(first["target_self"], 0);
    let second = observed_reply(&store, &received_key(&store, "group:11", "2102")?)?;
    assert_eq!(second["target_message_key"], user_id_collision_key);
    assert_eq!(second["target_self"], 0, "reply.id=bot QQ must not mean a bot reply");
    let cross_group_only = observed_reply(&store, &received_key(&store, "group:10", "2103")?)?;
    assert_eq!(cross_group_only["status"], "unresolved");
    assert!(cross_group_only["target_message_key"].is_null());
    Ok(())
}

#[test]
fn absent_or_invalid_reply_id_and_missing_target_remain_unresolved() -> Result<()> {
    let (engine, shared) = setup()?;
    // Reject a non-string ID even if careless JSON stringification would find a target.
    target(&shared.lock().unwrap(), "group:10", "[object Object]", "99", true)?;
    for (id, message) in [
        ("2201", json!([{"type":"reply","data":{}}, {"type":"text","data":{"text":"a"}}])),
        ("2202", json!([{"type":"reply","data":{"id":{}}}, {"type":"text","data":{"text":"b"}}])),
        ("2203", json!("[CQ:reply,id=not-in-this-chat] c")),
    ] {
        engine.ingest(&event("10", id, message))?;
    }
    let store = shared.lock().unwrap();
    for id in ["2201", "2202", "2203"] {
        let reply = observed_reply(&store, &received_key(&store, "group:10", id)?)?;
        assert_eq!(reply["status"], "unresolved", "message {id}");
        assert!(reply["target_message_key"].is_null(), "message {id} must not guess");
        assert!(reply["reason"].as_str().is_some_and(|reason| !reason.is_empty()));
    }
    Ok(())
}

#[test]
fn same_chat_collision_is_observably_ambiguous_and_old_plain_message_is_unknown() -> Result<()> {
    let (engine, shared) = setup()?;
    target(&shared.lock().unwrap(), "group:10", "112965911", "99", true)?;
    target(&shared.lock().unwrap(), "group:10", "112965911", "30", false)?;
    engine.ingest(&event("10", "2301", json!([
        {"type":"reply","data":{"id":"112965911"}},
        {"type":"text","data":{"text":"ambiguous"}}
    ])))?;
    // Simulate a pre-R stored message: the database has no original segment.
    target(&shared.lock().unwrap(), "group:10", "2302", "20", false)?;
    let store = shared.lock().unwrap();
    let ambiguous = observed_reply(&store, &received_key(&store, "group:10", "2301")?)?;
    assert_eq!(ambiguous["status"], "unresolved");
    assert_eq!(ambiguous["reason"], "ambiguous");
    assert!(ambiguous["target_message_key"].is_null());
    let old = observed_reply(&store, &received_key(&store, "group:10", "2302")?)?;
    assert_eq!(old["status"], "unknown");
    assert!(old["target_message_key"].is_null());
    Ok(())
}
