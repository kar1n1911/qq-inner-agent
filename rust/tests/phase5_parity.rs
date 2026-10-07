//! P5 契约测试：精确断言状态不变量；概率/时长只使用容差或区间。
//! 所有业务时间与随机数均显式注入，期望值为固化金标准，所有断言无条件运行。
#[path = "golden/mod.rs"]
mod golden;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Agent},
    engine::activity::ActivityRhythm,
    engine::orientation::{
        observation_satisfied, GroupOrientation, OrientationProvider, OrientationTransport,
    },
    engine::policy::{active_at, normalize},
    store::Store,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::sync::watch;

fn expected(input: Value) -> Value {
    golden::expected(include_str!("golden/phase5.json"), &input)
}
fn agent(extra: &Value) -> Agent {
    serde_json::from_value(merge(&defaults()["agent"], extra)).unwrap()
}
fn event(message: Value) -> Value {
    json!({"post_type":"message","message_type":"group","self_id":99,"user_id":20,"group_id":10,"message_id":123,"time":1000,"sender":{"card":"群名片","nickname":"昵称"},"message":message})
}
fn normalized(case: &Value) -> Value {
    let a = agent(&case["agent"]);
    match normalize(
        &case["event"],
        case["selfId"].as_str().unwrap(),
        &a,
        case["now"].as_f64().unwrap(),
    ) {
        None => Value::Null,
        Some(m) => {
            let units: Vec<u16> = m.text.encode_utf16().collect();
            let mut v = serde_json::to_value(m).unwrap();
            v["text"] = json!(units);
            v
        }
    }
}
fn case(label: &str, e: Value) -> Value {
    json!({"label":label,"event":e,"selfId":"99","now":1000,"agent":{"allowedGroups":["10"],"allowedUsers":["20"],"ignoredUsers":["21"],"aliases":["Bot"],"activeWindowSeconds":100,"maxInputChars":100}})
}

#[test]
fn normalize_matrix_matches_real_js() {
    let mut cases = Vec::new();
    for (id, hint) in [("99", "self"), ("30", "other"), ("all", "open")] {
        cases.push(case(
            &format!("array at {hint}"),
            event(json!([{"type":"at","data":{"qq":id}},{"type":"text","data":{"text":"你好"}}])),
        ));
        cases.push(case(
            &format!("CQ at {hint}"),
            event(json!(format!("[CQ:at,qq={id},name=x]你好"))),
        ));
    }
    for (label, message) in [
        (
            "array markers",
            json!([{"type":"face","data":{"id":"123"}},{"type":"reply","data":{"id":1}},{"type":"image","data":{"url":"secret"}},{"type":"face","data":{"id":"123456"}},{"data":{}}]),
        ),
        (
            "CQ markers",
            json!("[CQ:face,id=123][CQ:reply,id=1][CQ:image,url=secret][CQ:face,id=123456]"),
        ),
        ("self wins", json!("[CQ:at,qq=30][CQ:at,qq=99]")),
        (
            "array plain",
            json!([{"type":"text","data":{"text":" &#91;&amp; "}}]),
        ),
        (
            "entities",
            json!(
                "&#44;&#91;&#93;&amp;|&amp;#44;&amp;#91;&amp;#93;&amp;amp;|&#91;CQ:at,qq=99&#93;"
            ),
        ),
        ("empty string", json!("")),
        ("whitespace", json!(" \n\t\u{feff}")),
        ("empty array", json!([])),
        ("missing message", Value::Null),
        ("long BMP string", json!("甲".repeat(130))),
        (
            "long BMP array",
            json!([{"type":"text","data":{"text":"甲".repeat(130)}}]),
        ),
        ("emoji within bound", json!("你好😀")),
        ("alias colon", json!("bOT:你好")),
        ("alias full colon", json!("BOT：你好")),
        ("alias mention", json!("@bot 你好")),
        ("alias not prefix", json!("你好 Bot:你好")),
        ("alias missing space", json!("@Bot你好")),
    ] {
        cases.push(case(label, event(message)));
    }
    for (label, patch) in [
        ("not allowed group", json!({"group_id":11})),
        (
            "not allowed private",
            json!({"message_type":"private","user_id":30}),
        ),
        ("private", json!({"message_type":"private"})),
        ("ignored", json!({"user_id":21})),
        ("self", json!({"user_id":99})),
        ("wrong self_id", json!({"self_id":98})),
        ("no sender", json!({"user_id":0})),
        ("no message id", json!({"message_id":null})),
        ("zero message id", json!({"message_id":0})),
        ("not message", json!({"post_type":"notice"})),
        ("invalid kind", json!({"message_type":"other"})),
        ("too old", json!({"time":899})),
        ("old boundary", json!({"time":900})),
        ("too future", json!({"time":1061})),
        ("future boundary", json!({"time":1060})),
        ("nan time", json!({"time":"NaN"})),
        ("infinite time", json!({"time":"Infinity"})),
        ("zero time fallback", json!({"time":0})),
        ("string time", json!({"time":"950"})),
    ] {
        cases.push(case(label, merge(&event(json!("你好")), &patch)));
    }
    let mut no_self = case("empty self id", event(json!("你好")));
    no_self["selfId"] = json!("");
    cases.push(no_self);
    let actual: Vec<_> = cases.iter().map(normalized).collect();
    // 解码发生在 CQ 替换之后，且 &amp; 必须最后替换，不能二次解码或新增 @ 提示。
    let i = cases.iter().position(|c| c["label"] == "entities").unwrap();
    assert_eq!(
        actual[i]["text"],
        json!(",[]&|&#44;&#91;&#93;&amp;|[CQ:at,qq=99]"
            .encode_utf16()
            .collect::<Vec<_>>())
    );
    assert_eq!(actual[i]["hint"], "open");
    for i in 0..6 {
        assert_eq!(
            actual[i]["hint"],
            ["self", "self", "other", "other", "open", "open"][i]
        );
    }
    {
        let expected = expected(json!({"kind":"normalize","cases":cases}));
        for (i, got) in actual.iter().enumerate() {
            assert_json(got, &expected[i]);
        }
    }
}

