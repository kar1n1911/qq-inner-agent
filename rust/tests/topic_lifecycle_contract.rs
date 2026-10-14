//! P acceptance tests: drive real inbound -> model -> send, then inspect sends.
//! These tests deliberately do not call the topic scoring function as an oracle.
//! For the explicitly requested learned-degree assertions, assume an operational
//! read-only `topic_revisit_status` view exposing `chat`, `topic`, and a [0,1]
//! `revisitability`. This query shape is a test-author choice. If the product
//! exposes an equivalent query elsewhere, adapt only `revisitability` below.

use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{orientation::{OrientationProvider, OrientationTransport}, Engine, EngineTransport, Options},
    store::Store,
    transport::{message_segments, OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex};

const TOPIC: &str = "海豚照片梗经典回顾";

#[derive(Clone, Debug)]
struct Sent { chat: String, segments: Vec<Value> }

fn reply_id(sent: &Sent) -> Option<&str> {
    sent.segments.iter().find(|segment| segment["type"] == "reply")
        .and_then(|segment| segment["data"]["id"].as_str())
}

struct Peer {
    sent: Mutex<Vec<Sent>>,
    necessary: bool,
}

impl OrientationProvider for Peer {
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if system.contains("TASK: FORM") {
                Ok(json!({"allocation":"open","candidates":[{"kind":"system2","text":TOPIC}]}))
            } else if system.contains("TASK: EVALUATE") {
                let id = payload["candidates"][0]["id"].clone();
                Ok(json!({"ratings":[{"id":id,"motivation":5.0,"relevance":5.0,
                    "originality":5.0,"for":if self.necessary { vec!["urgency"] } else { vec!["relevance"] },
                    "against":[]}]}))
            } else if system.contains("TASK: ARTICULATE") {
                // The model deliberately omits replyTo. A mandatory reference
                // must be chosen from the real topic target, not a model guess.
                Ok(json!({"text":"那个海豚图我还记得","emoji":null,"faceId":null,
                    "bubbles":[],"replyTo":null,"mention":null}))
            } else {
                anyhow::bail!("unexpected model request")
            }
        })
    }
}

impl OrientationTransport for Peer {
    fn self_id(&self) -> String { "99".into() }
    fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unexpected network call") })
    }
}

impl EngineTransport for Peer {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "99".into(), ..State::default() }
    }
    fn send<'a>(
        &'a self, chat: &'a str, body: &'a str, face: Option<&'a str>,
        reply_to: Option<&'a str>, mention: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(Sent {
                chat: chat.into(), segments: message_segments(chat, body, face, reply_to, mention),
            });
            Ok(json!({"message_id":"ack-1"}))
        })
    }
}

struct World {
    engine: Arc<Engine>,
    store: Arc<Mutex<Store>>,
    peer: Arc<Peer>,
    clock: Arc<AtomicU64>,
    logs: Arc<Mutex<Vec<(String, Value)>>>,
}

fn world(necessary: bool, topic_learning: bool) -> Result<World> {
    let config = Config::from_value(&merge(&defaults(), &json!({
        "apiKey":"test-only", "onebotToken":"", "dataDir":"/tmp/topic-lifecycle-contract",
        "provider":{"model":"fixture"},
        "agent":{
            "allowedGroups":["10","11","12"],
            "debounceSeconds":0, "minThinkIntervalSeconds":1,
            "quietHours":null,
            "observation":{"enabled":false}, "identity":{"enabled":false},
            "learning":{"enabled":false}, "memoryRecall":false,
            "sending":{"enabled":false}, "emoji":{"enabled":false,"learnFrequency":false},
            "topicLifecycle":{"recentSeconds":300,"remoteSeconds":1800,
                "learning":{"enabled":topic_learning}}
        }
    })))?;
    let store = Arc::new(Mutex::new(Store::in_memory()?));
    let peer = Arc::new(Peer { sent: Mutex::new(Vec::new()), necessary });
    let clock = Arc::new(AtomicU64::new(10_000));
    let logs = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::new(
        config, store.clone(), peer.clone(), peer.clone(),
        Options {
            now: { let clock = clock.clone(); Arc::new(move || clock.load(Ordering::SeqCst) as f64) },
            random: Arc::new(|| 0.0),
            selection_random: Arc::new(|| 0.0),
            expression_random: Arc::new(|| 0.0),
            log: { let logs = logs.clone(); Arc::new(move |name, value| logs.lock().unwrap().push((name.into(), value))) },
            ..Options::default()
        },
    )?;
    Ok(World { engine, store, peer, clock, logs })
}

