use qq_inner_core::store::Store;
use serde_json::json;
fn store() -> Store {
    let s = Store::in_memory().unwrap();
    affect::enable(&s).unwrap();
    for (chat, id, sender, own) in [
        ("a", "1", "u", false),
        ("b", "1", "v", false),
        ("a", "2", "bot", true),
    ] {
        s.message(&json!({"chat":chat,"id":id,"sender":sender,"name":sender,"text":"约定明天","ts":100.,"self":own})).unwrap();
    }
    s
}

use qq_inner_core::{
    engine::policy::pick_length_target,
    persona::affect::{
        self, bounded_step, disposition, motivation, Dimension::*, Disposition::*, LengthBias,
    },
};

#[test]
fn neutral_small_signal_corners_and_clamping() {
    assert_eq!(motivation(0., 0.), 1.);
    assert!((motivation(-0.0026, 0.0008) - 1.).abs() < 0.001);
    for (v, r, d, gain) in [
        (-1., -1., Angry, 1.4),
        (-1., 1., Withdrawn, 0.5),
        (1., 1., Scrutinizing, 0.7),
        (1., -1., Supportive, 1.2),
    ] {
        assert_eq!(disposition(v, r), d);
        // Gaussian references overlap: finite sigma approximates corner gains.
        assert!((motivation(v, r) - gain).abs() < 0.005);
        assert_eq!(motivation(v * 10., r * 10.), motivation(v, r));
    }
    assert_eq!(disposition(0., 0.), Scrutinizing);
    assert_eq!(disposition(-0.0026, 0.0008), Withdrawn);
}

#[test]
fn motivation_is_continuous_in_both_axes_including_zero_and_old_dead_zone() {
    for fixed in -100..=100 {
        let fixed = fixed as f64 / 100.;
        for i in -2000..2000 {
            let x = i as f64 / 2000.;
            let next = (i + 1) as f64 / 2000.;
            for (a, b) in [
                (motivation(x, fixed), motivation(next, fixed)),
                (motivation(fixed, x), motivation(fixed, next)),
            ] {
                assert!((a - b).abs() < 0.002, "{fixed}, {x}: {a} -> {b}");
            }
        }
    }
    // Old sign classification jumped from Withdrawn=0.5 to Scrutinizing=0.7.
    assert!((motivation(-1e-8, 0.1) - motivation(1e-8, 0.1)).abs() < 1e-7);
    // Old dead-zone classification merely moved this same jump to -0.05.
    assert!((motivation(-0.05 - 1e-8, 0.1) - motivation(-0.05 + 1e-8, 0.1)).abs() < 1e-7);
}

#[test]
fn quadrants_and_circuit() {
    let s = store();
    for _ in 0..3 {
        assert!(affect::burst_allowed(&s, "a", "1").unwrap());
        affect::reserve_burst(&s, "a", "1").unwrap();
    }
    assert!(!affect::burst_allowed(&s, "a", "1").unwrap());
    assert!(affect::reserve_burst(&s, "a", "1").is_err());
    assert!(affect::burst_allowed(&s, "b", "1").unwrap());
    assert!(affect::burst_allowed(&s, "a", "3").unwrap());
}

fn index(length: &str) -> usize {
    ["tiny", "short", "medium", "long"]
        .iter()
        .position(|s| *s == length)
        .unwrap()
}

#[test]
fn length_tilt_is_monotone_and_addressed_never_tiny() {
    for hint in ["open", "self", "other"] {
        for sign in [-1., 1.] {
            let mut previous_mean = None;
            for step in 0..=20 {
                let magnitude = step as f64 / 20.;
                let bias = LengthBias::from_affect(sign * magnitude, sign * magnitude);
                let mut total = 0;
                for i in 0..2000 {
                    let draw = (i as f64 + 0.5) / 2000.;
                    let length = pick_length_target(hint, Some(&bias), || draw);
                    let neutral = pick_length_target(hint, None, || draw);
                    if step == 0 {
                        assert_eq!(length, neutral);
                    }
                    if hint == "self" {
                        assert_ne!(length, "tiny");
                    }
                    if sign > 0. {
                        assert!(index(length) >= index(neutral));
                    } else {
                        assert!(index(length) <= index(neutral));
                    }
                    total += index(length);
                }
                let mean = total as f64 / 2000.;
                if let Some(previous) = previous_mean {
                    assert!(sign * (mean - previous) >= 0., "{hint}, {sign}, {step}");
                }
                previous_mean = Some(mean);
            }
        }
        let tiny_signal = LengthBias::from_affect(-0.0026, 0.0008);
        let changed = (0..10000)
            .filter(|i| {
                let draw = (*i as f64 + 0.5) / 10000.;
                pick_length_target(hint, Some(&tiny_signal), || draw)
                    != pick_length_target(hint, None, || draw)
            })
            .count();
        assert!(changed <= 2);
    }
}