#[test]
fn normalize_char_truncation_explicitly_differs_from_js_utf16() {
    for array in [false, true] {
        // 第 100 个 UTF-16 码元是 emoji 的高代理项；Rust 保留完整第 100 个 char。
        let raw = format!("{}😀尾", "甲".repeat(99));
        let message = if array {
            json!([{"type":"text","data":{"text":raw}}])
        } else {
            json!(raw)
        };
        let c = case("surrogate split", event(message));
        let got = normalized(&c);
        let units = got["text"].as_array().unwrap();
        assert_eq!(units.len(), 101);
        assert_eq!(&units[99..], &[json!(0xd83d), json!(0xde00)]);
        {
            let js = expected(json!({"kind":"normalize","cases":[c]}));
            assert_eq!(js[0]["text"].as_array().unwrap().len(), 100);
            assert_eq!(js[0]["text"][99], 0xd83d);
            assert_eq!(&units[..100], js[0]["text"].as_array().unwrap());
            assert_ne!(got["text"], js[0]["text"]);
        }
    }
}

// JSON 的 1 与 1.0 是同一个 JS number；比较值而非 serde_json 的内部数字表示。
fn assert_json(got: &Value, expected: &Value) {
    match (got, expected) {
        (Value::Number(a), Value::Number(b)) => assert_eq!(a.as_f64(), b.as_f64()),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                assert_json(a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.len(), b.len(), "{got} != {expected}");
            for (k, v) in a {
                assert!(b.contains_key(k), "missing {k}");
                assert_json(v, &b[k]);
            }
        }
        _ => assert_eq!(got, expected),
    }
}