fn old_topic(w: &World, group: &str, id: &str, ts: f64, sender: &str, self_message: bool) -> Result<()> {
    w.store.lock().unwrap().record_message(&json!({
        "chat":format!("group:{group}"),"id":id,"sender":sender,"name":"participant",
        "text":TOPIC,"ts":ts,"self":self_message
    }))?;
    Ok(())
}

fn bot_send(w: &World, group: &str, id: &str, ts: f64) -> Result<()> {
    let chat = format!("group:{group}");
    let store = w.store.lock().unwrap();
    let delivery = store.delivery(&chat, true, ts)?;
    store.finish_delivery(&delivery, "sent_unconfirmed", Some(&json!(id)))?;
    store.outbound_message(&json!({
        "chat":chat,"id":id,"sender":"99","name":"bot","text":TOPIC,
        "ts":ts,"self":true
    }), &delivery)?;
    Ok(())
}

fn inbound(group: &str, id: &str, ts: u64, sender: &str, message: Value) -> Value {
    json!({"post_type":"message","message_type":"group","group_id":group,
        "self_id":"99","user_id":sender,"message_id":id,"time":ts,
        "sender":{"nickname":"friend"},"message":message})
}

async fn trigger(w: &World, group: &str, id: &str, message: Value) -> Result<()> {
    w.engine.ingest(&inbound(group, id, 9_999, "20", message))?;
    w.engine.tick()?;
    w.engine.wait_idle().await;
    anyhow::ensure!(w.engine.last_error().is_none(), "engine error: {:?}", w.engine.last_error());
    Ok(())
}

fn sent_in(w: &World, chat: &str) -> Vec<Sent> {
    w.peer.sent.lock().unwrap().iter().filter(|s| s.chat == chat).cloned().collect()
}

fn revisitability(w: &World, chat: &str) -> Result<f64> {
    let rows = w.store.lock().unwrap().rows(
        "SELECT revisitability FROM topic_revisit_status WHERE chat=? AND topic=?",
        [chat, TOPIC],
    ).context("learned topic revisitability must be inspectable per group")?;
    anyhow::ensure!(rows.len() == 1, "expected one learned topic state for {chat}");
    let score = rows[0]["revisitability"].as_f64().context("revisitability must be numeric")?;
    anyhow::ensure!((0.0..=1.0).contains(&score), "revisitability must be normalized");
    Ok(score)
}

#[tokio::test]
async fn recent_topic_can_send_without_a_reply_segment() -> Result<()> {
    let w = world(false, false)?;
    old_topic(&w, "10", "topic-1", 9_900., "30", false)?;
    trigger(&w, "10", "current-1", json!("今天吃什么")).await?;
    let sent = sent_in(&w, "group:10");
    assert_eq!(sent.len(), 1, "a recent topic remains eligible to send; logs={:?}", w.logs.lock().unwrap());
    assert_eq!(reply_id(&sent[0]), None, "reply is optional while topic is alive");
    Ok(())
}

#[tokio::test]
async fn ended_topic_send_quotes_the_exact_message_despite_a_new_unrelated_message() -> Result<()> {
    let w = world(false, false)?;
    old_topic(&w, "10", "topic-2", 9_500., "30", false)?;
    trigger(&w, "10", "current-2", json!("今天吃什么")).await?;
    let sent = sent_in(&w, "group:10");
    assert_eq!(sent.len(), 1, "middle tier should send");
    assert_eq!(reply_id(&sent[0]), Some("topic-2"));
    Ok(())
}

#[tokio::test]
async fn remote_topic_is_withheld_without_a_reason_and_sent_with_an_exact_quote_when_needed() -> Result<()> {
    let unnecessary = world(false, false)?;
    old_topic(&unnecessary, "10", "topic-3", 7_000., "30", false)?;
    trigger(&unnecessary, "10", "current-3", json!("今天吃什么")).await?;
    assert!(sent_in(&unnecessary, "group:10").is_empty(), "unneeded remote topic must not send");

    let necessary = world(true, false)?;
    old_topic(&necessary, "10", "topic-3", 7_000., "30", false)?;
    trigger(&necessary, "10", "current-3", json!("今天吃什么")).await?;
    let sent = sent_in(&necessary, "group:10");
    assert_eq!(sent.len(), 1, "needed remote topic should send");
    assert_eq!(reply_id(&sent[0]), Some("topic-3"));
    Ok(())
}

