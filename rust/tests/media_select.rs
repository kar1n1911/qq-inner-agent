//! 设计不变量精确断言；估计值只验范围、单调性，不从实现生成期望。
use qq_inner_core::{
    media::media_select::{self as ms, Attention, Config, Drift, Feedback, GroupActivity, Outcome},
    media::media_source::Override,
    persona::conversation::{Classification, Evidence, Relation, Stage},
    store::Store,
};
use rusqlite::params;
use serde_json::json;
use std::collections::HashSet;

fn db() -> Store {
    let db = Store::in_memory().unwrap();
    ms::enable(&db).unwrap();
    db
}
fn config() -> Config {
    Config {
        enabled: true,
        ..Default::default()
    }
}
fn classified(stage: Stage, relation: Relation, confident: bool) -> Classification {
    Classification {
        stage,
        relation,
        confident,
        confidence: if confident { 1. } else { 0. },
        evidence: Evidence::default(),
    }
}
fn message(db: &Store, chat: &str, id: &str, ts: f64, text: &str, own: bool) {
    db.message(&json!({"chat":chat,"id":id,"sender":if own {"99"} else {"20"},"name":"不得输出的昵称","text":text,"ts":ts,"self":own})).unwrap();
}
fn asset(db: &Store, chat: &str, hash: &str, tier: &str, context: &str) {
    db.execute("INSERT INTO media_assets(chat,hash,kind,file,occurrences,first_seen,last_seen,bytes,source_tier) VALUES(?,?,'face','14',1,0,0,0,?)",params![chat,hash,tier]).unwrap();
    message(db, chat, &format!("context-{hash}"), 100., context, false);
    db.execute(
        "INSERT INTO media_contexts VALUES(?,?,?,'before')",
        params![chat, hash, format!("context-{hash}")],
    )
    .unwrap();
}
fn train(db: &Store, chat: &str, source: &str, hash: &str, bucket: &str, outcome: Outcome) {
    for i in 0..8 {
        ms::learn(
            db,
            Feedback {
                chat,
                event: &format!("training-{hash}-{bucket}-{i}"),
                source,
                hash,
                bucket,
                classification: &classified(Stage::Closing, Relation::Continuation, true),
                outcome,
                awake: true,
            },
        )
        .unwrap();
    }
}
fn n(db: &Store, table: &str) -> i64 {
    db.connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
const NOW: f64 = 10. * 86400. + 12. * 3600. + 600.;
fn history(db: &Store, chat: &str) {
    for day in 1..10 {
        for index in 0..3 {
            let id = format!("day-{day}-{index}");
            message(
                db,
                chat,
                &id,
                day as f64 * 86400. + 12. * 3600. + index as f64,
                "猫猫",
                false,
            );
            db.execute(
                "INSERT INTO media_receipts VALUES(?,?,0)",
                params![chat, id],
            )
            .unwrap();
        }
    }
    message(db, chat, "previous", NOW - 620., "猫猫", false);
    message(db, chat, "closing", NOW - 600., "猫猫 好的", false);
}

#[test]
fn silence_truth_table_and_uncertainty_are_exact_invariants() {
    // 手算资格表：仅自然终止×接续/转向可把沉默当负面；收束与单发始终剔除。
    for stage in [
        Stage::Developing,
        Stage::Closing,
        Stage::NaturalEnd,
        Stage::Standalone,
    ] {
        for relation in [Relation::Continuation, Relation::Shift, Relation::Unrelated] {
            let expected = if stage == Stage::NaturalEnd && relation != Relation::Unrelated {
                Some(0.)
            } else {
                None
            };
            assert_eq!(
                ms::signal(&classified(stage, relation, true), Outcome::Silence, true),
                expected
            );
            assert_eq!(
                ms::signal(&classified(stage, relation, true), Outcome::Silence, false),
                None,
                "作息扣除"
            );
            for outcome in [
                Outcome::Positive,
                Outcome::Negative,
                Outcome::Neutral,
                Outcome::Silence,
            ] {
                assert_eq!(
                    ms::signal(&classified(stage, relation, false), outcome, true),
                    None,
                    "不确定就不学"
                );
            }
        }
    }
}
#[test]
fn excluded_silence_is_audited_without_any_fitness_write() {
    let db = db();
    for (i, stage) in [Stage::Closing, Stage::Standalone, Stage::Developing]
        .into_iter()
        .enumerate()
    {
        let c = classified(stage, Relation::Continuation, true);
        assert_eq!(
            ms::learn(
                &db,
                Feedback {
                    chat: "group:10",
                    event: &i.to_string(),
                    source: "group:10",
                    hash: "cat",
                    bucket: "quiet_rescue",
                    classification: &c,
                    outcome: Outcome::Silence,
                    awake: true
                }
            )
            .unwrap(),
            None
        );
    }
    let c = classified(Stage::NaturalEnd, Relation::Continuation, false);
    for (i, outcome) in [Outcome::Positive, Outcome::Negative, Outcome::Silence]
        .into_iter()
        .enumerate()
    {
        ms::learn(
            &db,
            Feedback {
                chat: "group:10",
                event: &format!("uncertain-{i}"),
                source: "group:10",
                hash: "cat",
                bucket: "quiet_rescue",
                classification: &c,
                outcome,
                awake: true,
            },
        )
        .unwrap();
    }
    assert_eq!(n(&db, "media_feedback"), 6);
    assert_eq!(n(&db, "media_fitness"), 0);
    assert_eq!(
        db.rows(
            "SELECT signal FROM media_feedback WHERE signal IS NOT NULL",
            []
        )
        .unwrap()
        .len(),
        0
    );
}
#[test]
fn explicit_negative_is_not_rewarded_and_learning_is_group_and_bucket_scoped() {
    let db = db();
    train(
        &db,
        "group:10",
        "group:10",
        "gentle",
        "quiet_rescue",
        Outcome::Positive,
    );
    train(
        &db,
        "group:10",
        "group:10",
        "barbed",
        "quiet_rescue",
        Outcome::Negative,
    );
    let gentle = ms::fitness(&db, "group:10", "group:10", "gentle", "quiet_rescue").unwrap();
    let barbed = ms::fitness(&db, "group:10", "group:10", "barbed", "quiet_rescue").unwrap();
    // 启发式：支持证据应提高、争论证据应降低；不用拟合精确小数。
    assert!(gentle > 0.65 && barbed < 0.35 && gentle > barbed);
    assert_eq!(
        db.rows(
            "SELECT * FROM media_fitness WHERE target='group:11' OR bucket='active'",
            []
        )
        .unwrap()
        .len(),
        0
    );
    let before = n(&db, "media_feedback");
    train(
        &db,
        "group:10",
        "group:10",
        "gentle",
        "quiet_rescue",
        Outcome::Positive,
    );
    assert_eq!(n(&db, "media_feedback"), before, "重放不再学习");
    assert_eq!(
        ms::signal(
            &classified(Stage::Closing, Relation::Continuation, true),
            Outcome::Neutral,
            true
        ),
        None,
        "有人回复不是好评"
    );
}
#[test]
fn zero_draw_cannot_bypass_temperature_even_with_wild_drift() {
    let db = db();
    history(&db, "group:10");
    asset(&db, "group:10", "barbed", "unknown", "猫猫 好的");
    let mut c = config();
    c.attention.drift_level = Some(Drift::Wild);
    db.execute("INSERT INTO media_wild VALUES('group:10',1)", [])
        .unwrap();
    train(
        &db,
        "group:10",
        "group:10",
        "barbed",
        "quiet_rescue",
        Outcome::Negative,
    );
    assert!(ms::select(&db, "group:10", NOW, &c, 0.).unwrap().is_none());
    assert!(ms::candidates(&db, "group:10", "猫猫 好的", NOW, &c, true)
        .unwrap()
        .is_empty());
    asset(&db, "group:10", "gentle", "unknown", "猫猫 好的");
    train(
        &db,
        "group:10",
        "group:10",
        "gentle",
        "quiet_rescue",
        Outcome::Positive,
    );
    let selected = ms::select(&db, "group:10", NOW, &c, 0.).unwrap().unwrap();
    assert_eq!(selected.candidate.hash, "gentle");
    // 三条红线的精确输出契约：连昵称、来源消息 id、索引正文的字段都没有。
    assert_eq!(
        selected
            .candidate
            .segment(std::path::Path::new("/unused"))
            .unwrap(),
        json!({"type":"face","data":{"id":"14"}})
    );
    assert!(ms::candidates(&db, "group:10", "火箭 发射", NOW, &c, false)
        .unwrap()
        .is_empty());
    message(&db, "group:10", "drift-check", NOW, "火箭 发射 收到", false);
    assert!(
        ms::select(&db, "group:10", NOW, &c, 0.).unwrap().is_none(),
        "检索不到就不发"
    );
}
#[test]
fn public_is_required_before_entering_cross_group_candidates() {
    let db = db();
    let mut c = config();
    c.sharing = true;
    for chat in ["group:10", "group:11"] {
        db.execute(
            "INSERT INTO expressions(chat,subject,kind,term) VALUES(?,'group','jargon','猫猫')",
            [chat],
        )
        .unwrap();
    }
    for (hash, tier) in [("p", "public"), ("v", "private"), ("u", "unknown")] {
        asset(&db, "group:10", hash, tier, "原始聊天不得外溢");
    }
    asset(&db, "private:20", "dm", "public", "猫猫");
    let hashes = |db: &Store| {
        ms::candidates(db, "group:11", "猫猫", NOW, &c, false)
            .unwrap()
            .into_iter()
            .map(|r| r.hash)
            .collect::<Vec<_>>()
    };
    assert_eq!(hashes(&db), vec!["p"]);
    db.set_media_source_override("group:10", "u", Some(Override::Public))
        .unwrap();
    assert_eq!(hashes(&db), vec!["p", "u"]);
    db.set_media_source_override("group:10", "p", Some(Override::Private))
        .unwrap();
    assert_eq!(hashes(&db), vec!["u"], "小时缓存不得缓存公开性");
    assert_eq!(
        ms::candidates(&db, "group:10", "原始聊天不得外溢", NOW, &c, false)
            .unwrap()
            .len(),
        3,
        "群内档位不限制使用"
    );
    let cache = db.rows("SELECT * FROM media_sharing", []).unwrap();
    assert_eq!(cache[0]["summary"], "猫猫");
    assert!(!serde_json::to_string(&cache).unwrap().contains("原始聊天"));
    db.execute("DELETE FROM expressions", []).unwrap();
    assert_eq!(hashes(&db), vec!["u"], "小时内快照稳定");
    assert!(
        ms::candidates(&db, "group:11", "猫猫", NOW + 3600., &c, false)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn sharing_is_symmetric_monotone_and_has_a_hard_floor() {
    let set = |s: &str| {
        s.split_whitespace()
            .map(str::to_owned)
            .collect::<HashSet<_>>()
    };
    let a = set("aa bb cc dd");
    let low = set("aa ee ff gg");
    let mid = set("aa bb ff gg");
    let high = set("aa bb cc gg");
    // 手算交集 1/4 低于 .35，3/4 达到 .75；边界是精确契约。
    assert_eq!(ms::share(&a, &low), 0.);
    assert_eq!(ms::share(&a, &high), 1.);
    assert_eq!(ms::share(&a, &mid), ms::share(&mid, &a));
    assert!(ms::share(&a, &mid) >= ms::share(&a, &low));
    assert!(ms::share(&a, &high) >= ms::share(&a, &mid));
    assert_eq!(ms::share(&a, &HashSet::new()), 0.);
}
#[test]
fn group_history_is_independent_of_own_activity_and_other_groups() {
    let db = db();
    history(&db, "group:10");
    let before = ms::group_activity(&db, "group:10", NOW, 300.).unwrap();
    for i in 0..100 {
        message(
            &db,
            "group:10",
            &format!("self-{i}"),
            NOW - i as f64,
            "自己很活跃",
            true,
        );
        message(
            &db,
            "group:11",
            &format!("other-{i}"),
            NOW - i as f64,
            "别群很活跃",
            false,
        );
    }
    db.execute(
        "INSERT OR REPLACE INTO activity_rhythm VALUES(1,'self-active',?, ?,1,1,0)",
        params![NOW, NOW + 3600.],
    )
    .unwrap();
    let after = ms::group_activity(&db, "group:10", NOW, 300.).unwrap();
    assert!(after.quiet);
    assert!(after.awake);
    assert_eq!(after.since_human, 600.);
    assert_eq!(after.rate, before.rate);
    assert_eq!(after.media_rate, before.media_rate);
    let night = ms::group_activity(&db, "group:10", NOW + 14. * 3600., 300.).unwrap();
    assert!(!night.awake, "人类历史只在白天，深夜不是冷场");
    assert_eq!(
        ms::probability(
            &night,
            &classified(Stage::Closing, Relation::Continuation, true),
            &config(),
            false
        ),
        0.
    );
}
fn activity(quiet: bool, since: f64) -> GroupActivity {
    GroupActivity {
        activity: ms::activity_level(if quiet { 0.01 } else { 2. }, since, 300.),
        rate: if quiet { 0.01 } else { 2. },
        since_human: since,
        quiet,
        awake: true,
        short_density: 0.5,
        media_rate: 0.2,
    }
}
#[test]
fn exclusion_precedes_lottery_and_quiet_probability_rises_to_cap() {
    let c = config();
    let stage = classified(Stage::Closing, Relation::Continuation, true);
    let p1 = ms::probability(&activity(true, 300.), &stage, &c, false);
    let p2 = ms::probability(&activity(true, 1200.), &stage, &c, false);
    let p3 = ms::probability(&activity(true, 90000.), &stage, &c, false);
    assert!(p1 > 0. && p2 >= p1 && p3 >= p2 && p3 <= c.quiet_cap);
    assert_eq!(
        p3,
        ms::probability(&activity(true, 900000.), &stage, &c, false)
    );
    assert_eq!(ms::probability(&activity(true, 300.), &stage, &c, true), 0.);
    assert_eq!(ms::probability(&activity(true, 2.), &stage, &c, false), 0.);
    for stage in [Stage::Standalone, Stage::Developing] {
        assert_eq!(
            ms::probability(
                &activity(true, 900.),
                &classified(stage, Relation::Continuation, true),
                &c,
                false
            ),
            0.
        );
    }
    assert!(
        ms::probability(
            &activity(true, 900.),
            &classified(Stage::Closing, Relation::Continuation, false),
            &c,
            false
        ) < p2
    );
}
#[test]
fn attention_bounds_and_group_override_are_explicit() {
    let c = Attention::default();
    let active = ms::attention(&activity(false, 300.), &c, false).unwrap();
    let quiet = ms::attention(&activity(true, 900.), &c, false).unwrap();
    assert!(quiet.drift_level >= active.drift_level);
    let wild = Attention {
        drift_level: Some(Drift::Wild),
        ..c.clone()
    };
    assert_ne!(
        ms::attention(&activity(false, 300.), &wild, true)
            .unwrap()
            .drift_level,
        Drift::Wild
    );
    assert_ne!(
        ms::attention(&activity(true, 900.), &wild, false)
            .unwrap()
            .drift_level,
        Drift::Wild
    );
    assert_eq!(
        ms::attention(&activity(true, 900.), &wild, true)
            .unwrap()
            .drift_level,
        Drift::Wild
    );
    assert_eq!(
        ms::attention(
            &activity(true, 900.),
            &Attention {
                enabled: false,
                ..c
            },
            true
        ),
        None
    );
    let config: Config = serde_json::from_value(
        json!({"groups":{"group:10":{"enabled":false},"group:11":{"drift_level":"wild"}}}),
    )
    .unwrap();
    assert!(!config.groups["group:10"].enabled);
    assert_eq!(config.groups["group:11"].drift_level, Some(Drift::Wild));
}
#[test]
fn real_classifier_closing_and_standalone_silence_do_not_write_negative_signals() {
    let db = db();
    history(&db, "group:10");
    asset(&db, "group:10", "cat", "unknown", "猫猫");
    db.execute(
        "INSERT INTO media_contexts VALUES('group:10','cat','closing','usage')",
        [],
    )
    .unwrap();
    let c = config();
    ms::observe(&db, "group:10", NOW, &c).unwrap();
    let r = db
        .rows("SELECT classification,signal FROM media_feedback", [])
        .unwrap();
    assert_eq!(r.len(), 1);
    let stage: Classification =
        serde_json::from_str(r[0]["classification"].as_str().unwrap()).unwrap();
    assert!(matches!(stage.stage, Stage::Closing | Stage::NaturalEnd));
    assert!(r[0]["signal"].is_null());
    assert_eq!(n(&db, "media_fitness"), 0);
    message(&db, "group:11", "before", 100., "苹果", false);
    message(&db, "group:11", "single", 200., "天气", false);
    message(&db, "group:11", "after", 400., "火箭", false);
    asset(&db, "group:11", "single", "unknown", "天气");
    db.execute(
        "INSERT INTO media_contexts VALUES('group:11','single','single','usage')",
        [],
    )
    .unwrap();
    ms::observe(&db, "group:11", 1000., &c).unwrap();
    assert_eq!(n(&db, "media_fitness"), 0);
    ms::observe(&db, "group:10", NOW + 60., &c).unwrap();
    assert_eq!(n(&db, "media_feedback"), 2);
}
#[test]
fn disabled_selector_does_not_touch_database_schema() {
    let db = Store::in_memory().unwrap();
    assert!(ms::select(&db, "group:10", NOW, &Config::default(), 0.)
        .unwrap()
        .is_none());
    ms::observe(&db, "group:10", NOW, &Config::default()).unwrap();
    assert!(
        ms::candidates(&db, "group:10", "猫猫", NOW, &Config::default(), false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.rows(
            "SELECT name FROM sqlite_master WHERE name LIKE 'media_%'",
            []
        )
        .unwrap()
        .len(),
        0
    );
    assert!(Config {
        minimum_match: 0.,
        ..config()
    }
    .validate()
    .is_err());
}

#[test]
fn continuous_attention_visits_every_level_without_skipping() {
    for accepted in [false, true] {
        let mut a = activity(false, 0.);
        a.activity = 1.;
        let mut previous = ms::attention(&a, &Attention::default(), accepted).unwrap();
        let mut drifts = HashSet::new();
        let mut anchors = HashSet::new();
        for i in 0..=10000 {
            a.activity = 1. - i as f64 / 10000.;
            let current = ms::attention(&a, &Attention::default(), accepted).unwrap();
            let d = current.drift_level as usize;
            let b = current.anchor_policy as usize;
            drifts.insert(d);
            anchors.insert(b);
            assert!(d >= previous.drift_level as usize && d <= previous.drift_level as usize + 1);
            assert!(
                b >= previous.anchor_policy as usize && b <= previous.anchor_policy as usize + 1
            );
            assert!(current.target <= previous.target + 1e-12);
            assert!((current.target - previous.target).abs() < 0.001);
            if current.drift_level == Drift::Wild {
                assert!(accepted && a.activity <= 1. / 3.);
            }
            previous = current;
        }
        assert_eq!(drifts.len(), if accepted { 4 } else { 3 });
        assert_eq!(anchors.len(), 3);
    }
    // 真实输入的旧切点（速率 .5、间隔 6*silence）不再造成选靶/概率跳变。
    let c = config();
    let stage = classified(Stage::Closing, Relation::Continuation, true);
    for (rate, since) in [(0.5, 600.), (2., 1800.)] {
        let mut values = Vec::new();
        for delta in [-1e-7, 1e-7] {
            let mut a = activity(false, since + delta);
            a.activity = ms::activity_level(rate + delta, since + delta, 300.);
            values.push((
                ms::attention(&a, &c.attention, true).unwrap().target,
                ms::probability(&a, &stage, &c, false),
            ));
        }
        assert!((values[0].0 - values[1].0).abs() < 1e-6);
        assert!((values[0].1 - values[1].1).abs() < 1e-6);
    }
    for i in 1..10000 {
        let rate = i as f64 / 1000.;
        assert!(
            ms::activity_level(rate, 300., 300.) >= ms::activity_level(rate - 0.001, 300., 300.)
        );
        assert!(ms::activity_level(rate, 301., 300.) < ms::activity_level(rate, 300., 300.));
    }
}

#[test]
fn reaction_density_is_continuous_and_changes_actual_selection() {
    let mut a = activity(true, 600.);
    let mut previous = 0.5;
    for i in 0..=10000 {
        a.short_density = i as f64 / 10000.;
        let current = ms::attention(&a, &Attention::default(), false).unwrap();
        assert!(current.reaction_weight >= previous);
        assert!(current.reaction_weight - previous < 0.001);
        previous = current.reaction_weight;
    }
    let db = db();
    history(&db, "group:10");
    for hash in ["face", "image"] {
        asset(&db, "group:10", hash, "unknown", "猫猫 好的");
        train(
            &db,
            "group:10",
            "group:10",
            hash,
            "quiet_rescue",
            Outcome::Positive,
        );
    }
    db.execute(
        "UPDATE media_assets SET kind='image' WHERE hash='image'",
        [],
    )
    .unwrap();
    let mut c = config();
    assert_eq!(
        ms::select(&db, "group:10", NOW, &c, 0.)
            .unwrap()
            .unwrap()
            .candidate
            .hash,
        "face"
    );
    db.execute(
        "UPDATE messages SET text=? WHERE id LIKE 'day-%'",
        ["这是一条用于学习群内人类短消息密度的长消息，长度明确超过二十四个字。"],
    )
    .unwrap();
    assert_eq!(
        ms::select(&db, "group:10", NOW, &c, 0.)
            .unwrap()
            .unwrap()
            .candidate
            .hash,
        "image"
    );
    c.groups.insert(
        "group:10".into(),
        Attention {
            reaction_style: Some(ms::Reaction::Reserved),
            ..Default::default()
        },
    );
    assert_eq!(
        ms::select(&db, "group:10", NOW, &c, 0.)
            .unwrap()
            .unwrap()
            .candidate
            .hash,
        "image"
    );
    c.groups.get_mut("group:10").unwrap().reaction_style = Some(ms::Reaction::Lively);
    assert_eq!(
        ms::select(&db, "group:10", NOW, &c, 0.)
            .unwrap()
            .unwrap()
            .candidate
            .hash,
        "face"
    );
}

#[test]
fn structural_confidence_is_monotone_continuous_and_drives_probability() {
    let c = config();
    let mut e = Evidence {
        weighted_turns: 3.,
        previous_gap_seconds: 10.,
        silent_seconds: 600.,
        ..Default::default()
    };
    let mut previous = 0.;
    let mut p_previous = 0.;
    for i in 0..=10000 {
        e.previous_similarity = i as f64 / 10000.;
        let confidence = e.confidence(&c.classification);
        assert!((0. ..=1.).contains(&confidence));
        assert!(confidence >= previous && confidence - previous < 0.001);
        let mut stage = classified(Stage::Closing, Relation::Continuation, i >= 2500);
        stage.confidence = confidence;
        let p = ms::probability(&activity(true, 600.), &stage, &c, false);
        assert!(p >= p_previous && p - p_previous < 0.001);
        previous = confidence;
        p_previous = p;
    }
    let baseline = e.confidence(&c.classification);
    e.weighted_turns += 1.;
    assert!(e.confidence(&c.classification) > baseline);
    e.previous_gap_seconds += 100.;
    assert!(e.confidence(&c.classification) < baseline);
    // 老 pending JSON 没有连续证据时，以零置信度保守读取。
    let old: Classification = serde_json::from_value(json!({
        "stage":"closing", "relation":"continuation", "confident":true,
        "evidence":{"previous_similarity":1., "next_similarity":0.,
        "alternating_turns":2, "silent_seconds":600., "closing_marker":true}
    }))
    .unwrap();
    assert_eq!(old.confidence, 0.);
}

#[test]
fn classifier_confidence_does_not_inherit_hard_link_or_turn_boundaries() {
    use qq_inner_core::persona::conversation;
    let mut c = conversation::Config {
        closing_markers: vec!["好的".into()],
        ..Default::default()
    };
    let mut messages: Vec<qq_inner_core::engine::policy::Message> = (0..4)
        .map(|i| {
            serde_json::from_value(json!({"chat":"group:10", "id":i.to_string(),
            "sender":(i % 2).to_string(), "name":"test", "text":"猫猫 好的",
            "ts":i as f64 * 10., "self":false, "hint":"open"}))
            .unwrap()
        })
        .collect();
    // 间隔跨过阶段判定的硬阈值，离散 turns 会跳变，连续证据不得跳变。
    let mut results = Vec::new();
    for gap in [c.gap_seconds - 1e-7, c.gap_seconds + 1e-7] {
        messages[3].ts = messages[2].ts + gap;
        results.push(conversation::classify(&messages, 3, 1000., &c));
    }
    assert_ne!(
        results[0].evidence.alternating_turns,
        results[1].evidence.alternating_turns
    );
    assert_ne!(results[0].confident, results[1].confident);
    assert!(results[0].confidence > results[1].confidence);
    assert!((results[0].confidence - results[1].confidence).abs() < 1e-6);
    messages[3].ts = 30.;
    messages[3].text = "猫猫 好的 收到".into();
    let similarity = conversation::classify(&messages, 3, 1000., &c)
        .evidence
        .previous_similarity;
    let mut results = Vec::new();
    for overlap in [similarity - 1e-7, similarity + 1e-7] {
        c.overlap = overlap;
        results.push(conversation::classify(&messages, 3, 1000., &c));
    }
    assert_ne!(results[0].confident, results[1].confident);
    assert!((results[0].confidence - results[1].confidence).abs() < 1e-6);
}

#[test]
fn schedule_survives_capacity_pruning_and_distinguishes_cold_start() {
    let db = db();
    for (chat, count, days) in [
        ("busy", 700, 1),
        ("sparse", 10, 3),
        ("new", 30, 1),
        ("established", 30, 2),
    ] {
        for i in 0..count {
            message(
                &db,
                chat,
                &i.to_string(),
                NOW - (i % days) as f64 * 86400. - (i / days) as f64,
                "hello",
                false,
            );
        }
    }
    let before = db
        .rows("SELECT * FROM group_hours ORDER BY chat,hour", [])
        .unwrap();
    db.prune(NOW, 30., 500).unwrap();
    assert_eq!(db.history("busy", Some(1000)).unwrap().len(), 500);
    assert_eq!(
        db.rows("SELECT * FROM group_hours ORDER BY chat,hour", [])
            .unwrap(),
        before
    );
    for (chat, awake) in [
        ("busy", true),
        ("sparse", false),
        ("new", false),
        ("established", true),
    ] {
        assert_eq!(
            ms::group_activity(&db, chat, NOW, 300.).unwrap().awake,
            awake,
            "{chat}"
        );
    }
    // Old retained counters do not keep a group's schedule awake forever.
    assert!(
        !ms::group_activity(&db, "busy", NOW + 31. * 86400., 300.)
            .unwrap()
            .awake
    );
}