#[test]
fn activity_persistence_redraw_and_disabled_fallback_match_js() {
    let store = Store::in_memory().unwrap();
    let base = json!({"schedule":{"enabled":false,"timezone":"UTC"},"rhythm":{"enabled":true,"dayProbability":0.5,"activeMinSeconds":30,"activeMaxSeconds":40,"restMinSeconds":50,"restMaxSeconds":60}});
    let changed_rhythm = merge(&base, &json!({"rhythm":{"dayProbability":0.75}}));
    let changed_schedule = merge(&changed_rhythm, &json!({"schedule":{"enabled":true}}));
    let disabled = merge(&changed_schedule, &json!({"rhythm":{"enabled":false}}));
    let steps = vec![
        json!({"now":1000,"agent":base,"draws":[0.0,0.0]}),
        json!({"now":1010,"agent":base,"draws":[]}),
        json!({"now":1030,"agent":base,"draws":[0.0,0.999999]}),
        json!({"now":1029,"agent":base,"draws":[0.9,0.999999]}),
        json!({"now":1030,"agent":changed_rhythm,"draws":[0.0,0.999999]}),
        json!({"now":1031,"agent":changed_schedule,"draws":[0.999999,0.0]}),
        json!({"now":43200,"agent":disabled,"draws":[]}),
        json!({"now":0,"agent":disabled,"draws":[]}),
    ];
    let mut trace = Vec::new();
    for step in &steps {
        let a = agent(&step["agent"]);
        let now = step["now"].as_f64().unwrap();
        let draws = step["draws"].as_array().unwrap();
        let mut used = 0;
        // 每一步重建对象，验证状态来自库而非对象内缓存。
        let snapshot = ActivityRhythm::new(&store, &a.schedule, &a.rhythm, || {
            let n = draws
                .get(used)
                .expect("unexpected redraw")
                .as_f64()
                .unwrap();
            used += 1;
            n
        })
        .snapshot(now)
        .unwrap();
        assert_eq!(used, draws.len());
        if snapshot.enabled {
            let duration = snapshot.until.unwrap() - snapshot.started.unwrap();
            let (min, max) = if snapshot.active {
                (a.rhythm.active_min_seconds, a.rhythm.active_max_seconds)
            } else {
                (a.rhythm.rest_min_seconds, a.rhythm.rest_max_seconds)
            };
            assert!((min..=max).contains(&duration));
            let row = store.activity_state().unwrap().unwrap();
            assert_eq!(row.started, snapshot.started.unwrap());
            assert_eq!(row.until, snapshot.until.unwrap());
            assert_eq!(row.active != 0, snapshot.active);
        } else {
            assert_eq!(snapshot.active, active_at(now, &a.schedule));
            assert_eq!(snapshot.started, None);
            assert_eq!(snapshot.until, None);
        }
        let mut row = serde_json::to_value(store.activity_state().unwrap().unwrap()).unwrap();
        row["id"] = json!(1);
        trace.push(json!({"snapshot":snapshot,"draws":used,"row":row}));
    }
    assert_eq!(trace[0]["row"], trace[1]["row"]);
    // 到期与回拨的重抽时刻、配置变更和相邻同状态均为精确不变量。
    for (i, started) in [(2, 1030.), (3, 1029.), (4, 1030.), (5, 1031.)] {
        assert_eq!(trace[i]["snapshot"]["started"].as_f64(), Some(started));
    }
    assert_eq!(trace[0]["snapshot"]["active"], true);
    assert_eq!(trace[2]["snapshot"]["active"], true);
    assert_eq!(trace[3]["snapshot"]["active"], false);
    assert_ne!(trace[3]["row"]["signature"], trace[4]["row"]["signature"]);
    assert_ne!(trace[4]["row"]["signature"], trace[5]["row"]["signature"]);
    assert_eq!(trace[5]["row"], trace[6]["row"]);
    assert_eq!(trace[6]["row"], trace[7]["row"]);
    assert_eq!(trace[6]["snapshot"]["active"], true);
    assert_eq!(trace[7]["snapshot"]["active"], false);
    let count: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM activity_rhythm WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    {
        let js = expected(json!({"kind":"activity","steps":steps}));
        assert_json(&json!(trace), &js);
    }
}

#[test]
fn observation_threshold_boundaries_match_js_including_disabled_config() {
    let store = Store::in_memory().unwrap();
    let mut row = store.ensure_orientation("group:10", 1000.).unwrap();
    let mut cases = Vec::new();
    let mut actual = Vec::new();
    for enabled in [true, false] {
        for mode in ["both", "either"] {
            let c = agent(&json!({"observation":{"enabled":enabled,"thresholdMode":mode,"minSeconds":30,"minMessages":2}})).observation;
            for now in [999., 1029., 1030., 1031.] {
                for count in [0, 1, 2, 3] {
                    row.message_count = count;
                    let got = observation_satisfied(&row, now, &c);
                    let time = now >= 1030.;
                    let volume = count >= 2;
                    assert_eq!(
                        got,
                        if mode == "both" {
                            time && volume
                        } else {
                            time || volume
                        }
                    );
                    cases.push(json!({"row":row,"now":now,"config":c}));
                    actual.push(got);
                }
            }
        }
    }
    // JS 纯阈值函数不检查 enabled；disabled 放行另由 beforeSpeak 的真实状态机验证。
    {
        let js = expected(json!({"kind":"threshold","cases":cases}));
        assert_eq!(json!(actual), js);
    }
}

