//! 不变量精确：阶段顺序、发送次数、状态与作用域。评分只验证方向/区间；
//! golden 数值容差仅处理跨语言浮点表示，不把启发式分数当质量真值。
#[path = "golden/mod.rs"]
mod golden;
use anyhow::Result;
use futures_util::future::BoxFuture;
use qq_inner_core::{
    config::{defaults, merge, Config},
    engine::orientation::{OrientationProvider, OrientationTransport},
    engine::{Engine, EngineTransport, Options},
    transport::{OneBotError, State},
    store::Store,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Notify, Semaphore};

struct Harness {
    now: Mutex<f64>,
    model: Mutex<Value>,
    trace: Mutex<Vec<Value>>,
    payloads: Mutex<Vec<Value>>,
    model_inputs: Mutex<Vec<(String, Value)>>,
    store: Arc<Mutex<Store>>,
    engine: Mutex<Weak<Engine>>,
    transport: Mutex<State>,
    sends: Mutex<usize>,
    orientation_reads: Mutex<Vec<String>>,
    identity_reads: Mutex<Vec<(String, Value)>>,
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
    fn call<'a>(&'a self, action: &'a str, params: Value) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            if matches!(action, "get_login_info" | "get_group_member_info") {
                self.identity_reads.lock().unwrap().push((action.into(), params.clone()));
                let model = self.model.lock().unwrap();
                let response = if action == "get_login_info" {
                    &model["loginInfo"]
                } else {
                    &model["memberInfo"][params["group_id"].as_str().unwrap()]
                };
                anyhow::ensure!(!response.is_null(), "unsupported");
                return Ok(response.clone());
            }
            self.orientation_reads.lock().unwrap().push(action.into());
            let hold = self.hold.lock().unwrap().as_deref() == Some("COLLECT");
            if hold && action == "get_group_info" {
                self.entered.notify_one();
                self.release.acquire().await.unwrap().forget();
            }
            if action == "get_group_msg_history"
                && self.hold.lock().unwrap().as_deref() == Some("HISTORY")
            {
                self.entered.notify_one();
                self.release.acquire().await.unwrap().forget();
            }
            anyhow::bail!("unsupported")
        })
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
            if system.contains(qq_inner_core::topic::relay::links::REVIEW) {
                self.model_inputs.lock().unwrap().push(("RELAY".into(), payload));
                anyhow::ensure!(self.model.lock().unwrap()["relayError"] != true, "audit unavailable");
                if self.model.lock().unwrap()["relayConcurrentDuplicate"] == true {
                    self.store.lock().unwrap().execute(
                        "INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,expires) VALUES('during-audit','group:10','group','short_term','during-audit',?,'[]',?)",
                        rusqlite::params!["https://example.org/esp32", self.now() + 100.],
                    )?;
                }
                return Ok(self.model.lock().unwrap().get("relayAudit").cloned().unwrap_or(json!({"keep":[]})));
            }
            let stage = system
                .split("TASK: ")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap();
            self.model_inputs.lock().unwrap().push((stage.into(), payload.clone()));
            if matches!(stage, "FORM" | "ARTICULATE") {
                if let Some(expected) = self.model.lock().unwrap()["expectPersona"].as_str() {
                    assert!(payload["persona"].as_str().unwrap().contains(expected));
                    assert!(payload["persona"].as_str().unwrap().starts_with("基座种子"));
                }
            }
            if stage == "ARTICULATE" {
                if payload.get("backstories").is_some() {
                    assert!(system.contains(qq_inner_core::persona::backstory::RULE));
                }
                if payload.get("recallEvidence").is_some() {
                    assert!(system.contains("细节未经核实必须表达不确定"));
                }
                if self.model.lock().unwrap().get("affect").is_some() {
                    assert!(system.contains("直接、少修饰"));
                }
                self.payloads.lock().unwrap().push(payload.clone());
            }
            self.push(json!([
                "model",
                stage,
                payload["addressedHint"],
                payload["trigger"],
                payload["history"]
                    .as_array()
                    .map(|a| a.iter().map(|m| m["id"].clone()).collect::<Vec<_>>())
                    .unwrap_or_default(),
                payload["lengthTarget"]
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
                    for key in ["affect", "recall"] {
                        if let Some(value) = m.get(key) {
                            formed[key] = value.clone();
                        }
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
                    assert!(matches!(
                        payload["lengthTarget"].as_str(),
                        Some("tiny" | "short" | "medium" | "long")
                    ));
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
        payloads: Mutex::new(vec![]),
        model_inputs: Mutex::new(vec![]),
        store: store.clone(),
        engine: Mutex::new(Weak::new()),
        transport: Mutex::new(State {
            connected: true,
            online: true,
            self_id: "99".into(),
            reconnects: 0,
        }),
        sends: Mutex::new(0),
        orientation_reads: Mutex::new(Vec::new()),
        identity_reads: Mutex::new(Vec::new()),
        budget: case["budget"].as_f64().unwrap_or(1000.),
        hold: Mutex::new(None),
        entered: Notify::new(),
        release: Semaphore::new(0),
    });
    let draw_source = h.clone();
    let clock = h.clone();
    let logger = h.clone();
    let draws = Mutex::new(
        case["expressionDraws"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter(),
    );
    let options = Options {
        now: Arc::new(move || clock.now()),
        random: Arc::new(move || {
            draw_source.model.lock().unwrap()["decisionDraw"]
                .as_f64()
                .unwrap_or(0.)
        }),
        expression_random: Arc::new(move || {
            draws
                .lock()
                .unwrap()
                .next()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5)
        }),
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
    cases.push(base(
        "humanize_explicitly_disabled",
        json!({"emoji":{"learnFrequency":false,"faceOnly":false}}),
        vec![direct(), run()],
    ));
    // parity：每档边界及 self 禁 tiny；序列验证装饰先抽样、长度后抽样。
    for hint in ["self", "open", "other"] {
        for draw in [0., 0.349, 0.35, 0.699, 0.7, 0.799, 0.8, 0.979, 0.98, 0.999] {
            let content = match hint {
                "self" => "[CQ:at,qq=99]你好",
                "other" => "[CQ:at,qq=20]你好",
                _ => "你好",
            };
            let mut case = base(
                &format!("length_{hint}_{draw}"),
                json!({"personality":{"variants":[]},"emoji":{"enabled":true,"probability":1}}),
                vec![ingest("m1", content), run()],
            );
            case["expressionDraws"] = json!([0.1, draw]);
            cases.push(case);
        }
    }
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
    let mut cases = scenarios();
    // 不变量：省略开关与显式关闭都必须逐条匹配真实 JS，覆盖全部原有场景。
    let disabled: Vec<_> = cases
        .iter()
        .cloned()
        .map(|mut case| {
            case["label"] = json!(format!(
                "{}-layers-disabled",
                case["label"].as_str().unwrap()
            ));
            case["config"]["agent"]["threeLayerDecision"] = json!(false);
            case
        })
        .collect();
    cases.extend(disabled);
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
    // 固化金标准：完整阶段 trace 与落库结果，继续使用原 equivalent 的浮点规则。
    // Rust 长度权重已独立调整；engine.json 仅对应长度 expected 随之更新。
    let expected: Vec<Value> = serde_json::from_value(golden::expected(
        include_str!("golden/engine.json"),
        &json!(cases),
    ))
    .unwrap();
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
    e.ingest(&h.event(&json!({"text":"继续聊园艺"}))).unwrap();
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
    assert!(e.chats()[0].1.pending);
    assert!(!e.chats()[0].1.pause_done);
    h.release.add_permits(1);
    e.wait_idle().await;
    assert!(h.rows("SELECT * FROM thoughts").is_empty());
    e.stop().await;
    let (e, h) = setup(&base("epoch", json!({}), vec![]));
    *h.hold.lock().unwrap() = Some("FORM".into());
    e.ingest(&h.event(&json!({"text":"继续聊园艺"}))).unwrap();
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

fn face_case(enabled: bool) -> Value {
    base(
        "face_only",
        json!({"threshold":1,"interruptThreshold":1,"sending":{"enabled":false},"emoji":{"enabled":true,"faceOnly":enabled,"probability":1,"faceIds":["14"],"symbols":["🙂"],"cooldownSeconds":0}}),
        vec![],
    )
}
async fn face_turn(
    e: &Arc<Engine>,
    h: &Arc<Harness>,
    id: &str,
    content: &str,
    group: i64,
    score: f64,
    response: Value,
) {
    *h.model.lock().unwrap() =
        json!({"score":score,"invalidStage":"ARTICULATE","invalid":response});
    e.ingest(&h.event(&json!({"id":id,"text":content,"group":group})))
        .unwrap();
    e.tick().unwrap();
    e.wait_idle().await;
}
#[tokio::test]
async fn face_only_runtime_gate_and_hard_rejections() {
    for (enabled, content, score, response, permitted) in [
        (false, "哈哈", 3., json!({"text":"","faceId":"14"}), false),
        (true, "哈哈", 3., json!({"text":"","faceId":"14"}), true),
        (
            true,
            "[CQ:at,qq=20]哈哈",
            2.,
            json!({"text":" ","faceId":"14"}),
            true,
        ),
        (
            true,
            "[CQ:at,qq=99]哈哈",
            3.,
            json!({"text":"","faceId":"14"}),
            false,
        ),
        (true, "帮帮我", 3., json!({"text":"","faceId":"14"}), false),
        (
            true,
            "有点难过",
            3.,
            json!({"text":"","faceId":"14"}),
            false,
        ),
        (
            true,
            "哈哈但我真的很难过",
            3.,
            json!({"text":"","faceId":"14"}),
            false,
        ),
        (true, "哈哈", 4., json!({"text":"","faceId":"14"}), false),
        (true, "哈哈", 3., json!({"text":"","faceId":"999"}), false),
        (true, "哈哈", 3., json!({"text":"","faceId":["14"]}), false),
        (
            true,
            "哈哈",
            3.,
            json!({"text":"","emoji":"🙂","faceId":"14"}),
            false,
        ),
        (true, "哈哈", 3., json!({"text":"","emoji":"🙂"}), false),
    ] {
        let (e, h) = setup(&face_case(enabled));
        face_turn(&e, &h, "m1", content, 10, score, response).await;
        assert_eq!(
            *h.sends.lock().unwrap(),
            usize::from(permitted),
            "{enabled} {content} {score}"
        );
        let payload = h
            .payloads
            .lock()
            .unwrap()
            .last()
            .expect("must reach articulation")
            .clone();
        assert_eq!(payload.get("runtimeInstructions").is_some(), enabled);
        if permitted {
            assert!(h
                .trace
                .lock()
                .unwrap()
                .contains(&json!(["send", "group:10", "", "14"])));
            assert_eq!(h.rows("SELECT * FROM deliveries").len(), 1);
        }
        e.stop().await;
    }
}
#[tokio::test]
async fn face_only_streak_survives_reload_and_resets_only_with_text() {
    let (old, h) = setup(&face_case(true));
    face_turn(
        &old,
        &h,
        "a",
        "哈哈",
        10,
        3.,
        json!({"text":"","faceId":"14"}),
    )
    .await;
    old.stop().await;
    // 历史被保留期清理也不能重置连续单 face 限制。
    h.store
        .lock()
        .unwrap()
        .execute("DELETE FROM messages WHERE self=1", [])
        .unwrap();
    *h.now.lock().unwrap() += 61.;
    let clock = h.clone();
    let e = Engine::new(
        old.config.clone(),
        h.store.clone(),
        h.clone(),
        h.clone(),
        Options {
            now: Arc::new(move || clock.now()),
            expression_random: Arc::new(|| 0.5),
            ..Options::default()
        },
    )
    .unwrap();
    *h.engine.lock().unwrap() = Arc::downgrade(&e);
    face_turn(
        &e,
        &h,
        "b",
        "确实",
        10,
        3.,
        json!({"text":"","faceId":"14"}),
    )
    .await;
    assert_eq!(*h.sends.lock().unwrap(), 1);
    face_turn(
        &e,
        &h,
        "c",
        "哈哈",
        11,
        3.,
        json!({"text":"","faceId":"14"}),
    )
    .await;
    assert_eq!(*h.sends.lock().unwrap(), 2, "不同群不共享连续限制");
    *h.now.lock().unwrap() += 61.;
    face_turn(
        &e,
        &h,
        "d",
        "同感",
        10,
        3.,
        json!({"text":"这段确实有意思"}),
    )
    .await;
    assert_eq!(*h.sends.lock().unwrap(), 3);
    *h.now.lock().unwrap() += 61.;
    face_turn(
        &e,
        &h,
        "e",
        "哈哈",
        10,
        3.,
        json!({"text":"","faceId":"14"}),
    )
    .await;
    assert_eq!(*h.sends.lock().unwrap(), 4, "正文之后可再次单 face");
    e.stop().await;
}
#[tokio::test]
async fn face_learning_captures_human_segments_and_default_does_not_create_tables() {
    for enabled in [false, true] {
        let mut case = face_case(false);
        case["config"]["agent"]["emoji"]["learnFrequency"] = json!(enabled);
        let (e, h) = setup(&case);
        for (id, content) in [
            ("a", json!([{"type":"face","data":{"id":"14"}}])),
            ("b", json!("[CQ:face,id=14]")),
            ("c", json!("hi")),
        ] {
            e.ingest(&h.event(&json!({"id":id,"text":content})))
                .unwrap();
        }
        if enabled {
            assert_eq!(
                h.rows("SELECT sum(has_face) n FROM humanize_faces")[0]["n"],
                2
            );
        } else {
            assert!(h
                .rows("SELECT name FROM sqlite_master WHERE name='humanize_faces'")
                .is_empty());
        }
        e.stop().await;
    }
}

#[tokio::test]
async fn face_only_keeps_quota_cooldown_and_stale_output_guards() {
    for patch in [
        json!({"dryRun":true}),
        json!({"maxMessagesPerHour":1}),
        json!({"quietHours":{"start":11,"end":13,"timezone":"UTC"}}),
        json!({}),
    ] {
        let mut case = face_case(true);
        case["config"]["agent"] = merge(&case["config"]["agent"], &patch);
        let (e, h) = setup(&case);
        if patch["maxMessagesPerHour"] == 1 {
            let db = h.store.lock().unwrap();
            let id = db.delivery("group:10", false, h.now()).unwrap();
            db.finish_delivery(&id, "sent", None).unwrap();
        }
        if patch == json!({}) {
            *h.model.lock().unwrap() = json!({"score":3,"effectStage":"ARTICULATE","effect":"message","invalidStage":"ARTICULATE","invalid":{"text":"","faceId":"14"}});
            e.ingest(&h.event(&json!({"text":"哈哈"}))).unwrap();
            e.tick().unwrap();
            e.wait_idle().await;
        } else {
            face_turn(
                &e,
                &h,
                "m1",
                "哈哈",
                10,
                3.,
                json!({"text":"","faceId":"14"}),
            )
            .await;
        }
        assert_eq!(*h.sends.lock().unwrap(), 0, "{patch}");
        assert!(h.rows("SELECT * FROM humanize_reply_state").is_empty());
        e.stop().await;
    }
    // 上条有正文时也保留至少 30 秒间隔。
    let (e, h) = setup(&face_case(true));
    face_turn(&e, &h, "a", "哈哈", 10, 3., json!({"text":"确实挺有意思"})).await;
    *h.now.lock().unwrap() += 2.;
    face_turn(
        &e,
        &h,
        "b",
        "同感",
        10,
        3.,
        json!({"text":"","faceId":"14"}),
    )
    .await;
    assert_eq!(*h.sends.lock().unwrap(), 1);
    e.stop().await;
}
#[tokio::test]
async fn uncertain_face_delivery_blocks_next_face_but_definite_failure_does_not() {
    for status in ["uncertain", "failed"] {
        let (e, h) = setup(&face_case(true));
        *h.model.lock().unwrap() = json!({"score":3,"delivery":status,"invalidStage":"ARTICULATE","invalid":{"text":"","faceId":"14"}});
        e.ingest(&h.event(&json!({"text":"哈哈"}))).unwrap();
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(*h.sends.lock().unwrap(), 1);
        *h.now.lock().unwrap() += 61.;
        face_turn(
            &e,
            &h,
            "b",
            "同感",
            10,
            3.,
            json!({"text":"","faceId":"14"}),
        )
        .await;
        assert_eq!(
            *h.sends.lock().unwrap(),
            if status == "uncertain" { 1 } else { 2 }
        );
        e.stop().await;
    }
}

#[tokio::test]
async fn owner_teaching_commands_are_consumed_and_sources_are_special() {
    let case = base(
        "teaching",
        json!({"ownerTeaching":{"enabled":true,"ownerUin":"20"},"maxInputChars":100}),
        vec![],
    );
    let (engine, h) = setup(&case);
    let commands = [
        "/黑话 A=B".to_string(),
        "/黑话 A = 新意思".into(),
        "/记住 喜欢Rust".into(),
        "/黑话 缺等号".into(),
        "/记住".into(),
        format!("/记住 {}", "长".repeat(501)),
    ];
    for (n, command) in commands.iter().enumerate() {
        let event = json!({"post_type":"message","message_type":"private","self_id":99,"user_id":20,"message_id":format!("real-id-{n}"),"time":h.now(),"message":command});
        engine.ingest(&event).unwrap();
        engine.ingest(&event).unwrap(); // 去重不能执行或确认两次。
        engine.tick().unwrap();
        engine.wait_idle().await;
    }
    assert!(engine.chats().is_empty());
    let rows = h.rows("SELECT * FROM expressions");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["meaning"], "新意思");
    assert_eq!(rows[0]["kind"], "jargon");
    let memory = h.rows("SELECT * FROM memory_layers");
    assert_eq!(memory.len(), 1);
    assert_eq!(memory[0]["layer"], "long_term");
    assert_eq!(memory[0]["subject"], "person:20");
    assert_eq!(memory[0]["text"], "喜欢Rust");
    for row in rows.iter().chain(memory.iter()) {
        let sources: Value = serde_json::from_str(row["sources"].as_str().unwrap()).unwrap();
        assert_eq!(sources, json!(["owner-teaching"]));
        assert!(!row["sources"].as_str().unwrap().contains("real-id"));
    }
    let trace = h.trace.lock().unwrap().clone();
    assert!(!trace.iter().any(|r| r[0] == "model"));
    let replies: Vec<_> = trace.iter().filter(|r| r[0] == "send").collect();
    assert_eq!(replies.len(), commands.len());
    assert!(replies[..3].iter().all(|r| r[2] == "记住了"));
    assert!(replies[3..]
        .iter()
        .all(|r| r[2].as_str().unwrap().starts_with("没看懂：")));
    for (id, word) in [("forget1", "A"), ("forget2", "Rust")] {
        engine.ingest(&json!({"post_type":"message","message_type":"private","self_id":99,"user_id":20,"message_id":id,"time":h.now(),"message":format!("/忘记 {word}")})).unwrap();
        engine.tick().unwrap();
        engine.wait_idle().await;
    }
    assert!(h.rows("SELECT * FROM expressions").is_empty());
    assert!(h.rows("SELECT * FROM memory_layers").is_empty());
}

#[test]
fn owner_teaching_disabled_or_unauthorized_remains_normal_chat() {
    for (settings, kind, sender) in [
        (json!({}), "private", 20),
        (json!({"enabled":true}), "private", 20),
        (json!({"enabled":true,"ownerUin":"20"}), "group", 20),
        (json!({"enabled":true,"ownerUin":"21"}), "group", 20),
    ] {
        let (engine, h) = setup(&base("boundary", json!({"ownerTeaching":settings}), vec![]));
        engine.ingest(&json!({"post_type":"message","message_type":kind,"self_id":99,"user_id":sender,"group_id":10,"message_id":"boundary","time":h.now(),"message":"/黑话 A=B"})).unwrap();
        let chat = if kind == "group" {
            "group:10"
        } else {
            "private:20"
        };
        assert!(engine.state(chat).unwrap().pending);
        assert!(h.rows("SELECT * FROM expressions").is_empty());
    }
    let c = Config::from_value(&merge(
        &defaults(),
        &json!({"apiKey":"","onebotToken":"","dataDir":"unused"}),
    ))
    .unwrap();
    assert!(!c.agent.owner_teaching.enabled);
    assert_eq!(c.agent.owner_teaching.owner_uin, "1950202917");
}

// P6d 不变量：每一道初筛命中时（包含观察模型）调用数严格为零。
#[tokio::test]
async fn decision_screen_zero_calls() {
    for (label, agent) in [
        ("quiet", json!({"quietHours":{"start":0,"end":23}})),
        ("quota", json!({"maxMessagesPerHour":1})),
        ("cooldown", json!({"minThinkIntervalSeconds":3600})),
        ("no_new_message", json!({})),
    ] {
        let mut agent = agent;
        agent["threeLayerDecision"] = json!(true);
        let (e, h) = setup(&base(label, agent, vec![]));
        if label == "cooldown" {
            *h.now.lock().unwrap() = 1000.;
        }
        if label == "quota" {
            h.store
                .lock()
                .unwrap()
                .delivery("group:10", false, h.now())
                .unwrap();
        }
        e.ingest(&h.event(&json!({}))).unwrap();
        if label == "no_new_message" {
            let mut states = e.chats();
            states[0].1.pending = false;
            let (other, other_h) = setup(&base(label, json!({"threeLayerDecision":true}), vec![]));
            other.inherit_chats(states);
            other.tick().unwrap();
            other.wait_idle().await;
            assert!(!other_h
                .trace
                .lock()
                .unwrap()
                .iter()
                .any(|v| v[0] == "model"));
        } else {
            e.tick().unwrap();
            e.wait_idle().await;
            assert!(
                !h.trace.lock().unwrap().iter().any(|v| v[0] == "model"),
                "{label}"
            );
        }
    }
}

fn topic_setup(extra: Value) -> (Arc<Engine>, Arc<Harness>) {
    let (e, h) = setup(&base(
        "topic",
        merge(
            &json!({"threeLayerDecision":true,"pauseSeconds":300,"activeWindowSeconds":10000}),
            &extra,
        ),
        vec![],
    ));
    *h.now.lock().unwrap() = 5. * 86400. + 43200.;
    {
        let db = h.store.lock().unwrap();
        for i in 0..24 {
            db.message(&json!({"chat":"group:10","id":format!("old{i}"),"sender":"20","name":"Human","text":"好的","ts":h.now() - (1 + i / 6) as f64 * 86400.,"self":false})).unwrap();
        }
        db.message(&json!({"chat":"group:10","id":"last","sender":"20","name":"Human","text":"好的","ts":h.now()-600.,"self":false})).unwrap();
        db.mark_handled("group:10", "last", true).unwrap();
        db.add_thought(
            "group:10",
            &json!({"text":"聊聊花园","kind":"system2","subject":"someone_else"}),
            h.now(),
        )
        .unwrap();
    }
    e.restore().unwrap();
    (e, h)
}

#[tokio::test]
async fn independent_topic_gates_and_probability() {
    for reason in [
        "empty_thoughts",
        "outside_group_schedule",
        "group_active",
        "expectation",
        "proactive_quota",
        "proactive_cooldown",
        "unanswered_message",
    ] {
        let extra = match reason {
            "proactive_quota" => json!({"maxProactivePerHour":0}),
            "proactive_cooldown" => json!({"proactiveCooldownSeconds":900}),
            _ => json!({}),
        };
        let (e, h) = topic_setup(extra);
        {
            let db = h.store.lock().unwrap();
            match reason {
                "empty_thoughts" => {
                    db.execute("DELETE FROM thoughts", []).unwrap();
                }
                "outside_group_schedule" => {
                    db.execute("DELETE FROM messages WHERE id != 'last'", [])
                        .unwrap();
                }
                "group_active" => {
                    db.execute("UPDATE messages SET ts=? WHERE id='last'", [h.now() - 1.])
                        .unwrap();
                }
                "expectation" => {
                    db.expect("group:10", h.now(), 600., &json!({})).unwrap();
                }
                "proactive_cooldown" => {
                    db.delivery("group:10", true, h.now() - 1.).unwrap();
                }
                "unanswered_message" => {
                    db.execute("DELETE FROM handled", []).unwrap();
                }
                _ => {}
            }
            let state = e.chats()[0].1.clone();
            let result = qq_inner_core::engine::decision::screen(
                &db,
                "group:10",
                &state,
                &e.config.agent,
                h.now(),
            )
            .unwrap();
            assert_eq!(result.topic, Some(reason));
        }
        e.tick().unwrap();
        e.wait_idle().await;
        assert!(
            !h.trace.lock().unwrap().iter().any(|v| v[0] == "model"),
            "{reason}"
        );
    }
    let (e, h) = topic_setup(json!({}));
    {
        let db = h.store.lock().unwrap();
        let mut state = e.chats()[0].1.clone();
        let result = qq_inner_core::engine::decision::screen(
            &db,
            "group:10",
            &state,
            &e.config.agent,
            h.now(),
        )
        .unwrap();
        assert_eq!(result.reply, Some("no_new_message"));
        assert_eq!(result.topic, None);
        assert!((0.01..0.3).contains(&result.probability));
        state.pending = true; // ②的资格不能由 pending 取反得到。
        assert_eq!(
            qq_inner_core::engine::decision::screen(
                &db,
                "group:10",
                &state,
                &e.config.agent,
                h.now()
            )
            .unwrap()
            .topic,
            None
        );
        let mut g =
            qq_inner_core::media::media_select::group_activity(&db, "group:10", h.now(), 300.).unwrap();
        let p = qq_inner_core::engine::decision::topic_probability(&g, 1000., 300.);
        g.since_human *= 2.;
        assert!(qq_inner_core::engine::decision::topic_probability(&g, 2000., 300.) >= p);
        // 用均匀抽样网格检查触发比例，避免随机测试抖动；启发式只断言区间。
        let accepted = (0..1000).filter(|i| (*i as f64 / 1000.) < p).count();
        assert!((10..300).contains(&accepted));
    }
    e.tick().unwrap();
    e.wait_idle().await;
    assert!(h
        .trace
        .lock()
        .unwrap()
        .iter()
        .any(|v| v[0] == "model" && v[3] == "topic"));
    assert_eq!(e.last_error(), None);
}

#[tokio::test]
async fn topic_probability_rejection_and_reply_priority() {
    let (e, h) = topic_setup(json!({}));
    h.model.lock().unwrap()["decisionDraw"] = json!(0.99);
    e.tick().unwrap();
    e.wait_idle().await;
    assert!(!h.trace.lock().unwrap().iter().any(|v| v[0] == "model"));
    assert!(e.chats()[0].1.due > h.now());
    // 新消息优先走①，②的空池/作息条件不阻止被点名的正常回复。
    e.ingest(&h.event(&json!({"id":"new"}))).unwrap();
    h.model.lock().unwrap()["decisionDraw"] = json!(0.);
    e.tick().unwrap();
    e.wait_idle().await;
    let trace = h.trace.lock().unwrap();
    assert!(trace.iter().any(|v| v[0] == "model" && v[3] == "message"));
    assert!(!trace.iter().any(|v| v[0] == "model" && v[3] == "topic"));
}

#[tokio::test]
async fn grown_persona_reaches_form_and_articulate_without_replacing_seed() {
    let (engine, h) = setup(&base(
        "grown-persona",
        json!({"persona":"基座种子，保持诚实。","identity":{"enabled":true,"growPersona":true,"minTraits":1,"minAgeDays":0}}),
        vec![],
    ));
    h.store.lock().unwrap().execute("INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources) VALUES('grown','group:10','group','traits','style','好奇探索，重视证据','[]')",[]).unwrap();
    *h.model.lock().unwrap() = json!({"expectPersona":"成长人格"});
    engine
        .ingest(&h.event(&json!({"text":"[CQ:at,qq=99]如何判断盆土干湿？"})))
        .unwrap();
    engine.tick().unwrap();
    engine.wait_idle().await;
    assert!(!h.payloads.lock().unwrap().is_empty());
    assert!(h.payloads.lock().unwrap()[0]["persona"]
        .as_str()
        .unwrap()
        .contains("责任边界"));
}

#[tokio::test]
async fn backstory_is_opt_in_persisted_and_recalled_before_articulation() {
    for enabled in [false, true] {
        let (e, h) = setup(&base(
            "backstory",
            json!({"backstory":{"enabled":enabled}}),
            vec![],
        ));
        face_turn(
            &e,
            &h,
            "b1",
            "[CQ:at,qq=99]基于经历评价耐心",
            10,
            5.,
            json!({"text":"虚构情景中的耐心值得参考。"}),
        )
        .await;
        let payloads = h.payloads.lock().unwrap().clone();
        assert_eq!(payloads.len(), 1);
        if enabled {
            let rows =
                qq_inner_core::persona::backstory::recall(&h.store.lock().unwrap(), "group:10", h.now(), 8)
                    .unwrap();
            assert_eq!(
                rows.len(),
                1,
                "messages={:?} notes={:?} layers={:?}",
                h.rows("SELECT * FROM messages"),
                h.rows("SELECT * FROM notes"),
                h.rows("SELECT * FROM memory_layers")
            );
            assert_eq!(payloads[0]["backstories"], json!(rows));
        } else {
            assert!(payloads[0].get("backstories").is_none());
            assert!(h
                .rows("SELECT name FROM sqlite_master WHERE name LIKE 'persona_backstory%'")
                .is_empty());
        }
        drop(payloads);
        if enabled {
            *h.now.lock().unwrap() += 10.;
            face_turn(
                &e,
                &h,
                "b2",
                "[CQ:at,qq=99]再聊聊",
                10,
                5.,
                json!({"text":"接着聊。"}),
            )
            .await;
            assert_eq!(
                h.payloads.lock().unwrap()[1]["backstories"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
    }
}

#[tokio::test]
async fn backstory_engine_rejects_unsafe_requests_and_stored_evidence() {
    for (request, evidence) in [
        ("[CQ:at,qq=99]基于经历评价耐心；编造张三的经历", false),
        ("[CQ:at,qq=99]基于经历评价耐心", true),
    ] {
        let (e, h) = setup(&base(
            "backstory-rejected",
            json!({"backstory":{"enabled":true}}),
            vec![],
        ));
        if evidence {
            h.store
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO notes(chat,text) VALUES('group:10','真实记忆')",
                    [],
                )
                .unwrap();
        }
        face_turn(
            &e,
            &h,
            "b1",
            request,
            10,
            5.,
            json!({"text":"不编造经历。"}),
        )
        .await;
        assert_eq!(h.payloads.lock().unwrap()[0]["backstories"], json!([]));
        assert!(h.rows("SELECT * FROM persona_backstory").is_empty());
    }
}

#[tokio::test]
async fn quiet_group_collects_on_tick_and_rejoin_without_speaking() {
    let (e, h) = setup(&base(
        "eager_orientation",
        json!({"allowedGroups":["10"],"observation":{"enabled":true}}),
        vec![],
    ));
    assert!(e.chats().is_empty());
    e.tick().unwrap();
    e.wait_idle().await;
    let row = e.orientation.get("group:10").unwrap().unwrap();
    assert_eq!(row.collected, 1);
    assert_eq!(row.status, "observing");
    assert_eq!(row.sources["availability"]["info"], "unavailable");
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(h.orientation_reads.lock().unwrap().len(), 3);
    e.ingest(&json!({"post_type":"notice","notice_type":"group_increase","self_id":99,"user_id":99,"group_id":10,"time":h.now()})).unwrap();
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(h.orientation_reads.lock().unwrap().len(), 6);
    assert_eq!(e.orientation.get("group:10").unwrap().unwrap().collected, 1);
    assert!(h.payloads.lock().unwrap().is_empty());
    e.stop().await;
}

#[tokio::test]
async fn orientation_tick_and_speak_share_collection_and_reject_stale_epoch() {
    let (e, h) = setup(&base(
        "concurrent_orientation",
        json!({"allowedGroups":["10"],"observation":{"enabled":true,"minSeconds":600,"minMessages":100}}),
        vec![],
    ));
    *h.hold.lock().unwrap() = Some("COLLECT".into());
    e.tick().unwrap();
    entered(&h).await;
    for _ in 0..3 {
        e.tick().unwrap();
    }
    let speak = e.orientation.before_speak("group:10");
    tokio::pin!(speak);
    assert!(futures_util::poll!(&mut speak).is_pending());
    assert_eq!(h.orientation_reads.lock().unwrap().len(), 3);
    // 采集期间仍能访问 Store；重新入群使旧 epoch 的结果失效。
    e.orientation.joined("group:10", h.now()).unwrap();
    h.release.add_permits(1);
    e.wait_idle().await;
    assert!(!speak.await.unwrap());
    assert_eq!(e.orientation.get("group:10").unwrap().unwrap().collected, 0);
    *h.hold.lock().unwrap() = None;
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(e.orientation.get("group:10").unwrap().unwrap().collected, 1);
    assert_eq!(h.orientation_reads.lock().unwrap().len(), 6);
    e.stop().await;
}

#[tokio::test]
async fn disabled_orientation_never_collects_existing_rows() {
    let (e, h) = setup(&base("disabled_orientation", json!({}), vec![]));
    let row = e.orientation.ensure("group:10").unwrap();
    e.tick().unwrap();
    e.wait_idle().await;
    assert!(!e.orientation.collect("group:10", row.epoch).await.unwrap());
    assert!(e.orientation.before_speak("group:10").await.unwrap());
    assert!(h.orientation_reads.lock().unwrap().is_empty());
    assert_eq!(e.orientation.get("group:10").unwrap().unwrap().collected, 0);
    e.stop().await;
}

#[tokio::test]
async fn orientation_collects_outside_schedule_only_when_online_and_enabled() {
    for (enabled, connected, online) in [
        (true, true, true),
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        let (e, h) = setup(&base(
            "orientation_outside_schedule",
            json!({
                "allowedGroups":["10"],
                "observation":{"enabled":enabled},
                "schedule":{
                    "enabled":true,
                    "activeStart":"14:00",
                    "inactiveStart":"01:00",
                    "timezone":"UTC"
                }
            }),
            vec![],
        ));
        // 固定时钟为 UTC 12:00，位于 14:00–01:00 作息表外。
        assert!(!e.available(h.now()).unwrap());
        e.orientation.ensure("group:10").unwrap();
        {
            let mut transport = h.transport.lock().unwrap();
            transport.connected = connected;
            transport.online = online;
        }
        e.tick().unwrap();
        e.wait_idle().await;
        let expected = enabled && connected && online;
        assert_eq!(
            h.orientation_reads.lock().unwrap().len(),
            if expected { 3 } else { 0 }
        );
        assert_eq!(
            e.orientation.get("group:10").unwrap().unwrap().collected,
            i64::from(expected)
        );
        assert!(h.payloads.lock().unwrap().is_empty());
        assert_eq!(*h.sends.lock().unwrap(), 0);
        e.stop().await;
    }
}

#[tokio::test]
async fn off_duty_ingest_records_batch_and_resumes_backlog_after_active_window() {
    for three_layer in [false, true] {
        let (e, h) = setup(&base(
            "off_duty_backlog",
            json!({
                "threeLayerDecision":three_layer,
                "schedule":{"enabled":true,"activeStart":"14:00","inactiveStart":"01:00","timezone":"UTC"}
            }),
            vec![],
        ));
        assert!(!e.available(h.now()).unwrap());
        for i in 0..26 {
            let event = h.event(&json!({
                "id":format!("backlog{i}"),
                "text":"继续聊园艺"
            }));
            e.ingest(&event).unwrap();
            e.ingest(&event).unwrap(); // 重复投递不增加 version 或入库数量。
        }
        let state = e.chats()[0].1.clone();
        assert_eq!(state.version, 26);
        assert!(state.pending);
        assert!(!state.pause_done);
        assert_eq!(state.hint, qq_inner_core::engine::policy::Hint::Open);
        assert_eq!(state.last_id, "backlog25");
        assert_eq!(state.last_human, h.now());
        assert_eq!(state.due, h.now());
        assert_eq!(h.rows("SELECT * FROM messages WHERE self=0").len(), 26);
        e.tick().unwrap();
        e.wait_idle().await;
        assert!(e.chats()[0].1.pending);
        assert!(h.rows("SELECT * FROM calls").is_empty());
        assert!(h.rows("SELECT * FROM deliveries").is_empty());
        assert_eq!(*h.sends.lock().unwrap(), 0);

        // 离岗两小时已超过活跃窗口，回岗后仍需处理积压且保留待处理消息。
        *h.now.lock().unwrap() = 14. * 3600.;
        assert!(e.available(h.now()).unwrap());
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(*h.sends.lock().unwrap(), 1, "{:?}", h.trace.lock().unwrap());
        assert!(!e.chats()[0].1.pending);
        assert!(h.trace.lock().unwrap().iter().any(|row| {
            row[0] == "model"
                && row[1] == "FORM"
                && row[2] == "open"
                && row[4].as_array().unwrap().contains(&json!("backlog25"))
        }));
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(*h.sends.lock().unwrap(), 1);
        e.stop().await;
    }
}

#[tokio::test]
async fn off_duty_ingest_advances_observation_and_memory() {
    let (e, h) = setup(&base(
        "off_duty_perception",
        json!({
            "learning":{"enabled":true},
            "observation":{"enabled":true},
            "schedule":{"enabled":true,"activeStart":"14:00","inactiveStart":"01:00","timezone":"UTC"}
        }),
        vec![],
    ));
    assert!(!e.available(h.now()).unwrap());
    h.store
        .lock()
        .unwrap()
        .expect("group:10", h.now(), 60., &json!({}))
        .unwrap();
    e.ingest(&h.event(&json!({"text":"继续聊园艺"}))).unwrap();
    assert_eq!(
        e.orientation
            .get("group:10")
            .unwrap()
            .unwrap()
            .message_count,
        1
    );
    assert!(!h
        .rows("SELECT * FROM memory_layers WHERE layer='short_term'")
        .is_empty());
    assert_eq!(
        h.store
            .lock()
            .unwrap()
            .expectation("group:10", h.now())
            .unwrap()
            .unwrap()["observation"]["event"],
        "human_message"
    );
    assert!(e.chats()[0].1.pending);
    e.tick().unwrap();
    e.wait_idle().await;
    assert!(h.rows("SELECT * FROM calls").is_empty());
    assert_eq!(*h.sends.lock().unwrap(), 0);
    e.stop().await;
}

#[tokio::test]
async fn replies_recheck_duty_before_first_task_poll_and_resume() {
    for teaching in [false, true] {
        let (e, h) = setup(&base(
            "duty_changed_before_poll",
            json!({
                "ownerTeaching":{"enabled":teaching,"ownerUin":"20"},
                "schedule":{"enabled":true,"activeStart":"14:00","inactiveStart":"01:00","timezone":"UTC"}
            }),
            vec![],
        ));
        *h.now.lock().unwrap() = 23. * 3600.;
        let event = if teaching {
            json!({"post_type":"message","message_type":"private","self_id":99,"user_id":20,"message_id":"command","time":h.now(),"message":"/记住 喜欢Rust"})
        } else {
            h.event(&json!({"text":"继续聊园艺"}))
        };
        e.ingest(&event).unwrap();
        e.tick().unwrap();
        // tick 已准入，但异步任务尚未 poll 时进入休息时间。
        *h.now.lock().unwrap() = 26. * 3600.;
        assert!(!e.available(h.now()).unwrap());
        e.wait_idle().await;
        assert!(h.rows("SELECT * FROM calls").is_empty());
        assert!(h.rows("SELECT * FROM deliveries").is_empty());
        assert_eq!(*h.sends.lock().unwrap(), 0);
        *h.now.lock().unwrap() = 38. * 3600.;
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(*h.sends.lock().unwrap(), 1);
        e.stop().await;
    }
}

#[tokio::test]
async fn backlog_digest_reaches_cycle_stages_and_can_withhold() {
    for (enabled, empty) in [(false, false), (false, true), (true, false), (true, true)] {
        let (e, h) = setup(&base(
            "digest_resume",
            json!({
                "schedule":{"enabled":true,"activeStart":"14:00","inactiveStart":"01:00","timezone":"UTC"},
                "observation":{"backlogDigest":{"enabled":enabled,"threshold":80,"headCount":2,"tailCount":3,"sampleMax":20,"samplePercent":10}}
            }),
            vec![],
        ));
        *h.model.lock().unwrap() = json!({"empty":empty});
        for i in 0..300 {
            e.ingest(&h.event(&json!({"id":format!("digest{i}"),"text":"继续聊园艺"})))
                .unwrap();
        }
        e.tick().unwrap();
        e.wait_idle().await;
        assert!(h.model_inputs.lock().unwrap().is_empty());
        *h.now.lock().unwrap() = 14. * 3600.;
        e.tick().unwrap();
        e.wait_idle().await;
        assert!(e.last_error().is_none(), "{:?}", e.last_error());
        assert_eq!(*h.sends.lock().unwrap(), usize::from(!empty));
        assert!(!e.chats()[0].1.pending);
        let inputs = h.model_inputs.lock().unwrap().clone();
        assert_eq!(inputs.len(), if empty { 1 } else { 4 });
        for (stage, payload) in inputs {
            if enabled {
                let digest = &payload["backlogDigest"];
                assert_eq!(digest["totalMessages"], 300, "{stage}");
                assert_eq!(digest["sampleIds"].as_array().unwrap().len(), 20);
                assert_eq!(digest["omittedMessages"], 275);
            } else {
                assert!(payload.get("backlogDigest").is_none());
            }
            let history = payload["history"].as_array().unwrap();
            assert_eq!(history.len(), if enabled { 25 } else { 300 }, "{stage}");
            if !enabled {
                for (i, message) in history.iter().enumerate() {
                    assert_eq!(message["id"], format!("digest{i}"));
                }
            }
            assert_eq!(history.first().unwrap()["id"], "digest0");
            assert_eq!(history.last().unwrap()["id"], "digest299");
            assert!(history.iter().all(|m| m["text"].is_string()
                && m["timestamp"].is_number() && m["self"] == false));
        }
        e.stop().await;
    }
}

#[tokio::test]
async fn backlog_digest_disabled_reads_all_and_enabled_at_threshold_keeps_recent_history() {
    let mut payloads = Vec::new();
    for (enabled, threshold) in [(false, 1), (true, 30)] {
        let (e, h) = setup(&base(
            "digest_compatibility",
            json!({"observation":{"backlogDigest":{"enabled":enabled,"threshold":threshold}}}),
            vec![],
        ));
        *h.model.lock().unwrap() = json!({"empty":true});
        for i in 0..30 {
            e.ingest(&h.event(&json!({"id":format!("digest{i}")}))).unwrap();
        }
        e.tick().unwrap();
        e.wait_idle().await;
        let inputs = h.model_inputs.lock().unwrap().clone();
        assert_eq!(inputs.len(), 1);
        let payload = inputs[0].1.clone();
        assert!(payload.get("backlogDigest").is_none());
        assert_eq!(payload["history"].as_array().unwrap().len(), if enabled { 24 } else { 30 });
        payloads.push(payload);
        e.stop().await;
    }
    payloads[0]["history"] = payloads[1]["history"].clone();
    assert_eq!(payloads[0], payloads[1], "除 history 容量外，payload 协议不变");
}

#[tokio::test]
async fn backfill_perceives_once_without_scheduling_and_preserves_live_priority() {
    let (e, h) = setup(&base(
        "backfill",
        json!({"learning":{"enabled":true},"observation":{"enabled":true}}),
        vec![],
    ));
    let old = h.event(&json!({"id":"old","text":"[CQ:at,qq=99]历史问题"}));
    e.ingest_backfill(&old).unwrap();
    e.ingest_backfill(&old).unwrap();
    e.ingest(&old).unwrap(); // A later live duplicate must not reply either.
    assert_eq!(h.rows("SELECT * FROM messages").len(), 1);
    assert_eq!(e.orientation.get("group:10").unwrap().unwrap().message_count, 1);
    assert!(!h.rows("SELECT * FROM memory_layers WHERE layer='short_term'").is_empty());
    assert!(e.chats().iter().all(|(_, s)| !s.pending && s.pause_done && s.version == 0));
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(*h.sends.lock().unwrap(), 0);
    assert!(h.rows("SELECT * FROM calls").is_empty());

    e.ingest(&h.event(&json!({"id":"live"}))).unwrap();
    let before = serde_json::to_value(e.chats()).unwrap();
    e.ingest_backfill(&h.event(&json!({"id":"old2","text":"普通历史消息"}))).unwrap();
    assert_eq!(serde_json::to_value(e.chats()).unwrap(), before);
    e.ingest(&h.event(&json!({"id":"live2","text":"普通实时消息"}))).unwrap();
    assert_eq!(e.chats()[0].1.hint, qq_inner_core::engine::policy::Hint::SelfChat);
    assert!(e.chats()[0].1.pending);
    assert_eq!(e.orientation.get("group:10").unwrap().unwrap().message_count, 4);
    e.stop().await;
}

#[tokio::test]
async fn backfill_fetches_allowed_groups_and_is_disabled_or_cancelled() {
    let (e, h) = setup(&base("backfill_fetch", json!({"backfill":{"count":7}}), vec![]));
    // Harness errors on unsupported calls: both groups must still be attempted.
    e.backfill_once().await;
    assert_eq!(*h.orientation_reads.lock().unwrap(), vec!["get_group_msg_history"; 2]);
    h.orientation_reads.lock().unwrap().clear();
    e.stop().await;
    e.backfill_once().await;
    assert!(h.orientation_reads.lock().unwrap().is_empty());

    let (e, h) = setup(&base("backfill_disabled", json!({"backfill":{"enabled":false}}), vec![]));
    e.backfill_once().await;
    assert!(h.orientation_reads.lock().unwrap().is_empty());
    e.stop().await;
}

#[tokio::test]
async fn stopping_cancels_in_flight_backfill() {
    let (e, h) = setup(&base("backfill_cancel", json!({}), vec![]));
    *h.hold.lock().unwrap() = Some("HISTORY".into());
    let engine = e.clone();
    let task = tokio::spawn(async move { engine.backfill_once().await });
    h.entered.notified().await;
    e.stop().await;
    tokio::time::timeout(std::time::Duration::from_secs(1), task).await.unwrap().unwrap();
    assert_eq!(*h.orientation_reads.lock().unwrap(), vec!["get_group_msg_history"]);
    assert!(h.rows("SELECT * FROM messages").is_empty());
}

#[tokio::test]
async fn backfill_ingests_expired_history_without_replying() {
    let (e, h) = setup(&base(
        "expired_backfill",
        json!({"learning":{"enabled":true},"observation":{"enabled":true}}),
        vec![],
    ));
    let mut old = h.event(&json!({"id":"expired"}));
    let ts = h.now() - e.config.agent.active_window_seconds - 3600.;
    old["time"] = json!(ts);
    e.ingest(&old).unwrap();
    assert!(h.rows("SELECT * FROM messages").is_empty());
    e.ingest_backfill(&old).unwrap();
    e.ingest_backfill(&old).unwrap();
    let messages = h.rows("SELECT * FROM messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["ts"], ts);
    assert_eq!(e.orientation.get("group:10").unwrap().unwrap().message_count, 1);
    assert!(!h.rows("SELECT * FROM memory_layers WHERE layer='short_term'").is_empty());
    assert!(e.chats().is_empty());
    e.tick().unwrap();
    e.wait_idle().await;
    assert_eq!(*h.sends.lock().unwrap(), 0);
    assert!(h.rows("SELECT * FROM calls").is_empty());
    e.stop().await;
}


#[tokio::test]
async fn visible_self_identity_is_cached_scoped_and_injected() {
    for (member, expected) in [
        (
            json!({"card":"群里的卡琳","nickname":"Kar1n1911"}),
            "群里的卡琳",
        ),
        (json!({"card":"","nickname":"Kar1n1911"}), "Kar1n1911"),
        (Value::Null, "Kar1n1911"),
    ] {
        let (e, h) = setup(&base(
            "self_identity",
            json!({"name":"Lantaneen","aliases":["Lantaneen","Luma"]}),
            vec![],
        ));
        *h.model.lock().unwrap() = json!({"allocation":"self","loginInfo":{"nickname":"Kar1n1911"},"memberInfo":{"10":member,"11":{"card":"别群名片","nickname":"Kar1n1911"}}});
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(h.identity_reads.lock().unwrap().len(), 3);
        for (i, name) in ["Kar1n1911", expected, "Luma"].iter().enumerate() {
            e.ingest(&h.event(&json!({"id":format!("alias{i}"),"text":format!("{name}：你好")})))
                .unwrap();
            assert_eq!(
                e.state("group:10").unwrap().hint,
                qq_inner_core::engine::policy::Hint::SelfChat
            );
            e.tick().unwrap();
            e.wait_idle().await;
            *h.now.lock().unwrap() += 2.;
        }
        let inputs = h.model_inputs.lock().unwrap().clone();
        let payload = &inputs
            .iter()
            .rev()
            .find(|(stage, _)| stage == "FORM")
            .unwrap()
            .1;
        let own = payload["history"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["self"] == true)
            .unwrap();
        assert_eq!(own["speaker"], expected);
        assert_eq!(payload["selfIdentity"]["qq"], "99");
        assert_eq!(payload["selfIdentity"]["nickname"], "Kar1n1911");
        assert_eq!(
            payload["selfIdentity"]["groupCard"],
            member["card"].as_str().unwrap_or("")
        );
        assert_eq!(
            payload["selfIdentity"]["instructions"],
            "群友讨论的那个机器人就是你,不要以第三方身份谈论自己"
        );
        assert_eq!(
            h.identity_reads.lock().unwrap().len(),
            3,
            "turns reuse cached identity"
        );
        e.ingest(&h.event(&json!({"id":"other-card","text":"别群名片：你好"})))
            .unwrap();
        assert_ne!(
            e.state("group:10").unwrap().hint,
            qq_inner_core::engine::policy::Hint::SelfChat
        );
        *h.now.lock().unwrap() += 301.;
        *h.model.lock().unwrap() = json!({"allocation":"self"});
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(
            h.identity_reads.lock().unwrap().len(),
            6,
            "failed refresh is also cached"
        );
        e.ingest(&h.event(&json!({"id":"fallback","text":"Luma：你好"})))
            .unwrap();
        e.tick().unwrap();
        e.wait_idle().await;
        let inputs = h.model_inputs.lock().unwrap().clone();
        let payload = &inputs
            .iter()
            .rev()
            .find(|(stage, _)| stage == "FORM")
            .unwrap()
            .1;
        assert_eq!(payload["selfIdentity"]["visibleName"], "Lantaneen");
        assert!(payload["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["self"] == true)
            .all(|m| m["speaker"] == "Lantaneen"));
        e.stop().await;
    }
}

#[tokio::test]
async fn self_chat_wakes_off_duty_but_other_hints_preserve_backlog() {
    use qq_inner_core::engine::policy::Hint;
    for three_layer in [false, true] {
        for resting_rhythm in [false, true] {
            for kind in ["mention", "name", "private", "open", "other"] {
                let (e, h) = setup(&base(
                    "wake",
                    json!({
                        "threeLayerDecision":three_layer,
                        "name":{"text":"小机器人"},
                        "aliases":["小机器人"],
                        "schedule":{"enabled":!resting_rhythm,"activeStart":"14:00","inactiveStart":"01:00","timezone":"UTC"},
                        "rhythm":{"enabled":resting_rhythm,"dayProbability":0,"restMinSeconds":600,"restMaxSeconds":600}
                    }),
                    vec![],
                ));
                assert!(!e.available(h.now()).unwrap());
                let event = match kind {
                    "private" => {
                        json!({"post_type":"message","message_type":"private","self_id":99,"user_id":20,"message_id":"wake","time":h.now(),"message":"活着就说话"})
                    }
                    "name" => h.event(&json!({"text":"小机器人：活着就说话"})),
                    "open" => h.event(&json!({"text":"继续聊园艺"})),
                    "other" => h.event(&json!({"text":"[CQ:at,qq=21]继续聊园艺"})),
                    _ => h.event(&json!({})),
                };
                e.ingest(&event).unwrap();
                let addressed = matches!(kind, "mention" | "name" | "private");
                assert_eq!(e.chats()[0].1.hint == Hint::SelfChat, addressed, "{kind}");
                let version = e.chats()[0].1.version;
                if addressed {
                    *h.hold.lock().unwrap() = Some("FORM".into());
                }
                e.tick().unwrap();
                if addressed {
                    entered(&h).await;
                    e.tick().unwrap(); // 休息 tick 不应取消已叫醒的在途回复。
                    assert_eq!(e.chats()[0].1.version, version);
                    h.release.add_permits(1);
                }
                e.wait_idle().await;
                assert!(e.last_error().is_none(), "{:?}", e.last_error());
                assert_eq!(
                    *h.sends.lock().unwrap(),
                    usize::from(addressed),
                    "{kind}, layers={three_layer}, rhythm={resting_rhythm}: {:?}",
                    h.trace.lock().unwrap()
                );
                assert_eq!(e.chats()[0].1.pending, !addressed);
                if !addressed {
                    assert_eq!(e.chats()[0].1.version, version + 1);
                    assert!(h.model_inputs.lock().unwrap().is_empty());
                }
                assert!(!e.available(h.now()).unwrap());
                e.stop().await;
            }
        }
    }
}

#[tokio::test]
async fn relay_links_enter_formation_only_after_review_and_local_dedup() {
    const URL: &str = "https://example.org/esp32";
    for mode in ["keep", "drop", "duplicate", "disabled", "one_author", "audit_error", "concurrent_duplicate", "high_risk_enabled"] {
        let (e, h) = topic_setup(json!({"relay":{"enabled":mode != "disabled", "allowHighRisk":mode == "high_risk_enabled"}}));
        *h.model.lock().unwrap() = json!({"relayConcurrentDuplicate":mode == "concurrent_duplicate","relayError":mode == "audit_error","relayAudit":{"keep":if mode == "drop" {json!([])} else {json!([0])}},"empty":true});
        {
            let db = h.store.lock().unwrap();
            // No reservoir needed: relay independently makes the topic path eligible.
            db.execute("DELETE FROM thoughts", []).unwrap();
            for (id, author, body) in [("r1", "30", "look"), ("r2", if mode == "one_author" {"30"} else {"31"}, "read")] {
                db.message(&json!({"chat":"group:11","id":id,"sender":author,"name":"PRIVATE_NAME","text":format!("{body} {URL}"),"ts":h.now()-60.,"self":false})).unwrap();
            }
            for i in 0..2 {
                db.message(&json!({"chat":"group:10","id":format!("interest{i}"),"sender":"20","text":"esp32","ts":h.now()-700.-i as f64,"self":false})).unwrap();
            }
            if mode == "duplicate" {
                db.execute("INSERT INTO memory_layers(id,chat,subject,layer,slot,text,sources,expires) VALUES('relay-duplicate','group:10','group','short_term','relay',?,'[]',?)", rusqlite::params![URL,h.now()+100.]).unwrap();
            }
        }
        e.tick().unwrap();
        e.wait_idle().await;
        assert_eq!(e.last_error(), None, "{mode}");
        let inputs = h.model_inputs.lock().unwrap();
        let audit = inputs.iter().find(|(stage, _)| stage == "RELAY");
        assert_eq!(audit.is_some(), matches!(mode, "keep" | "drop" | "audit_error" | "concurrent_duplicate" | "high_risk_enabled"), "{mode}");
        if let Some((_, payload)) = audit { assert_eq!(payload, &json!({"links":[URL]})); }
        if mode == "disabled" { assert!(inputs.is_empty()); }
        let formed = inputs.iter().find(|(stage, _)| stage == "FORM");
        let topics = formed.map(|(_, payload)| &payload["externalTopics"]);
        assert_eq!(topics.is_some_and(|v| v.is_array()), matches!(mode, "keep" | "high_risk_enabled"), "{mode}");
        if matches!(mode, "keep" | "high_risk_enabled") {
            let topics = topics.unwrap();
            assert_eq!(topics[0]["item"]["url"], URL);
            assert_eq!(topics[0]["source"], URL);
            let serialized = topics.to_string();
            for secret in ["PRIVATE_NAME", "group:11", "r1", "r2", "look", "read"] {
                assert!(!serialized.contains(secret), "{serialized}");
            }
        }
        // FORM returns no ideas: relay must never bypass the existing pipeline to send.
        assert_eq!(*h.sends.lock().unwrap(), 0, "{mode}");
    }
}
