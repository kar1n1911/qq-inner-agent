use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{orientation::{OrientationProvider, OrientationTransport}, Engine, EngineTransport, Options},
    store::Store,
    transport::{OneBotError, State},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const NOW: f64 = 1_700_000_000.;
const MEAL_FALLBACK: &str = "这顿整体挺舒服，晚饭够不够还得看饭量。";
const SLEEP_FALLBACK: &str = "早睡计划听着像总要拖到明天。";

struct Model {
    bubbles: Vec<String>,
    text: String,
    review: Review,
    calls: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Copy)]
enum Review { Accept, Invalid, Timeout }

impl OrientationProvider for Model {
    fn json<'a>(&'a self, _system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            let call = {
                let mut calls = self.calls.lock().unwrap();
                let call = calls.len();
                calls.push(payload.clone());
                call
            };
            match call {
                0 => return Ok(json!({"allocation":"self","candidates":[{"kind":"system2","text":"接住这句玩笑"}]})),
                1 => {
                    let id = payload["candidates"][0]["id"].clone();
                    return Ok(json!({"ratings":[{"id":id,"motivation":5.,"relevance":5.,"originality":5.}]}));
                }
                2 => return Ok(json!({"text":self.text,"bubbles":self.bubbles})),
                _ => {}
            }
            match self.review {
                Review::Accept => Ok(json!({"accepted":true,"valid":true,"duplicate":false,
                    "verdict":"accept","allow":true})),
                Review::Invalid => Ok(json!({"unexpected":"invalid review response"})),
                Review::Timeout => std::future::pending::<Result<Value>>().await,
            }
        })
    }
}

#[derive(Default)]
struct Transport { sent: Mutex<Vec<String>> }

impl OrientationTransport for Transport {
    fn self_id(&self) -> String { "999".into() }
    fn call<'a>(&'a self, _action: &'a str, _params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { Ok(json!({})) })
    }
}
impl EngineTransport for Transport {
    fn state(&self) -> State {
        State { connected: true, online: true, self_id: "999".into(), ..State::default() }
    }
    fn send<'a>(&'a self, _chat: &'a str, text: &'a str, _face: Option<&'a str>,
        _reply_to: Option<&'a str>, _mention: Option<&'a str>)
        -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            let mut sent = self.sent.lock().unwrap();
            sent.push(text.to_owned());
            Ok(json!({"message_id":format!("out-{}",sent.len())}))
        })
    }
}

async fn delivered(bubbles: &[&str], text: &str, enabled: bool, max_chars: u64, review: Review) -> Vec<String> {
    delivered_after(bubbles, text, enabled, max_chars, review, None).await
}

async fn delivered_after(bubbles: &[&str], text: &str, enabled: bool, max_chars: u64,
    review: Review, previous_self: Option<&str>) -> Vec<String> {
    let raw = merge(&defaults(), &json!({
        "apiKey":"fixture", "onebotToken":"fixture", "dataDir":"unused",
        "provider":{"model":"fixture-model"},
        "agent":{
            "multiBubble":enabled,"maxOutputChars":max_chars,"quietHours":null,
            "allowedUsers":["42"],
            "debounceSeconds":0,"minThinkIntervalSeconds":1,"threshold":1,
            "learning":{"enabled":false},"observation":{"enabled":false},
            "emoji":{"enabled":false,"faceOnly":false,"learnFrequency":false},
            "sending":{"enabled":false}
        }
    }));
    let config = Config::from_value(&raw).unwrap();
    let transport = Arc::new(Transport::default());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Model {
        bubbles: bubbles.iter().map(|bubble| (*bubble).to_owned()).collect(),
        text: text.into(), review, calls,
    });
    let options = Options {
        now: Arc::new(|| NOW), random: Arc::new(|| 0.),
        selection_random: Arc::new(|| 0.), expression_random: Arc::new(|| 0.7),
        activity_random: Arc::new(|| 0.), ..Options::default()
    };
    let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
    if let Some(previous) = previous_self {
        store.lock().unwrap().message(&json!({"chat":"private:42","id":"old",
            "sender":"999","name":"机器人","text":previous,"ts":NOW-30.,"self":true})).unwrap();
    }
    let engine = Engine::new(config, store,
        model, transport.clone(), options).unwrap();
    engine.ingest(&json!({"post_type":"message","message_type":"private",
        "user_id":42,"self_id":999,"message_id":1,"time":NOW,
        "sender":{"nickname":"群友"},"message":"哈哈"})).unwrap();
    engine.tick().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), engine.wait_idle())
        .await.expect("the turn, including any review, must finish within its budget");
    let sent = transport.sent.lock().unwrap().clone();
    sent
}