struct MockProvider {
    calls: AtomicUsize,
    mode: Mutex<String>,
    payloads: Mutex<Vec<Value>>,
}
impl OrientationProvider for MockProvider {
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            assert_eq!(system, qq_inner_core::prompts::ORIENTATION);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.payloads.lock().unwrap().push(payload);
            match self.mode.lock().unwrap().as_str() {
                "failure" => anyhow::bail!("provider_failure"),
                "invalid" => Ok(json!({"style":""})),
                _ => Ok(json!({"style":" 谨慎接话 ","summary":" 园艺讨论 ","topics":["园艺"]})),
            }
        })
    }
}
#[derive(Default)]
struct MockTransport {
    reads: AtomicUsize,
}
impl OrientationTransport for MockTransport {
    fn self_id(&self) -> String {
        "99".into()
    }
    fn call<'a>(&'a self, action: &'a str, params: Value) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::SeqCst);
            assert_eq!(params["group_id"].as_f64(), Some(10.));
            match action {
                "get_group_info" => {
                    assert_eq!(params["no_cache"], true);
                    Ok(json!({"group_id":10,"group_name":"园艺群"}))
                }
                "_get_group_notice" => anyhow::bail!("unsupported secret"),
                "get_group_msg_history" => Ok(json!({"messages":"malformed"})),
                _ => panic!("unexpected action {action}"),
            }
        })
    }
}
struct Fixture {
    orientation: GroupOrientation,
    store: Arc<Mutex<Store>>,
    provider: Arc<MockProvider>,
    transport: Arc<MockTransport>,
    clock: Arc<AtomicU64>,
    _signal: watch::Sender<bool>,
}
impl Fixture {
    fn new(a: Agent) -> Self {
        let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let provider = Arc::new(MockProvider {
            calls: AtomicUsize::new(0),
            mode: Mutex::new("ok".into()),
            payloads: Mutex::new(Vec::new()),
        });
        let transport = Arc::new(MockTransport::default());
        let clock = Arc::new(AtomicU64::new(1000f64.to_bits()));
        let now = clock.clone();
        let (tx, rx) = watch::channel(false);
        let orientation = GroupOrientation::new(
            store.clone(),
            a,
            provider.clone(),
            transport.clone(),
            Arc::new(move || f64::from_bits(now.load(Ordering::SeqCst))),
            rx,
        )
        .unwrap();
        Self {
            orientation,
            store,
            provider,
            transport,
            clock,
            _signal: tx,
        }
    }
    fn set_time(&self, now: f64) {
        self.clock.store(now.to_bits(), Ordering::SeqCst);
    }
    async fn trace(&self, steps: &[Value]) -> Vec<Value> {
        let mut result = Vec::new();
        for step in steps {
            self.set_time(step["now"].as_f64().unwrap());
            if let Some(mode) = step["mode"].as_str() {
                *self.provider.mode.lock().unwrap() = mode.into();
            }
            let chat = step["chat"].as_str().unwrap_or("group:10");
            let mut gate = Value::Null;
            match step["op"].as_str().unwrap() {
                "observe" => self.orientation.observe(chat).unwrap(),
                "joined" => self
                    .orientation
                    .joined(chat, step["timestamp"].as_f64().unwrap())
                    .unwrap(),
                "speak" => gate = json!(self.orientation.before_speak(chat).await.unwrap()),
                _ => panic!("invalid test op"),
            }
            result.push(json!({"gate":gate,"calls":self.provider.calls.load(Ordering::SeqCst),"reads":self.transport.reads.load(Ordering::SeqCst),"row":self.orientation.get(chat).unwrap(),"profile":self.orientation.profile(chat).unwrap()}));
        }
        result
    }
}
fn orientation_agent(enabled: bool) -> Value {
    json!({"allowedGroups":["10"],"observation":{"enabled":enabled,"minSeconds":30,"minMessages":2}})
}

