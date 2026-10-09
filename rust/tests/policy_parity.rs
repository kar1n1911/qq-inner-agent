//! `policy.rs` / `text.rs` 与 JS 的一致性测试。
//!
//! 时区是这里最值得较真的地方：JS 用 ICU 的 `Intl.DateTimeFormat`，Rust 用 `chrono-tz`，
//! 两者数据库版本不同。因此测试不只比对 `quiet()` 的布尔结果，还直接逐点比对
//! "本地分钟数"，避免布尔值把偏差掩盖掉。期望值来自已捕获的 JSON 固化金标准，缺失即失败。
use qq_inner_core::config::{load_with_env, Config};
use qq_inner_core::engine::policy::{
    active_at, allowed, local_minutes_of_day, pick_length_target, quiet, repeated, Allocation,
    Candidate, CandidateKind,
};
use qq_inner_core::memory::text::{similarity, terms};
use serde_json::{json, Value};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

/// cargo test 会并发跑测试；临时目录名必须唯一，否则两个测试会互相覆盖 payload。
/// （codex 的 config_parity 用 pid + 随机数解决了同样的问题。）
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 走真实的加载路径（对应 JS 的 `loadConfig`）：只写一份 config.json 覆盖项，
/// 其余字段由 defaults 补全，credentials 与 dataDir 由加载流程生成。
fn config_with(extra: Value) -> Config {
    let dir = std::env::temp_dir().join(format!(
        "qq-inner-config-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, AtomicOrdering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("create config dir");
    fs::write(
        dir.join("config.json"),
        serde_json::to_string(&extra).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("secrets.json"), "{}").unwrap();
    let loaded = load_with_env(&dir, |_| None).expect("load config");
    let _ = fs::remove_dir_all(&dir);
    loaded.config
}

// 输入也与快照逐值匹配，防止参数矩阵改变后误用旧期望值。
fn expected(payload: &Value) -> Value {
    // 四个不同 payload 全部捕获，包括 active_at；序列化键由 serde_json 的有序对象生成。
    let cases: std::collections::BTreeMap<String, Value> =
        serde_json::from_str(include_str!("golden/policy.json")).unwrap();
    assert_eq!(cases.len(), 4, "必须保留四组不同的 policy 金标准");
    let key = serde_json::to_string(payload).unwrap();
    cases
        .get(&key)
        .unwrap_or_else(|| panic!("缺少 policy 固化金标准: {key}"))
        .clone()
}

/// 覆盖南北半球、半小时偏移、超长 DST 跳变与整点偏移的时区。
const ZONES: [&str; 7] = [
    "Europe/Stockholm",
    "UTC",
    "America/New_York",
    "Asia/Shanghai",
    "Australia/Lord_Howe",
    "Pacific/Chatham",
    "America/Santiago",
];

/// 跨越 2026 年 EU/US DST 切换点与年初的若干时刻。
const INSTANTS: [i64; 7] = [
    1_767_225_600, // 2026-01-01T00:00:00Z
    1_774_747_800, // 2026-03-29T01:30:00Z（EU 进入夏令时前后）
    1_792_891_800, // 2026-10-25T01:30:00Z（EU 退出夏令时前后）
    1_793_491_200, // 2026-11-01T00:00:00Z（US 退出夏令时）
    1_781_524_800, // 2026-06-15T12:00:00Z
    0,             // epoch
    1_798_761_600, // 2026-12-31T00:00:00Z
];

#[test]
fn local_minutes_match_icu_point_by_point() {
    let mut local = Vec::new();
    for zone in ZONES {
        for ts in INSTANTS {
            local.push(json!({ "ts": ts, "zone": zone }));
        }
    }
    let payload =
        json!({ "local": local, "quiet": [], "active": [], "terms": [], "similarity": [] });
    let js = expected(&payload);

    let mut index = 0;
    for zone in ZONES {
        for ts in INSTANTS {
            let expected = js["local"][index].as_i64().expect("js minutes");
            let got = local_minutes_of_day(ts as f64, Some(zone))
                .unwrap_or_else(|| panic!("rust could not resolve {zone}"));
            assert_eq!(got, expected, "{zone} at {ts}");
            index += 1;
        }
    }
}

#[test]
fn quiet_matches_javascript_across_zones_and_windows() {
    // 每个时区 × 每个时刻 × 若干窗口（含同起止、跨夜、整天）。
    let windows: [(f64, f64); 6] = [
        (23.0, 8.0),
        (8.0, 23.0),
        (0.0, 0.0),
        (12.0, 13.0),
        (0.0, 23.0),
        (22.0, 6.0),
    ];
    let mut cases = Vec::new();
    for zone in ZONES {
        for ts in INSTANTS {
            for (start, end) in windows {
                cases.push(json!({
                    "ts": ts,
                    "hours": { "start": start, "end": end, "timezone": zone },
                }));
            }
        }
    }
    let payload =
        json!({ "local": [], "quiet": cases, "active": [], "terms": [], "similarity": [] });
    let js = expected(&payload);

    for (index, case) in cases.iter().enumerate() {
        let hours = qq_inner_core::config::QuietHours {
            start: case["hours"]["start"].as_f64().unwrap(),
            end: case["hours"]["end"].as_f64().unwrap(),
            timezone: Some(case["hours"]["timezone"].as_str().unwrap().to_string()),
        };
        let ts = case["ts"].as_f64().unwrap();
        let expected = js["quiet"][index].as_bool().expect("js quiet");
        assert_eq!(quiet(ts, Some(&hours)), expected, "case {index}: {case}");
    }
}

#[test]
fn fixed_utc_offsets_resolve_before_falling_back_to_a_region() {
    // 2026-06-15T12:00:00Z —— 固定偏移不受夏令时影响，结果可直接手算。
    let ts = 1_781_524_800.0;
    assert_eq!(local_minutes_of_day(ts, Some("UTC")), Some(12 * 60));
    assert_eq!(local_minutes_of_day(ts, Some("GMT")), Some(12 * 60));
    assert_eq!(local_minutes_of_day(ts, Some("UTC+8")), Some(20 * 60));
    assert_eq!(local_minutes_of_day(ts, Some("UTC-3")), Some(9 * 60));
    assert_eq!(
        local_minutes_of_day(ts, Some("UTC+05:30")),
        Some(17 * 60 + 30)
    );
    assert_eq!(local_minutes_of_day(ts, Some("GMT+2")), Some(14 * 60));
    assert_eq!(local_minutes_of_day(ts, Some("+08:00")), Some(20 * 60));
    // 大小写与首尾空白不敏感。
    assert_eq!(local_minutes_of_day(ts, Some(" utc+8 ")), Some(20 * 60));
    // 越界的偏移不能被当成固定偏移，也不该落到某个区域名上。
    assert!(local_minutes_of_day(ts, Some("UTC+99")).is_none());
    assert!(local_minutes_of_day(ts, Some("UTC+8:99")).is_none());
    // 两步走的第二步：确实需要夏令时时才用区域名。
    assert!(local_minutes_of_day(ts, Some("Europe/Stockholm")).is_some());
}

#[test]
fn quiet_returns_false_without_configuration() {
    assert!(!quiet(1_767_225_600.0, None));
    let same = qq_inner_core::config::QuietHours {
        start: 8.0,
        end: 8.0,
        timezone: Some("UTC".into()),
    };
    assert!(
        !quiet(1_767_225_600.0, Some(&same)),
        "equal start/end means no quiet hours"
    );
}

#[test]
fn active_at_matches_javascript_across_zones() {
    let schedules = [
        json!({ "enabled": true, "activeStart": "08:00", "inactiveStart": "23:00", "timezone": "Europe/Stockholm" }),
        json!({ "enabled": true, "activeStart": "22:00", "inactiveStart": "06:00", "timezone": "Asia/Shanghai" }),
        json!({ "enabled": false, "activeStart": "08:00", "inactiveStart": "23:00", "timezone": "UTC" }),
        json!({ "enabled": true, "activeStart": "00:00", "inactiveStart": "23:59", "timezone": "America/New_York" }),
        json!({ "enabled": true, "activeStart": "08:30", "inactiveStart": "22:15", "timezone": "Australia/Lord_Howe" }),
    ];
    let mut cases = Vec::new();
    for schedule in &schedules {
        for ts in INSTANTS {
            cases.push(json!({ "ts": ts, "schedule": schedule }));
        }
    }
    let payload =
        json!({ "local": [], "quiet": [], "active": cases, "terms": [], "similarity": [] });
    let js = expected(&payload);

    for (index, case) in cases.iter().enumerate() {
        let schedule: qq_inner_core::config::Schedule =
            serde_json::from_value(case["schedule"].clone()).expect("schedule");
        let ts = case["ts"].as_f64().unwrap();
        let expected = js["active"][index].as_bool().expect("js active");
        assert_eq!(active_at(ts, &schedule), expected, "case {index}: {case}");
    }
}

#[test]
fn terms_and_similarity_match_javascript_on_a_tricky_corpus() {
    let corpus = [
        "你好世界",
        "hello world",
        "abc中文混排def",
        "emoji🙂与汉字",
        "ＡＢＣ全角",
        "ＡＢ",
        "ａｂ",
        "単語とかな",
        "한국어 텍스트",
        "x1y22z333",
        "a",
        "汉字単字",
        // 注意：这里刻意不放 BMP 以外的汉字（如扩展 B 区）。JS 的二元组用
        // `slice(i, i+2)` 按 UTF-16 码元切分，遇到代理对会切出半个字符（孤立代理项），
        // JSON 无法承载，serde_json 也会直接拒绝解析。这是 JS 侧的缺陷，Rust 按 `char`
        // 切分得到的是合法字符，两者在非 BMP 汉字上本就不同（见 text.rs 的说明）。
        "  trimmed  ",
        "混合Mixed文字123",
        "🀄麻将牌",
    ];
    let pairs: Vec<Vec<String>> = vec![
        vec!["你好世界".into(), "你好世界".into()],
        vec!["你好世界".into(), "你好".into()],
        vec!["abc中文混排def".into(), "中文混排".into()],
        vec!["hello world".into(), "world hello".into()],
        vec!["a".into(), "a".into()],
        vec!["汉字単字".into(), "汉字".into()],
    ];
    let payload = json!({
        "local": [], "quiet": [], "active": [],
        "terms": corpus,
        "similarity": pairs,
    });
    let js = expected(&payload);

    for (index, text) in corpus.iter().enumerate() {
        let mut got: Vec<String> = terms(text).into_iter().collect();
        got.sort();
        let expected: Vec<String> = js["terms"][index]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, expected, "terms({text:?}) diverged");
    }
    for (index, pair) in pairs.iter().enumerate() {
        let expected = js["similarity"][index].as_f64().unwrap();
        let got = similarity(&pair[0], &pair[1]);
        assert_eq!(
            got.to_bits(),
            expected.to_bits(),
            "similarity({:?}, {:?}) diverged",
            pair[0],
            pair[1]
        );
    }
}

fn candidate(
    id: &str,
    kind: CandidateKind,
    motivation: f64,
    relevance: f64,
    originality: f64,
) -> Candidate {
    Candidate {
        id: id.into(),
        kind,
        text: format!("candidate {id}"),
        motivation,
        relevance,
        originality,
        for_tags: vec![],
        against_tags: vec![],
    }
}

#[test]
fn selection_honours_probability_saturation_and_allocation() {
    let agent = config_with(
        json!({ "agent": { "proactive": true, "threshold": 4.09, "interruptThreshold": 4.8 } }),
    )
    .agent;
    let rated = vec![
        candidate("a", CandidateKind::System2, 5.0, 5.0, 5.0),
        candidate("b", CandidateKind::System2, 4.5, 3.0, 3.0),
        candidate("c", CandidateKind::System2, 4.0, 2.0, 5.0), // relevance 太低
    ];

    // 被点名时一定选最高分，不看阈值。
    let picked =
        qq_inner_core::engine::policy::select(&rated, Allocation::SelfChat, &agent, 0.0, || 0.0)
            .unwrap();
    assert_eq!(picked.candidate.id, "a");

    // 开放轮次用 threshold。
    let picked =
        qq_inner_core::engine::policy::select(&rated, Allocation::Open, &agent, 0.0, || 1.0)
            .unwrap();
    assert_eq!(picked.candidate.id, "a");

    // 别人被点名时用更高的 interruptThreshold：a 仍达标。
    let picked =
        qq_inner_core::engine::policy::select(&rated, Allocation::Other, &agent, 0.0, || 1.0)
            .unwrap();
    assert_eq!(picked.candidate.id, "a");

    // 都不达标且不触发 system1 时应返回 None。
    let weak = vec![candidate("w", CandidateKind::System2, 2.0, 3.0, 3.0)];
    assert!(
        qq_inner_core::engine::policy::select(&weak, Allocation::Open, &agent, 0.0, || 1.0)
            .is_none()
    );

    // 但 system1 概率命中时可以退化为 system1 候选。默认概率是 0，因此这里显式调高。
    let system1_agent =
        config_with(json!({ "agent": { "proactive": true, "system1Probability": 1 } })).agent;
    let system1 = vec![candidate("s", CandidateKind::System1, 1.0, 3.0, 3.0)];
    let picked = qq_inner_core::engine::policy::select(
        &system1,
        Allocation::Open,
        &system1_agent,
        0.0,
        || 0.0,
    )
    .unwrap();
    assert_eq!(picked.candidate.id, "s");

    // 非主动模式下，除了被点名都返回 None。
    let passive = config_with(json!({ "agent": { "proactive": false } })).agent;
    assert!(
        qq_inner_core::engine::policy::select(&rated, Allocation::Open, &passive, 0.0, || 0.0)
            .is_none()
    );
    assert!(qq_inner_core::engine::policy::select(
        &rated,
        Allocation::SelfChat,
        &passive,
        0.0,
        || 0.0
    )
    .is_some());
}

#[test]
fn turns_silent_raises_the_score_but_is_capped() {
    let agent = config_with(json!({ "agent": { "proactive": true } })).agent;
    let rated = vec![candidate("a", CandidateKind::System2, 4.0, 5.0, 5.0)];
    let none =
        qq_inner_core::engine::policy::select(&rated, Allocation::SelfChat, &agent, 0.0, || 0.0)
            .unwrap();
    let many =
        qq_inner_core::engine::policy::select(&rated, Allocation::SelfChat, &agent, 500.0, || 0.0)
            .unwrap();
    assert!(many.adjusted > none.adjusted);
    // 上限是 motivation × 1.2，再被 5.0 截断。
    assert!(many.adjusted <= 5.0);
    assert!((many.adjusted - 4.8).abs() < 1e-12);
}

#[test]
fn length_buckets_never_give_the_throwaway_bucket_to_an_addressed_turn() {
    for i in 0..=1000 {
        let draw = i as f64 / 1000.0;
        assert_ne!(pick_length_target("self", None, || draw), "tiny");
        assert!(matches!(
            pick_length_target("open", None, || draw),
            "tiny" | "short" | "medium" | "long"
        ));
    }
    assert_eq!(pick_length_target("open", None, || 0.149), "tiny");
    assert_eq!(pick_length_target("open", None, || 0.15), "short");
    assert_eq!(pick_length_target("open", None, || 0.55), "medium");
    assert_eq!(pick_length_target("open", None, || 0.93), "long");
    assert_eq!(pick_length_target("self", None, || 0.0), "short");
    assert_eq!(pick_length_target("self", None, || 0.50), "medium");
    assert_eq!(pick_length_target("self", None, || 0.92), "long");
}

#[test]
fn exact_repetition_is_distinct_from_similarity_penalty() {
    let history = vec![
        "今天天气不错".to_string(),
        "先试试这个简单方法。".to_string(),
    ];
    assert!(
        repeated("今天天气不错", &history),
        "identical text counts as a repeat"
    );
    assert!(repeated("  今天天气不错  ", &history), "comparison trims");
    assert!(!repeated("先试试这个简单方法", &history));
    assert_eq!(
        qq_inner_core::engine::policy::repetition_penalty("先试试这个简单方法", &history),
        1.0
    );
    assert!(!repeated("完全不同的一句话内容", &history));
}

#[test]
fn allowed_requires_an_explicit_match() {
    let agent =
        config_with(json!({ "agent": { "allowedGroups": ["10"], "allowedUsers": ["20"] } })).agent;
    assert!(allowed("group:10", &agent));
    assert!(allowed("private:20", &agent));
    assert!(!allowed("group:11", &agent));
    assert!(
        !allowed("private:10", &agent),
        "a group id must not match the private list"
    );
    assert!(!allowed("channel:10", &agent));
    assert!(!allowed("group:", &agent));
}

#[test]
fn selection_probability_is_continuous_in_every_degree() {
    use qq_inner_core::engine::policy::select;
    let agent = config_with(json!({"agent": {"proactive": true, "threshold": 3.01,
        "interruptThreshold": 4.8, "system1Probability": 0}}))
    .agent;
    let rate = |candidate: &Candidate, allocation, agent: &qq_inner_core::config::Agent| {
        let count = (0..10000)
            .filter(|i| {
                select(
                    std::slice::from_ref(candidate),
                    allocation,
                    agent,
                    0.0,
                    || (*i as f64 + 0.5) / 10000.0,
                )
                .is_some()
            })
            .count();
        count as f64 / 10000.0
    };
    for axis in 0..3 {
        let with_score = |score| {
            let mut c = candidate("one", CandidateKind::System2, 5.0, 5.0, 5.0);
            match axis {
                0 => c.motivation = score,
                1 => c.relevance = score,
                _ => c.originality = score,
            }
            c
        };
        let boundary = if axis == 0 { 3.01 } else { 3.0 };
        let below = rate(&with_score(boundary - 0.0001), Allocation::Open, &agent);
        let above = rate(&with_score(boundary + 0.0001), Allocation::Open, &agent);
        assert!((above - below).abs() < 0.001);
        let mut previous = 0.0;
        for score in [1.0, 1.0001, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0] {
            let current = rate(&with_score(score), Allocation::Open, &agent);
            assert!(current >= previous);
            previous = current;
        }
        assert_eq!(rate(&with_score(1.0001), Allocation::Open, &agent), 0.0);
    }
    let c = candidate("one", CandidateKind::System2, 2.5, 3.0, 3.0);
    assert!(rate(&c, Allocation::Other, &agent) < rate(&c, Allocation::Open, &agent));
    let fallback = qq_inner_core::config::Agent {
        system1_probability: 1.0,
        ..agent.clone()
    };
    let weak = candidate("weak", CandidateKind::System1, 1.0, 1.0001, 5.0);
    assert_eq!(rate(&weak, Allocation::Open, &fallback), 0.0);
    assert!(select(&[weak], Allocation::Other, &fallback, 0.0, || 0.0).is_none());
    let duplicated = vec![c.clone(); 100];
    for draw in [0.1, 0.9] {
        assert_eq!(
            select(&duplicated, Allocation::Open, &agent, 0.0, || draw).is_some(),
            select(
                std::slice::from_ref(&c),
                Allocation::Open,
                &agent,
                0.0,
                || draw
            )
            .is_some()
        );
    }
    let low = candidate("low", CandidateKind::System2, 5.0, 1.0, 5.0);
    let good = candidate("good", CandidateKind::System2, 4.0, 5.0, 5.0);
    assert_eq!(
        select(&[low, good.clone()], Allocation::Open, &agent, 0.0, || 0.0)
            .unwrap()
            .candidate
            .id,
        "good"
    );
    let mut tied = good.clone();
    tied.id = "second".into();
    assert_eq!(
        select(&[good, tied], Allocation::Open, &agent, 0.0, || 0.0)
            .unwrap()
            .candidate
            .id,
        "good"
    );
}

#[test]
fn repetition_penalty_controls_suppression_without_count_amplification() {
    use qq_inner_core::engine::policy::{repetition_penalty, suppress_repetition};
    let history = vec!["alpha beta gamma delta".into()];
    let text = "alpha beta gamma epsilon";
    let penalty = repetition_penalty(text, &history);
    assert!(penalty > 0.0 && penalty < 1.0);
    assert!(suppress_repetition(text, &history, || penalty - 1e-6));
    assert!(!suppress_repetition(text, &history, || penalty + 1e-6));
    assert_eq!(
        penalty,
        repetition_penalty(text, &vec![history[0].clone(); 100])
    );
    assert!(!suppress_repetition("unrelated words", &history, || 0.0));
    assert!(!suppress_repetition(text, &[], || 0.0));
    assert!(suppress_repetition(" x ", &["x".into()], || panic!(
        "exact repeats need no draw"
    )));
}