#[tokio::test]
async fn unresolved_reply_and_unrelated_activity_do_not_extend_topic_lifetime() -> Result<()> {
    let w = world(false, false)?;
    old_topic(&w, "10", "topic-4", 9_500., "30", false)?;
    // A pre-R row has no original reply segment; its apparent proximity proves nothing.
    w.store.lock().unwrap().record_message(&json!({
        "chat":"group:10","id":"old-unknown","sender":"40","name":"old",
        "text":"[reply] 今天吃什么","ts":9_998.0,"self":false
    }))?;
    // An unresolved reference is not proof that this new message belongs to topic-4.
    trigger(&w, "10", "current-4", json!([
        {"type":"reply","data":{"id":"missing-target"}},
        {"type":"text","data":{"text":"今天吃什么"}}
    ])).await?;
    let sent = sent_in(&w, "group:10");
    assert_eq!(sent.len(), 1);
    assert_eq!(reply_id(&sent[0]), Some("topic-4"));
    let logs = w.logs.lock().unwrap();
    let lifecycle = logs.iter().find(|(name, v)| name == "topic_lifecycle" && v["chat"] == "group:10")
        .context("topic lifecycle decision must be observable")?;
    assert_eq!(lifecycle.1["lastActivity"], 9_500.0);
    Ok(())
}

async fn feedback_history(w: &World, group: &str, positive: bool) -> Result<()> {
    let bot_id = format!("bot-{group}");
    bot_send(w, group, &bot_id, 7_000.)?;
    for n in 0..3 {
        w.clock.store(7_001 + n, Ordering::SeqCst);
        let text = if positive { "这个海豚照片梗真好笑，再来" } else { "别再发这个海豚照片梗了" };
        w.engine.ingest(&inbound(group, &format!("feedback-{group}-{n}"), 7_001 + n,
            &format!("3{n}"), json!([
                {"type":"reply","data":{"id":bot_id}},
                {"type":"text","data":{"text":text}}
            ])))?;
    }
    w.clock.store(10_000, Ordering::SeqCst);
    Ok(())
}

#[tokio::test]
async fn opposite_group_feedback_diverges_and_does_not_leak_to_a_third_group() -> Result<()> {
    let w = world(false, true)?;
    feedback_history(&w, "10", true).await?;
    feedback_history(&w, "11", false).await?;
    old_topic(&w, "12", "topic-12", 7_000., "30", false)?;
    for group in ["10", "11", "12"] {
        trigger(&w, group, &format!("current-{group}"), json!("今天吃什么")).await?;
    }
    assert_eq!(sent_in(&w, "group:10").len(), 1, "positive feedback should permit revisiting");
    assert!(sent_in(&w, "group:11").is_empty(), "negative feedback should discourage revisiting");
    assert!(sent_in(&w, "group:12").is_empty(), "another group's positive learning must not leak");
    let positive = revisitability(&w, "group:10")?;
    let negative = revisitability(&w, "group:11")?;
    anyhow::ensure!(positive > negative, "opposite feedback must produce different learned degrees");
    Ok(())
}

#[tokio::test]
async fn disabling_topic_learning_restores_the_static_remote_decision() -> Result<()> {
    let w = world(false, false)?;
    feedback_history(&w, "10", true).await?;
    trigger(&w, "10", "current-static", json!("今天吃什么")).await?;
    assert!(sent_in(&w, "group:10").is_empty(), "static remote policy must remain in force");
    Ok(())
}

#[tokio::test]
async fn one_unanswered_revisit_does_not_erase_positive_group_feedback() -> Result<()> {
    let w = world(false, true)?;
    feedback_history(&w, "10", true).await?;
    // One prior bot revisit received no reply. Silence is weaker than explicit feedback.
    bot_send(&w, "10", "unanswered-revisit", 7_100.)?;
    trigger(&w, "10", "current-after-silence", json!("今天吃什么")).await?;
    assert_eq!(sent_in(&w, "group:10").len(), 1,
        "one unanswered message must not drive revisitability to its floor");
    anyhow::ensure!(revisitability(&w, "group:10")? > 0.0,
        "one silence is a weak negative signal, not a floor reset");
    Ok(())
}

#[tokio::test]
async fn historical_messages_are_not_retroactively_scored_as_feedback() -> Result<()> {
    let w = world(false, true)?;
    old_topic(&w, "10", "historical-bot", 7_000., "99", true)?;
    for n in 0..3 {
        w.engine.ingest_backfill(&inbound("10", &format!("historical-{n}"), 7_001 + n,
            &format!("3{n}"), json!([
                {"type":"reply","data":{"id":"historical-bot"}},
                {"type":"text","data":{"text":"这个海豚照片梗真好笑，再来"}}
            ])))?;
    }
    trigger(&w, "10", "current-after-backfill", json!("今天吃什么")).await?;
    assert!(sent_in(&w, "group:10").is_empty(),
        "old backfilled references must not become fresh positive feedback");
    Ok(())
}