#[tokio::test]
async fn orientation_ready_once_rejoin_and_duplicate_notices_match_js() {
    let a = orientation_agent(true);
    let f = Fixture::new(agent(&a));
    let steps = vec![
        json!({"op":"observe","now":1000}),
        json!({"op":"speak","now":1000}),
        json!({"op":"observe","now":1000}),
        json!({"op":"speak","now":1029}),
        json!({"op":"speak","now":1030}),
        json!({"op":"speak","now":1031}),
        json!({"op":"observe","now":1031}),
        json!({"op":"joined","now":1040,"timestamp":1040}),
        json!({"op":"observe","now":1041}),
        json!({"op":"joined","now":1042,"timestamp":1040}),
        json!({"op":"joined","now":1043,"timestamp":1039}),
        json!({"op":"joined","now":1050,"timestamp":1050}),
    ];
    let trace = f.trace(&steps).await;
    for i in [1, 3] {
        assert_eq!(trace[i]["gate"], false);
        assert_eq!(trace[i]["calls"], 0);
    }
    for i in [4, 5] {
        assert_eq!(trace[i]["gate"], true);
        assert_eq!(trace[i]["calls"], 1);
        assert_eq!(trace[i]["reads"], 3);
    }
    assert_eq!(trace[6]["row"]["message_count"], 2);
    assert_eq!(trace[7]["row"]["epoch"], 1);
    assert_eq!(trace[7]["row"]["message_count"], 0);
    assert_eq!(trace[7]["row"]["status"], "observing");
    assert_eq!(trace[7]["row"]["sources"], json!({}));
    assert_eq!(trace[7]["profile"], Value::Null);
    // 重复/旧通知连 started 和消息计数也不得改变。
    assert_eq!(trace[8]["row"], trace[9]["row"]);
    assert_eq!(trace[9]["row"], trace[10]["row"]);
    assert_eq!(trace[11]["row"]["epoch"], 2);
    assert_eq!(trace[11]["row"]["started"].as_f64(), Some(1050.));
    let payloads = f.provider.payloads.lock().unwrap();
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        payloads[0]["sources"]["availability"],
        json!({"info":"available","notices":"unavailable","history":"unavailable"})
    );
    assert!(payloads[0]["sources"].get("notices").is_none());
    assert!(payloads[0]["sources"].get("history").is_none());
    assert!(!payloads[0].to_string().contains("secret"));
    drop(payloads);
    {
        let js = expected(json!({"kind":"orientation","agent":a,"steps":steps}));
        assert_json(&json!(trace), &js);
    }
}

#[tokio::test]
async fn orientation_failures_keep_gate_closed_and_retry_at_exact_boundary() {
    for mode in ["failure", "invalid"] {
        let a = orientation_agent(true);
        let f = Fixture::new(agent(&a));
        let steps = vec![
            json!({"op":"observe","now":1000}),
            json!({"op":"observe","now":1000}),
            json!({"op":"speak","now":1030,"mode":mode}),
            json!({"op":"speak","now":1089}),
            json!({"op":"speak","now":1090}),
            json!({"op":"speak","now":1149,"mode":"ok"}),
            json!({"op":"speak","now":1150}),
            json!({"op":"speak","now":1151}),
        ];
        let trace = f.trace(&steps).await;
        for i in [2, 3, 4, 5] {
            assert_eq!(trace[i]["gate"], false);
            assert_eq!(trace[i]["row"]["status"], "observing");
            assert_eq!(trace[i]["profile"], Value::Null);
        }
        assert_eq!(trace[2]["row"]["retry_at"].as_f64(), Some(1090.));
        assert_eq!(trace[3]["calls"], 1);
        assert_eq!(trace[4]["row"]["retry_at"].as_f64(), Some(1150.));
        assert_eq!(trace[5]["calls"], 2);
        assert_eq!(trace[6]["gate"], true);
        assert_eq!(trace[7]["calls"], 3);
        assert_eq!(trace[7]["reads"], 3);
        assert_eq!(trace[7]["row"]["retry_at"].as_f64(), Some(0.));
        assert_eq!(trace[7]["row"]["error"], Value::Null);
        // 退避写入共享数据库，不能只存在于对象内存。
        assert_eq!(
            f.store
                .lock()
                .unwrap()
                .orientation_state("group:10")
                .unwrap()
                .unwrap()
                .status,
            "ready"
        );
        {
            let js = expected(json!({"kind":"orientation","agent":a,"steps":steps}));
            assert_json(&json!(trace), &js);
        }
    }
}

#[tokio::test]
async fn orientation_disabled_and_private_bypass_without_io_match_js() {
    for enabled in [true, false] {
        let a = orientation_agent(enabled);
        let f = Fixture::new(agent(&a));
        let chat = if enabled { "private:20" } else { "group:10" };
        let steps = vec![
            json!({"op":"observe","now":1000,"chat":chat}),
            json!({"op":"speak","now":1000,"chat":chat}),
        ];
        let trace = f.trace(&steps).await;
        assert_eq!(trace[1]["gate"], true);
        assert_eq!(trace[1]["calls"], 0);
        assert_eq!(trace[1]["reads"], 0);
        assert_eq!(trace[1]["row"], Value::Null);
        {
            let js = expected(json!({"kind":"orientation","agent":a,"steps":steps}));
            assert_json(&json!(trace), &js);
        }
    }
}
