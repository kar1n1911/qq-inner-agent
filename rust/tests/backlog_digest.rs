use qq_inner_core::{
    config::{defaults, merge, validate, Config},
    engine::backlog::{build, Settings},
    store::Store,
};
use serde_json::{json, Value};

fn insert(db: &Store, chat: &str, id: &str, own: bool) {
    // 刻意共用时间戳；边界必须按入库顺序且隔离会话。
    db.message(&json!({"chat":chat,"id":id,"sender":"20","name":"Human","text":format!("正文 {id}"),"ts":100.,"self":own})).unwrap();
}

fn settings() -> Settings {
    Settings {
        enabled: true,
        threshold: 10,
        head_count: 2,
        tail_count: 3,
        sample_max: 4,
        sample_percent: 0.,
    }
}

#[test]
fn defaults_and_validation_are_public_and_opt_in() {
    let expected = json!({"enabled":false,"threshold":80,"headCount":4,"tailCount":8,"sampleMax":20,"samplePercent":10});
    assert_eq!(
        defaults()["agent"]["observation"]["backlogDigest"],
        expected
    );
    let mut runtime_expected = expected;
    runtime_expected["samplePercent"] = json!(10.);
    assert_eq!(
        serde_json::to_value(Settings::default()).unwrap(),
        runtime_expected
    );
    for invalid in [
        json!(null),
        json!(true),
        json!({"enabled":"true"}),
        json!({"threshold":0}),
        json!({"threshold":1_000_001}),
        json!({"headCount":0}),
        json!({"tailCount":101}),
        json!({"sampleMax":-1}),
        json!({"sampleMax":101}),
        json!({"samplePercent":-0.1}),
        json!({"samplePercent":100.1}),
        json!({"samplePercent":"NaN"}),
        json!({"samplePercent":null}),
        json!({"threshold":1.5}),
        json!({"surprise":true}),
    ] {
        let cfg = merge(
            &defaults(),
            &json!({"agent":{"observation":{"backlogDigest":invalid}}}),
        );
        assert!(validate(&cfg).is_err(), "{invalid}");
    }
    let cfg = merge(
        &defaults(),
        &json!({"apiKey":"","onebotToken":"","dataDir":"unused","agent":{"observation":{"backlogDigest":{"enabled":true,"sampleMax":0}}}}),
    );
    validate(&cfg).unwrap();
    let parsed = Config::from_value(&cfg).unwrap();
    assert!(parsed.agent.observation.backlog_digest.enabled);
    assert_eq!(parsed.agent.observation.backlog_digest.head_count, 4);
    assert_eq!(parsed.agent.observation.backlog_digest.sample_max, 0);
    for percent in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Settings {
            sample_percent: percent,
            ..settings()
        }
        .validate()
        .is_err());
    }
}

#[test]
fn threshold_last_self_boundary_sampling_and_chat_isolation() {
    let db = Store::in_memory().unwrap();
    let chat = "group:10";
    for i in 0..50 {
        insert(&db, chat, &format!("old{i}"), false);
    }
    insert(&db, chat, "own", true);
    let cfg = settings();
    for i in 0..10 {
        insert(&db, chat, &format!("new{i}"), false);
    }
    assert!(build(&db, chat, &cfg).unwrap().is_none(), "超过阈值才触发");
    for i in 10..40 {
        insert(&db, chat, &format!("new{i}"), false);
    }
    insert(&db, "group:11", "other-own", true);
    insert(&db, "group:11", "other-human", false);
    assert!(build(&db, chat, &Settings::default()).unwrap().is_none());
    let digest = build(&db, chat, &cfg).unwrap().unwrap();
    assert_eq!(digest.context["totalMessages"], 40);
    assert_eq!(digest.context["omittedMessages"], 31);
    assert_eq!(digest.context["headIds"], json!(["new0", "new1"]));
    assert_eq!(
        digest.context["tailIds"],
        json!(["new37", "new38", "new39"])
    );
    let ids: Vec<_> = digest
        .messages
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 9);
    let positions: Vec<usize> = ids
        .iter()
        .map(|id| id.strip_prefix("new").unwrap().parse().unwrap())
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(positions[2..6].iter().all(|i| (2..37).contains(i)));
    assert!(digest.context["notice"]
        .as_str()
        .unwrap()
        .contains("中间省略了 31 条，以下为随机抽样"));
    assert_eq!(
        digest.context,
        build(&db, chat, &cfg).unwrap().unwrap().context
    );
    assert!(build(&db, "group:11", &cfg).unwrap().is_none());
    insert(&db, chat, "next-own", true);
    assert!(
        build(&db, chat, &cfg).unwrap().is_none(),
        "自身发言覆盖已读区间"
    );
}

#[test]
fn overlapping_windows_and_empty_sample_never_duplicate_messages() {
    let db = Store::in_memory().unwrap();
    for i in 0..5 {
        insert(&db, "private:20", &i.to_string(), false);
    }
    let cfg = Settings {
        threshold: 1,
        head_count: 4,
        tail_count: 4,
        ..settings()
    };
    let digest = build(&db, "private:20", &cfg).unwrap().unwrap();
    assert_eq!(digest.messages.len(), 5);
    assert_eq!(digest.context["omittedMessages"], 0);
    assert_eq!(digest.context["sampleIds"], json!([]));
    let cfg = Settings {
        head_count: 1,
        tail_count: 1,
        sample_max: 0,
        ..cfg
    };
    let digest = build(&db, "private:20", &cfg).unwrap().unwrap();
    assert_eq!(digest.context["omittedMessages"], 3);
    assert_eq!(
        digest
            .messages
            .iter()
            .map(|m| m["id"].clone())
            .collect::<Vec<Value>>(),
        json!(["0", "4"]).as_array().unwrap().clone()
    );
}

#[test]
fn percentage_rounding_cap_and_zero_percent_fallback() {
    let db = Store::in_memory().unwrap();
    for i in 0..27 {
        insert(&db, "group:10", &i.to_string(), false);
    }
    // 去掉头尾各一条后中间为 25 条；四舍五入、上限和中间总数分别独立约束。
    for (percent, max, expected) in [
        (10., 20, 3),
        (1., 20, 0),
        (50., 20, 13),
        (100., 4, 4),
        (0., 7, 7),
        (0., 100, 25),
        (100., 100, 25),
        (10., 0, 0),
    ] {
        let cfg = Settings {
            head_count: 1,
            tail_count: 1,
            sample_max: max,
            sample_percent: percent,
            ..settings()
        };
        let digest = build(&db, "group:10", &cfg).unwrap().unwrap();
        assert_eq!(
            digest.context["sampleIds"].as_array().unwrap().len(),
            expected,
            "percent={percent}, max={max}"
        );
        assert_eq!(digest.messages.len(), 2 + expected);
        assert_eq!(digest.context["omittedMessages"], 25 - expected);
    }
}
