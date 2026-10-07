//! 设计不变量精确断言；估计值只验范围、单调性，不从实现生成期望。
use qq_inner_core::{
    persona::conversation::{Classification, Evidence, Relation, Stage},
    media::media_select::{self as ms, Attention, Config, Drift, Feedback, GroupActivity, Outcome},
    media::media_source::Override,
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
    assert!(quiet.0 >= active.0);
    let wild = Attention {
        drift_level: Some(Drift::Wild),
        ..c.clone()
    };
    assert_ne!(
        ms::attention(&activity(false, 300.), &wild, true)
            .unwrap()
            .0,
        Drift::Wild
    );
    assert_ne!(
        ms::attention(&activity(true, 900.), &wild, false)
            .unwrap()
            .0,
        Drift::Wild
    );
    assert_eq!(
        ms::attention(&activity(true, 900.), &wild, true).unwrap().0,
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
