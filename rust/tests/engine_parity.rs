//! 不变量精确：阶段顺序、发送次数、状态与作用域。评分只验证方向/区间；
//! golden 数值容差仅处理跨语言浮点表示，不把启发式分数当质量真值。
use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::{Engine, EngineTransport, Options},
    onebot::{OneBotError, State},
    orientation::{OrientationProvider, OrientationTransport},
    store::Store,
};
use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{Notify, Semaphore};

struct Harness {
    now: Mutex<f64>,
    model: Mutex<Value>,
    trace: Mutex<Vec<Value>>,
    store: Arc<Mutex<Store>>,
    engine: Mutex<Weak<Engine>>,
    transport: Mutex<State>,
    sends: Mutex<usize>,
    budget: f64,
    hold: Mutex<Option<String>>,
    entered: Notify,
    release: Semaphore,
}
impl Harness {
    fn now(&self) -> f64 {
        *self.now.lock().unwrap()
    }
    fn event(&self, s: &Value) -> Value {
        json!({"post_type":"message","message_type":"group","self_id":99,"user_id":s.get("sender").unwrap_or(&json!(20)),"group_id":s.get("group").unwrap_or(&json!(10)),"message_id":s.get("id").unwrap_or(&json!("m1")),"time":self.now(),"sender":{"nickname":"Human"},"message":s.get("text").unwrap_or(&json!("[CQ:at,qq=99]你好"))})
    }
    fn push(&self, v: Value) {
        self.trace.lock().unwrap().push(v);
    }
    fn rows(&self, sql: &str) -> Vec<Value> {
        let db = self.store.lock().unwrap();
        let mut q = db.connection().prepare(sql).unwrap();
        let names: Vec<String> = q.column_names().iter().map(|s| s.to_string()).collect();
        q.query_map([], |r| {
            let mut out = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                use rusqlite::types::ValueRef;
                let value = match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(s) => json!(std::str::from_utf8(s).unwrap()),
                    _ => panic!("blob"),
                };
                out.insert(name.clone(), value);
            }
            Ok(Value::Object(out))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }
}
impl OrientationTransport for Harness {
    fn self_id(&self) -> String {
        "99".into()
    }
    fn call<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async { anyhow::bail!("unsupported") })
    }
}
impl EngineTransport for Harness {
    fn state(&self) -> State {
        self.transport.lock().unwrap().clone()
    }
    fn send<'a>(
        &'a self,
        chat: &'a str,
        text: &'a str,
        face: Option<&'a str>,
    ) -> BoxFuture<'a, std::result::Result<Value, OneBotError>> {
        Box::pin(async move {
            {
                let db = self.store.lock().unwrap();
                let pending: i64 = db
                    .connection()
                    .query_row(
                        "SELECT count(*) FROM deliveries WHERE chat=? AND status='pending'",
                        [chat],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(pending, 1, "发送必须已有 pending 投递记录");
                assert_eq!(db.handled(chat).unwrap().unwrap()["pause_done"], 1);
            }
            self.push(json!(["send", chat, text, face]));
            let n = {
                let mut n = self.sends.lock().unwrap();
                *n += 1;
                *n
            };
            let m = self.model.lock().unwrap().clone();
            if let Some(status) = m["delivery"].as_str() {
                return Err(OneBotError {
                    code: "mock_delivery".into(),
                    uncertain: status == "uncertain",
                });
            }
            Ok(json!({"message_id":format!("sent{n}")}))
        })
    }
}
impl OrientationProvider for Harness {
    fn json<'a>(&'a self, system: &'a str, payload: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            let stage = system
                .split("TASK: ")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap();
            self.push(json!([
                "model",
                stage,
                payload["addressedHint"],
                payload["trigger"],
                payload["history"]
                    .as_array()
                    .map(|a| a.iter().map(|m| m["id"].clone()).collect::<Vec<_>>())
                    .unwrap_or_default()
            ]));
            anyhow::ensure!(
                self.store
                    .lock()
                    .unwrap()
                    .call_budget(self.now(), self.budget)?,
                "hourly_api_budget"
            );
            let hold = self.hold.lock().unwrap().as_deref() == Some(stage);
            if hold {
                self.entered.notify_one();
                self.release.acquire().await.unwrap().forget();
            }
            let m = self.model.lock().unwrap().clone();
            if m["effectStage"] == stage {
                self.model
                    .lock()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("effectStage");
                let e = self.engine.lock().unwrap().upgrade().unwrap();
                match m["effect"].as_str().unwrap() {
                    "message" => e.ingest(&self.event(&json!({"id":"new","text":"新的问题"})))?,
                    "learning" => {
                        self.store
                            .lock()
                            .unwrap()
                            .reset_learning("group:10", self.now(), None)?
                    }
                    "orientation" => e.orientation.joined("group:10", self.now())?,
                    "activity" => *self.now.lock().unwrap() += 31.,
                    "expired" => *self.now.lock().unwrap() += 1001.,
                    "offline" => self.transport.lock().unwrap().online = false,
                    other => panic!("unknown effect {other}"),
                }
            }
            if m["invalidStage"] == stage {
                return Ok(m.get("invalid").cloned().unwrap_or(json!({})));
            }
            Ok(match stage {
                "FORM" => {
                    let mut formed = json!({"allocation":m.get("allocation").unwrap_or(&json!("open")),"candidates":if m["empty"]==true {json!([])} else {json!([{"kind":"system2","text":"建议从土壤湿度判断浇水"}])}});
                    if let Some(learning) = m.get("learning") {
                        formed["learning"] = learning.clone();
                    }
                    formed
                }
                "EVALUATE" => {
                    json!({"ratings":payload["candidates"].as_array().unwrap().iter().map(|c|json!({"id":c["id"],"motivation":m.get("score").unwrap_or(&json!(5)),"relevance":4,"originality":4,"for":["relevance","bad","coherence","balance"],"against":["balance"]})).collect::<Vec<_>>()})
                }
                "FORECAST" => {
                    json!({"shouldSend":m["veto"]!=true,"outcomes":{"reply":0.6,"silence":0.4,"negative":0},"responseMode":if m["veto"]==true {"wait"} else {"answer"},"plan":"接住当前问题"})
                }
                "ARTICULATE" => {
                    // 明确测试 P6a 排除项，而不是偷偷移植 lengthTarget 接线。
                    assert!(payload.get("lengthTarget").is_none());
                    json!({"text":m.get("reply").unwrap_or(&json!("可以先看看盆土是否已经干透。"))})
                }
                "ORIENT" => json!({"style":"谨慎接话","summary":"园艺讨论","topics":["园艺"]}),
                _ => panic!("unexpected stage"),
            })
        })
    }
}
fn setup(case: &Value) -> (Arc<Engine>, Arc<Harness>) {
    let store = Arc::new(Mutex::new(Store::in_memory().unwrap()));
    let h = Arc::new(Harness {
        now: Mutex::new(43200.),
        model: Mutex::new(json!({})),
        trace: Mutex::new(vec![]),
        store: store.clone(),
        engine: Mutex::new(Weak::new()),
        transport: Mutex::new(State {
            connected: true,
            online: true,
            self_id: "99".into(),
            reconnects: 0,
        }),
        sends: Mutex::new(0),
        budget: case["budget"].as_f64().unwrap_or(1000.),
        hold: Mutex::new(None),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let clock = h.clone();
    let logger = h.clone();
    let options = Options {
        now: Arc::new(move || clock.now()),
        random: Arc::new(|| 0.),
        expression_random: Arc::new(|| 0.5),
        activity_random: Arc::new(|| 0.),
        selection_random: Arc::new(|| 0.9),
        log: Arc::new(move |event, data| logger.push(json!(["log", event, data]))),
    };
    let c = Config::from_value(&merge(&defaults(), &case["config"])).unwrap();
    let engine = Engine::new(c, store, h.clone(), h.clone(), options).unwrap();
    *h.engine.lock().unwrap() = Arc::downgrade(&engine);
    (engine, h)
}
fn base(label: &str, agent: Value, steps: Vec<Value>) -> Value {
    json!({"label":label,"config":merge(&json!({"apiKey":"mock","onebotToken":"","dataDir":"unused","provider":{"model":"mock"},"agent":{"allowedGroups":["10","11"],"allowedUsers":["20"],"schedule":{"enabled":false},"rhythm":{"enabled":false},"observation":{"enabled":false},"learning":{"enabled":false},"dryRun":false,"quietHours":null,"debounceSeconds":0,"minThinkIntervalSeconds":1,"proactiveCooldownSeconds":0,"activeWindowSeconds":1000,"maxMessagesPerHour":100,"maxProactivePerHour":100,"sending":{"enabled":true,"addressedProbability":1,"proactiveProbability":1}}}),&json!({"agent":agent})),"steps":steps})
}
fn ingest(id: &str, text: &str) -> Value {
    json!({"op":"ingest","id":id,"text":text})
}
fn run() -> Value {
    json!({"op":"run"})
}
fn model(v: Value) -> Value {
    json!({"op":"model","value":v})
}
fn advance(n: f64) -> Value {
    json!({"op":"advance","seconds":n})
}
fn scenarios() -> Vec<Value> {
    let direct = || ingest("m1", "[CQ:at,qq=99]怎么浇水？");
    let open = || ingest("m1", "怎么浇水？");
    let mut cases = vec![
        base(
            "addressed",
            json!({}),
            vec![direct(), run(), advance(61.), run()],
        ),
        base("open", json!({}), vec![open(), advance(10.), run()]),
        base(
            "burst",
            json!({"debounceSeconds":2}),
            vec![
                direct(),
                run(),
                ingest("m2", "补充一下"),
                ingest("m2", "重复事件"),
                advance(2.),
                run(),
            ],
        ),
        base(
            "cooldown",
            json!({"proactiveCooldownSeconds":60}),
            vec![json!({"op":"seed"}), open(), run(), advance(61.), run()],
        ),
        base(
            "quiet_open",
            json!({"quietHours":{"start":11,"end":13,"timezone":"UTC"}}),
            vec![open(), run()],
        ),
        base(
            "quiet_addressed",
            json!({"quietHours":{"start":11,"end":13,"timezone":"UTC"}}),
            vec![direct(), run()],
        ),
        base(
            "quota",
            json!({"maxMessagesPerHour":1}),
            vec![json!({"op":"seed"}), direct(), run()],
        ),
        base(
            "proactive_quota",
            json!({"maxProactivePerHour":0}),
            vec![open(), run()],
        ),
        base(
            "uncertain",
            json!({}),
            vec![
                model(json!({"delivery":"uncertain"})),
                direct(),
                run(),
                advance(61.),
                run(),
                json!({"op":"restore"}),
                run(),
            ],
        ),
        base(
            "failed",
            json!({}),
            vec![
                model(json!({"delivery":"failed"})),
                direct(),
                run(),
                advance(61.),
                run(),
            ],
        ),
        base(
            "dry_run",
            json!({"dryRun":true}),
            vec![direct(), run(), advance(61.), run()],
        ),
        base(
            "explicit_other",
            json!({"threshold":3,"interruptThreshold":4.5,"system1Probability":0}),
            vec![
                model(json!({"allocation":"self","score":3.5})),
                ingest("m1", "[CQ:at,qq=30]怎么浇水"),
                run(),
            ],
        ),
        base(
            "explicit_self",
            json!({}),
            vec![
                model(json!({"allocation":"other","score":1})),
                direct(),
                run(),
            ],
        ),
        base(
            "model_self_proactive_off",
            json!({"proactive":false}),
            vec![model(json!({"allocation":"self"})), open(), run()],
        ),
        base(
            "withhold_retained",
            json!({"threshold":4,"system1Probability":0}),
            vec![
                model(json!({"score":1})),
                open(),
                run(),
                advance(2.),
                model(json!({"empty":true,"score":5})),
                ingest("m2", "[CQ:at,qq=99]请继续"),
                run(),
            ],
        ),
        base(
            "forecast_withhold",
            json!({}),
            vec![
                model(json!({"veto":true})),
                direct(),
                run(),
                advance(61.),
                run(),
            ],
        ),
        base(
            "sending_disabled",
            json!({"sending":{"enabled":false}}),
            vec![direct(), run()],
        ),
        base(
            "empty",
            json!({}),
            vec![model(json!({"empty":true})), direct(), run()],
        ),
        base(
            "isolation",
            json!({"maxConcurrentChats":1}),
            vec![
                model(json!({"score":1})),
                open(),
                run(),
                json!({"op":"ingest","group":11,"id":"other","text":"[CQ:at,qq=99]你觉得呢"}),
                run(),
            ],
        ),
        base(
            "orientation_gate",
            json!({"observation":{"enabled":true,"minSeconds":100,"minMessages":100}}),
            vec![direct(), run()],
        ),
        base(
            "join_duplicate",
            json!({"observation":{"enabled":true}}),
            vec![direct(), json!({"op":"notice"}), json!({"op":"notice"})],
        ),
        base(
            "learning",
            json!({"learning":{"enabled":true,"minMessages":1,"intervalSeconds":30}}),
            vec![
                model(json!({"learning":{"layers":[],"expressions":[]}})),
                direct(),
                run(),
            ],
        ),
        base(
            "learning_rejected",
            json!({"learning":{"enabled":true,"minMessages":1,"intervalSeconds":30}}),
            vec![model(json!({"learning":{"layers":"bad"}})), direct(), run()],
        ),
    ];
    for stage in ["FORM", "EVALUATE", "FORECAST", "ARTICULATE"] {
        cases.push(base(
            &format!("invalid_{stage}"),
            json!({}),
            vec![model(json!({"invalidStage":stage})), direct(), run()],
        ));
        cases.push(base(
            &format!("obsolete_message_{stage}"),
            json!({}),
            vec![
                model(json!({"effectStage":stage,"effect":"message"})),
                direct(),
                run(),
            ],
        ));
    }
    for (effect, agent) in [
        ("learning", json!({})),
        ("orientation", json!({})),
        (
            "activity",
            json!({"rhythm":{"enabled":true,"dayProbability":1,"activeMinSeconds":30,"activeMaxSeconds":30}}),
        ),
        ("expired", json!({})),
        ("offline", json!({})),
    ] {
        cases.push(base(
            &format!("obsolete_{effect}"),
            agent,
            vec![
                model(json!({"effectStage":"ARTICULATE","effect":effect})),
                direct(),
                run(),
            ],
        ));
    }
    for value in [
        json!({"ratings":[]}),
        json!({"ratings":[{"id":"unknown","motivation":5,"relevance":5,"originality":5}]}),
    ] {
        cases.push(base(
            "invalid_ratings",
            json!({}),
            vec![
                model(json!({"invalidStage":"EVALUATE","invalid":value})),
                direct(),
                run(),
            ],
        ));
    }
    for value in [
        json!({"text":"<ThInK>secret</ThInK>"}),
        json!({"text":"  "}),
        json!({"text":5}),
    ] {
        cases.push(base(
            "invalid_text",
            json!({}),
            vec![
                model(json!({"invalidStage":"ARTICULATE","invalid":value})),
                direct(),
                run(),
            ],
        ));
    }
    let mut budget = base(
        "api_budget",
        json!({}),
        vec![direct(), run(), advance(61.), run()],
    );
    budget["budget"] = json!(1);
    cases.push(budget);
    cases
}
async fn script(case: &Value) -> Value {
    let (e, h) = setup(case);
    for step in case["steps"].as_array().unwrap() {
        match step["op"].as_str().unwrap() {
            "model"=>*h.model.lock().unwrap()=step["value"].clone(),
            "ingest"=>e.ingest(&h.event(step)).unwrap(),
            "advance"=>*h.now.lock().unwrap()+=step["seconds"].as_f64().unwrap(),
            "run"=>{e.tick().unwrap();e.wait_idle().await;},
            "seed"=>{let db=h.store.lock().unwrap();let id=db.delivery("group:10",step["proactive"]!=false,h.now()).unwrap();db.finish_delivery(&id,step["status"].as_str().unwrap_or("sent"),None).unwrap();},
            "restore"=>e.restore().unwrap(),
            "ready"=>{let r=e.orientation.ensure("group:10").unwrap();h.store.lock().unwrap().orientation_ready("group:10",r.epoch,&json!({})).unwrap();},
            "notice"=>e.ingest(&json!({"post_type":"notice","notice_type":"group_increase","self_id":99,"user_id":99,"group_id":10,"time":h.now()})).unwrap(),
            other=>panic!("unknown op {other}"),
        }
    }
    let mut decisions = h.rows("SELECT chat,action,score,tags FROM decisions ORDER BY rowid");
    for r in &mut decisions {
        r["tags"] = serde_json::from_str(r["tags"].as_str().unwrap()).unwrap();
    }
    let output = json!({"label":case["label"],"trace":h.trace.lock().unwrap().clone(),"decisions":decisions,"deliveries":h.rows("SELECT chat,proactive,status FROM deliveries ORDER BY rowid"),"assessments":h.rows("SELECT chat,human_id,status FROM send_assessments ORDER BY rowid"),"thoughts":h.rows("SELECT chat,text,kind,used,score,subject FROM thoughts ORDER BY rowid"),"handled":h.rows("SELECT * FROM handled ORDER BY chat"),"chats":e.chats(),"calls":h.rows("SELECT count(*) n FROM calls")[0]["n"],"lastError":e.last_error()});
    e.stop().await;
    output
}
fn equivalent(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => assert!(
            (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() < 1e-9,
            "{path}: {actual} != {expected}"
        ),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}: {actual} != {expected}");
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                equivalent(a, b, &format!("{path}/{i}"));
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.keys().collect::<Vec<_>>(),
                b.keys().collect::<Vec<_>>(),
                "{path}"
            );
            for (k, a) in a {
                equivalent(a, &b[k], &format!("{path}/{k}"));
            }
        }
        _ => assert_eq!(actual, expected, "{path}"),
    }
}
#[tokio::test]
async fn scripted_conversations_match_real_js_decision_by_decision() {
    let cases = scenarios();
    let mut actual = Vec::new();
    for c in &cases {
        let mut result = script(c).await;
        // P7a 新增 Rust 控制事件，JS 无此接口；单独行为测试，不纳入旧日志 oracle。
        result["trace"]
            .as_array_mut()
            .unwrap()
            .retain(|entry| entry[0] != "log" || entry[1] != "decision");
        actual.push(result);
    }
    // Node 缺失只跳过 oracle，Rust 场景仍全部执行。
    if !Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("SKIP engine JS golden: node unavailable");
        return;
    }
    let mut child = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            include_str!("fixtures/engine-oracle.mjs"),
        ])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!(cases).to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(&expected) {
        equivalent(a, b, a["label"].as_str().unwrap());
    }
}
fn actions(v: &Value) -> Vec<&str> {
    v["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["action"].as_str().unwrap())
        .collect()
}
fn stages(v: &Value) -> Vec<&str> {
    v["trace"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r[0] == "model")
        .map(|r| r[1].as_str().unwrap())
        .collect()
}
#[tokio::test]
async fn decision_and_delivery_invariants_without_node() {
    for case in scenarios() {
        let label = case["label"].as_str().unwrap();
        let out = script(&case).await;
        let actions = actions(&out);
        let calls = stages(&out);
        match label {
            "addressed" | "burst" | "quiet_addressed" | "explicit_self" => {
                assert_eq!(actions, ["sent"], "{label}");
                assert_eq!(calls, ["FORM", "EVALUATE", "FORECAST", "ARTICULATE"]);
                assert_eq!(out["chats"][0][1]["pending"], false);
                assert_eq!(out["chats"][0][1]["pauseDone"], true);
            }
            "quiet_open" | "quota" | "proactive_quota" | "model_self_proactive_off" => {
                assert!(calls.is_empty(), "{label}");
                assert!(actions.is_empty());
            }
            "uncertain" | "failed" => {
                assert_eq!(
                    actions,
                    if label == "uncertain" {
                        vec!["delivery_uncertain"]
                    } else {
                        vec!["delivery_failed"]
                    }
                );
                assert_eq!(out["deliveries"].as_array().unwrap().len(), 1);
                assert_eq!(calls.len(), 4, "再次 tick / restore 不得重发");
                assert_eq!(out["chats"][0][1]["pending"], false);
            }
            "dry_run" => {
                assert_eq!(actions, ["dry_run"]);
                assert!(out["deliveries"].as_array().unwrap().is_empty());
                assert_eq!(out["thoughts"][0]["used"], 1);
            }
            "explicit_other" => {
                assert_eq!(actions, ["withhold"]);
                assert_eq!(out["thoughts"][0]["used"], 0);
            }
            "withhold_retained" => {
                assert_eq!(actions, ["withhold", "sent"]);
                assert_eq!(
                    out["thoughts"].as_array().unwrap().len(),
                    1,
                    "保留候选应在下一轮重新评估"
                );
            }
            "forecast_withhold" => {
                assert_eq!(actions, ["forecast_withhold"]);
                assert_eq!(calls.len(), 3);
                assert_eq!(out["chats"][0][1]["pauseDone"], true);
            }
            "api_budget" => {
                assert_eq!(out["calls"], 1);
                assert_eq!(out["lastError"], "hourly_api_budget");
                assert!(out["deliveries"].as_array().unwrap().is_empty());
            }
            "isolation" => {
                assert_eq!(actions, ["withhold", "sent"]);
                assert_eq!(out["thoughts"][0]["chat"], "group:10");
                assert_eq!(out["thoughts"][0]["used"], 0);
                assert_eq!(out["thoughts"][1]["chat"], "group:11");
            }
            "orientation_gate" => {
                assert!(calls.is_empty());
                assert_eq!(out["chats"][0][1]["due"], 43205.);
                assert_eq!(out["chats"][0][1]["pending"], true);
            }
            "join_duplicate" => {
                assert_eq!(out["chats"][0][1]["version"], 2);
                assert_eq!(out["chats"][0][1]["pending"], false);
            }
            "obsolete_message_FORM" => {
                assert!(out["thoughts"].as_array().unwrap().is_empty());
                assert_eq!(out["chats"][0][1]["pending"], true);
                assert!(out["handled"].as_array().unwrap().is_empty());
            }
            l if l.starts_with("obsolete_") => {
                assert!(actions.is_empty(), "{label}");
                assert!(out["deliveries"].as_array().unwrap().is_empty());
            }
            l if l.starts_with("invalid_") => {
                assert!(actions.is_empty());
                assert!(out["lastError"].as_str().unwrap().starts_with("invalid_"));
            }
            _ => {}
        }
    }
}
#[tokio::test]
async fn interrupt_threshold_is_stricter_and_scores_stay_bounded() {
    let run_with = |hint: &str, score: f64| {
        base(
            "threshold",
            json!({"threshold":3,"interruptThreshold":4.5,"system1Probability":0}),
            vec![
                model(json!({"score":score})),
                ingest("m1", hint),
                advance(10.),
                run(),
            ],
        )
    };
    let open = script(&run_with("讨论浇水", 3.5)).await;
    let other = script(&run_with("[CQ:at,qq=30]讨论浇水", 3.5)).await;
    let high = script(&run_with("[CQ:at,qq=30]讨论浇水", 5.)).await;
    assert_eq!(actions(&open), ["sent"]);
    assert_eq!(actions(&other), ["withhold"]);
    assert_eq!(actions(&high), ["sent"]);
    let score = open["decisions"][0]["score"].as_f64().unwrap();
    assert!((3.5..=5.).contains(&score));
    assert!(high["decisions"][0]["score"].as_f64().unwrap() >= score);
}
async fn entered(h: &Harness) {
    tokio::time::timeout(std::time::Duration::from_secs(3), h.entered.notified())
        .await
        .expect("模型必须到达屏障");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_chat_single_flight_and_cross_chat_limit() {
    for limit in [1, 2] {
        let (e, h) = setup(&base(
            "concurrency",
            json!({"maxConcurrentChats":limit}),
            vec![],
        ));
        *h.hold.lock().unwrap() = Some("FORM".into());
        e.ingest(&h.event(&json!({"group":10}))).unwrap();
        e.tick().unwrap();
        entered(&h).await;
        e.ingest(&h.event(&json!({"group":11}))).unwrap();
        for _ in 0..5 {
            e.tick().unwrap();
        }
        if limit == 2 {
            entered(&h).await;
        }
        assert_eq!(
            e.chats().iter().filter(|(_, s)| s.busy).count(),
            limit as usize
        );
        assert_eq!(
            h.trace
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r[0] == "model")
                .count(),
            limit as usize
        );
        // 第一群有新消息，老 FORM 结果必须作废；busy 仍不能被第二次 tick 绕过。
        e.ingest(&h.event(&json!({"id":"new"}))).unwrap();
        e.tick().unwrap();
        *h.hold.lock().unwrap() = None;
        h.release.add_permits(limit as usize);
        e.wait_idle().await;
        assert!(e.chats().iter().all(|(_, s)| !s.busy));
        assert!(h
            .rows("SELECT chat FROM deliveries")
            .iter()
            .all(|r| r["chat"] != "group:10"));
        e.tick().unwrap();
        e.wait_idle().await;
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(h.rows("SELECT chat FROM deliveries ORDER BY chat").len(), 2);
        e.stop().await;
    }
}
#[tokio::test]
async fn cancellation_leaves_no_late_candidates_and_keeps_budget() {
    for stage in ["FORM", "EVALUATE", "FORECAST", "ARTICULATE"] {
        let (e, h) = setup(&base("stop", json!({}), vec![]));
        *h.hold.lock().unwrap() = Some(stage.into());
        e.ingest(&h.event(&json!({}))).unwrap();
        e.tick().unwrap();
        entered(&h).await;
        let calls = h.rows("SELECT count(*) n FROM calls")[0]["n"].clone();
        tokio::time::timeout(std::time::Duration::from_secs(3), e.stop())
            .await
            .unwrap();
        h.release.add_permits(1);
        e.tick().unwrap();
        e.wait_idle().await;
        assert!(h.rows("SELECT * FROM deliveries").is_empty());
        assert_eq!(
            h.rows("SELECT count(*) n FROM calls")[0]["n"],
            calls,
            "取消不退预算"
        );
        assert!(!e.chats()[0].1.busy);
        if stage == "FORM" {
            assert!(h.rows("SELECT * FROM thoughts").is_empty());
        }
    }
}
#[tokio::test]
async fn restore_keeps_memory_without_replaying_and_reload_drops_old_result() {
    let case = base("reload", json!({}), vec![]);
    let (old, h) = setup(&case);
    old.ingest(&h.event(&json!({}))).unwrap();
    h.store
        .lock()
        .unwrap()
        .note("group:10", "仅本群的笔记", h.now())
        .unwrap();
    *h.hold.lock().unwrap() = Some("FORM".into());
    old.tick().unwrap();
    entered(&h).await;
    old.stop().await;
    let clock = h.clone();
    let options = Options {
        now: Arc::new(move || clock.now()),
        ..Options::default()
    };
    let new = Engine::new(
        old.config.clone(),
        h.store.clone(),
        h.clone(),
        h.clone(),
        options,
    )
    .unwrap();
    new.inherit_chats(old.chats());
    assert!(!new.chats()[0].1.busy);
    assert_eq!(new.chats()[0].1.last_think, 0.);
    *h.hold.lock().unwrap() = None;
    h.release.add_permits(1);
    assert!(h.rows("SELECT * FROM thoughts").is_empty());
    let clock = h.clone();
    let fresh = Engine::new(
        old.config.clone(),
        h.store.clone(),
        h.clone(),
        h.clone(),
        Options {
            now: Arc::new(move || clock.now()),
            ..Options::default()
        },
    )
    .unwrap();
    fresh.restore().unwrap();
    fresh.tick().unwrap();
    fresh.wait_idle().await;
    assert_eq!(fresh.chats()[0].1.last_id, "m1");
    assert!(!fresh.chats()[0].1.pending);
    assert!(fresh.chats()[0].1.pause_done);
    assert_eq!(h.rows("SELECT * FROM notes").len(), 1);
    assert!(h.rows("SELECT * FROM deliveries").is_empty());
    new.stop().await;
    fresh.stop().await;
}
#[tokio::test]
async fn active_cap_debounce_min_interval_and_cold_cleanup() {
    let (e, h) = setup(&base(
        "scheduling",
        json!({"maxActiveChats":1,"debounceSeconds":2,"minThinkIntervalSeconds":60,"system1Probability":0}),
        vec![],
    ));
    *h.model.lock().unwrap() = json!({"score":1});
    let first = h.event(&json!({"text":"开放讨论"}));
    e.ingest(&first).unwrap();
    e.ingest(&first).unwrap();
    e.ingest(&h.event(&json!({"group":11}))).unwrap();
    assert_eq!(e.chats().len(), 1);
    assert_eq!(e.chats()[0].1.version, 1);
    assert!(
        h.rows("SELECT * FROM calls").is_empty(),
        "ingest 不能请求模型"
    );
    e.tick().unwrap();
    e.wait_idle().await;
    assert!(h.rows("SELECT * FROM calls").is_empty());
    *h.now.lock().unwrap() += 2.;
    e.tick().unwrap();
    e.wait_idle().await;
    let calls = h.rows("SELECT count(*) n FROM calls")[0]["n"].clone();
    e.ingest(&h.event(&json!({"id":"m2","text":"继续讨论"})))
        .unwrap();
    *h.now.lock().unwrap() += 2.;
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(h.rows("SELECT count(*) n FROM calls")[0]["n"], calls);
    // 点名绕过最小思考间隔，但仍遵守 debounce。
    e.ingest(&h.event(&json!({"id":"m3"}))).unwrap();
    *h.now.lock().unwrap() += 2.;
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(h.rows("SELECT * FROM deliveries").len(), 1);
    *h.now.lock().unwrap() += 1001.;
    e.tick().unwrap();
    assert!(e.chats().is_empty());
    e.stop().await;
}
// 真实网络不可使用；用 spawn_blocking + Condvar 复现 Provider 无法中断的传输边界。
struct BlockingProvider {
    harness: Arc<Harness>,
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
    finished: Arc<Notify>,
}
impl OrientationProvider for BlockingProvider {
    fn json<'a>(&'a self, _: &'a str, _: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            let gate = self.gate.clone();
            let h = self.harness.clone();
            let finished = self.finished.clone();
            Ok(tokio::task::spawn_blocking(move|| {
                assert!(h.store.lock().unwrap().call_budget(h.now(),100.).unwrap());
                h.entered.notify_one();
                let (lock,cond)=&*gate;let mut released=lock.lock().unwrap();
                while !*released {released=cond.wait(released).unwrap();}
                finished.notify_one();
                json!({"allocation":"self","candidates":[{"kind":"system2","text":"迟到的阻塞结果"}]})
            }).await?)
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocking_request_finishes_after_stop_but_cannot_commit() {
    let (original, h) = setup(&base("blocking", json!({}), vec![]));
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let finished = Arc::new(Notify::new());
    let provider = Arc::new(BlockingProvider {
        harness: h.clone(),
        gate: gate.clone(),
        finished: finished.clone(),
    });
    let clock = h.clone();
    let e = Engine::new(
        original.config.clone(),
        h.store.clone(),
        provider,
        h.clone(),
        Options {
            now: Arc::new(move || clock.now()),
            ..Options::default()
        },
    )
    .unwrap();
    e.ingest(&h.event(&json!({}))).unwrap();
    e.tick().unwrap();
    entered(&h).await;
    let stop = tokio::time::timeout(std::time::Duration::from_secs(3), e.stop()).await;
    // 先释放线程，再断言，避免测试失败时悬挂 Tokio shutdown。
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    stop.expect("stop 只能取消等待，不能等待不可取消的阻塞请求");
    tokio::time::timeout(std::time::Duration::from_secs(3), finished.notified())
        .await
        .unwrap();
    assert_eq!(h.rows("SELECT count(*) n FROM calls")[0]["n"], 1);
    assert!(h.rows("SELECT * FROM thoughts").is_empty());
    assert!(h.rows("SELECT * FROM deliveries").is_empty());
}
#[tokio::test]
async fn candidate_unicode_boundary_is_an_explicit_port_difference() {
    let (e, h) = setup(&base("unicode", json!({}), vec![]));
    *h.model.lock().unwrap() = json!({"invalidStage":"FORM","invalid":{"allocation":"self","candidates":[{"kind":"system2","text":"😀".repeat(301)}]}});
    e.ingest(&h.event(&json!({}))).unwrap();
    e.tick().unwrap();
    e.wait_idle().await;
    let rows = h.rows("SELECT text FROM thoughts");
    // 既有 policy::clip_chars 按标量字符，JS slice(0,300) 按 UTF-16（150 个 emoji）。
    assert_eq!(rows[0]["text"].as_str().unwrap().chars().count(), 300);
    assert_eq!(
        rows[0]["text"].as_str().unwrap().encode_utf16().count(),
        600
    );
    e.stop().await;
}
#[tokio::test]
async fn rest_tick_invalidates_version_and_orientation_epoch_is_independent() {
    let (e, h) = setup(&base(
        "rest",
        json!({"rhythm":{"enabled":true,"dayProbability":1,"activeMinSeconds":30,"activeMaxSeconds":30}}),
        vec![],
    ));
    *h.hold.lock().unwrap() = Some("FORM".into());
    e.ingest(&h.event(&json!({}))).unwrap();
    e.tick().unwrap();
    entered(&h).await;
    {
        let db = h.store.lock().unwrap();
        let mut block = db.activity_state().unwrap().unwrap();
        block.active = 0;
        db.save_activity(&block).unwrap();
    }
    let version = e.chats()[0].1.version;
    e.tick().unwrap();
    assert_eq!(e.chats()[0].1.version, version + 1);
    assert!(!e.chats()[0].1.pending);
    assert!(e.chats()[0].1.pause_done);
    h.release.add_permits(1);
    e.wait_idle().await;
    assert!(h.rows("SELECT * FROM thoughts").is_empty());
    e.stop().await;
    let (e, h) = setup(&base("epoch", json!({}), vec![]));
    *h.hold.lock().unwrap() = Some("FORM".into());
    e.ingest(&h.event(&json!({}))).unwrap();
    e.tick().unwrap();
    entered(&h).await;
    let version = e.chats()[0].1.version;
    e.orientation.joined("group:10", h.now()).unwrap();
    assert_eq!(
        e.chats()[0].1.version,
        version,
        "直接 epoch 重置不靠 version 检查兜底"
    );
    h.release.add_permits(1);
    e.wait_idle().await;
    assert!(h.rows("SELECT * FROM thoughts").is_empty());
    e.stop().await;
}
#[tokio::test]
async fn message_before_first_task_poll_does_not_bypass_debounce() {
    let (e, h) = setup(&base("admission", json!({"debounceSeconds":2}), vec![]));
    e.ingest(&h.event(&json!({}))).unwrap();
    *h.now.lock().unwrap() += 2.;
    e.tick().unwrap();
    // current_thread 调度器尚未 poll 子任务，等价 JS cycle 首次 await 后立即入站。
    e.ingest(&h.event(&json!({"id":"new"}))).unwrap();
    e.wait_idle().await;
    assert!(h.rows("SELECT * FROM calls").is_empty());
    assert!(e.chats()[0].1.pending);
    e.tick().unwrap();
    e.wait_idle().await;
    assert!(h.rows("SELECT * FROM calls").is_empty());
    *h.now.lock().unwrap() += 2.;
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(h.rows("SELECT * FROM deliveries").len(), 1);
    e.stop().await;
}

#[tokio::test]
async fn control_decision_event_reports_below_threshold_withhold() {
    let result = script(&base(
        "control_event",
        json!({"threshold":4,"system1Probability":0}),
        vec![model(json!({"score":1})), ingest("m1", "怎么浇水？"), run()],
    ))
    .await;
    let events: Vec<_> = result["trace"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry[0] == "log" && entry[1] == "decision")
        .collect();
    assert_eq!(
        events,
        vec![&json!(["log","decision",{
            "chat":"group:10","action":"withhold","score":0.0,"tags":[],"ts":43200.0
        }])]
    );
}