#[tokio::test]
async fn sample_b_repeated_conclusion_is_replaced_as_a_whole_before_the_first_send() {
    for bubbles in [
        ["早睡计划听着像明天才开始的计划。", "说到底，早睡计划还是明天再开始的笑话。"],
        ["早睡计划又说明天开始。", "真准时，一到今晚就宣布明天再开始。"],
        ["早睡计划又排到明天了。", "收尾还是那句：早睡等明天。"],
    ] {
        let sent = delivered(&bubbles, SLEEP_FALLBACK, true, 100, Review::Accept).await;
        assert_eq!(sent, [SLEEP_FALLBACK], "a repeated punchline must not leak even its first bubble");
    }
}

#[tokio::test]
async fn sample_a_overall_portion_and_dinner_consequence_all_survive() {
    let bubbles = ["这份餐整体看着挺顺眼。", "分量比照片里显得更扎实。", "拿它当晚饭应该不容易饿。"];
    assert_eq!(delivered(&bubbles, MEAL_FALLBACK, true, 100, Review::Accept).await, bubbles);
}

#[tokio::test]
async fn shared_topic_word_with_a_new_angle_is_not_called_a_duplicate() {
    let bubbles = ["早睡计划今晚先把手机放远一点。", "早睡之后明早通勤也许能少打两个哈欠。"];
    assert_eq!(delivered(&bubbles, "早睡计划今晚试试，明早通勤能更精神。", true, 100, Review::Accept).await, bubbles);
}

#[tokio::test]
async fn invalid_review_falls_back_without_partial_delivery() {
    let bubbles = ["这份餐整体看着挺顺眼。", "分量比照片里显得更扎实。"];
    assert_eq!(delivered(&bubbles, MEAL_FALLBACK, true, 100, Review::Invalid).await, [MEAL_FALLBACK]);
}

#[tokio::test]
async fn timed_out_review_falls_back_without_partial_delivery() {
    let bubbles = ["这份餐整体看着挺顺眼。", "分量比照片里显得更扎实。"];
    assert_eq!(delivered(&bubbles, MEAL_FALLBACK, true, 100, Review::Timeout).await, [MEAL_FALLBACK]);
}

#[tokio::test]
async fn fallback_does_not_bypass_the_existing_final_text_guard() {
    let bubbles = ["早睡计划又说明天开始。", "收尾还是那句：早睡等明天。"];
    assert!(delivered(&bubbles, "<think>不应发送</think>", true, 100,
        Review::Invalid).await.is_empty());
}

#[tokio::test]
async fn invalid_shape_and_length_never_send_an_unreviewed_bubble() {
    for (bubbles, limit) in [
        (vec!["一".into(), "二".into(), "三".into(), "四".into()], 100),
        (vec!["一".into(), "  ".into()], 100),
        (vec!["一".into(), "这条内容的长度明确超过单条限制".into()], 10),
        (vec!["甲".repeat(31), "乙".repeat(31), "丙".repeat(31)], 100),
    ] {
        let parts: Vec<_> = bubbles.iter().map(String::as_str).collect();
        let sent = delivered(&parts, MEAL_FALLBACK, true, limit, Review::Accept).await;
        assert!(sent.iter().all(|text| !bubbles.contains(text)),
            "invalid batch cannot partially escape: {sent:?}");
    }
}

#[tokio::test]
async fn default_switch_off_uses_only_the_existing_single_text() {
    let bubbles = ["第一条不会被发", "第二条不会被发"];
    assert_eq!(delivered(&bubbles, MEAL_FALLBACK, false, 100, Review::Accept).await, [MEAL_FALLBACK]);
}

#[tokio::test]
async fn existing_history_repetition_rule_still_checks_the_single_text() {
    let bubbles = ["这份餐整体看着挺顺眼。", "分量比照片里显得更扎实。"];
    assert!(delivered_after(&bubbles, MEAL_FALLBACK, true, 100, Review::Accept,
        Some(MEAL_FALLBACK)).await.is_empty());
}