#[test]
fn asymmetric_confidence_bounded() {
    for (signal, confidence, expected) in [
        (-1., 1., -0.15),
        (1., 1., 0.1),
        (-1., 0.5, -0.1),
        (1., 0., 0.),
    ] {
        assert_eq!(
            bounded_step(signal, confidence),
            expected,
            "signal={signal}, confidence={confidence}"
        );
    }
    for n in -100..=100 {
        assert!(bounded_step(n as f64 / 100., 0.7).abs() <= 0.15);
    }
}

#[test]
fn group_scores_accumulate_once_and_behavior_reads_them_without_agreement() {
    use qq_inner_core::{
        persona::affect::{self, Dimension, Settings},
        store::Store,
    };
    use serde_json::json;
    for chat in ["group:10", "private:20"] {
        for agreement in [-1., 1.] {
            let db = Store::in_memory().unwrap();
            affect::enable(&db).unwrap();
            for n in 0..2 {
                let last = json!({"chat":chat,"id":format!("m{n}"),"sender":"20","ts":100.,"self":false,"text":"test"});
                db.message(&last).unwrap();
                let score = json!({"mood":-1.,"rationality":0.5,"affinity":0.4,"agreement":agreement,"confidence":1.});
                for _ in 0..2 {
                    affect::apply(&db, chat, &last, &score, 100.).unwrap();
                }
                let b =
                    affect::behavior(&db, &Settings { enabled: true }, chat, &last, 100.).unwrap();
                assert_eq!(
                    b.group_mood,
                    if chat.starts_with("group:") {
                        -0.15 * (n + 1) as f64
                    } else {
                        0.
                    }
                );
                assert!((b.affinity - 0.04 * (n + 1) as f64).abs() < 1e-12);
            }
            assert_eq!(
                affect::read(&db, chat, "group", Dimension::Rationality, 100.).unwrap(),
                if chat.starts_with("group:") { 0.1 } else { 0. }
            );
            assert!(db
                .rows(
                    "SELECT * FROM affect_state WHERE subject='group' AND dimension='affinity'",
                    []
                )
                .unwrap()
                .is_empty());
        }
    }
}

#[test]
#[should_panic(expected = "agreement must never aggregate")]
fn agreement_never_becomes_affinity() {
    let s = store();
    affect::update(&s, "a", "person:u", Affinity, Agreement, 1., 1., "1", 100.).unwrap();
}
#[test]
fn independent_ratings_do_not_change_relationship() {
    let s = store();
    affect::rate(&s, "a", "1", -1., 1., 1., 100.).unwrap();
    assert_eq!(
        affect::read(&s, "a", "person:u", Affinity, 100.).unwrap(),
        0.
    );
    assert!(!affect::content_allowed("好感度0.7"));
    assert!(affect::content_allowed("我不认同这件事"));
}

#[test]
fn decay_and_read_writeback() {
    for (value, baseline, elapsed, expected) in [
        (1., 0., 10., 0.5),
        (-1., 0.5, 10., -0.25),
        (1., 0., -1., 1.),
    ] {
        assert_eq!(
            affect::decay(value, baseline, elapsed, 10.),
            expected,
            "value={value}, baseline={baseline}, elapsed={elapsed}"
        );
    }
    let s = store();
    affect::update(&s, "a", "person:u", Affinity, Affinity, 1., 1., "1", 100.).unwrap();
    assert!(
        (affect::read(&s, "a", "person:u", Affinity, 100. + 7. * 86400.).unwrap() - 0.05).abs()
            < 1e-12
    );
    assert_eq!(
        affect::read(&s, "b", "person:u", Affinity, 100.).unwrap(),
        0.
    );
    affect::rate(&s, "a", "1", -0.8, 0.9, 1., 100.).unwrap();
    assert_eq!(
        s.rows("SELECT mood,agreement FROM message_ratings", [])
            .unwrap()[0]["agreement"],
        0.9
    );
    assert!(affect::rate(&s, "a", "2", 0., 0., 1., 100.).is_err());
    assert!(affect::update(&s, "b", "person:u", Affinity, Affinity, 1., 1., "1", 100.).is_err());
    affect::reset(&s, "a", "person:u").unwrap();
    assert_eq!(
        affect::read(&s, "a", "person:u", Affinity, 100.).unwrap(),
        0.
    );
    assert!(s
        .rows("SELECT * FROM message_ratings", [])
        .unwrap()
        .is_empty());
}
