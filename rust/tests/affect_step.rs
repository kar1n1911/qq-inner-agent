use qq_inner_core::persona::affect::bounded_step;
#[test]
fn asymmetric_confidence_bounded() {
    assert_eq!(bounded_step(-1., 1.), -0.15);
    assert_eq!(bounded_step(1., 1.), 0.1);
    assert_eq!(bounded_step(-1., 0.5), -0.1);
    for n in -100..=100 {
        assert!(bounded_step(n as f64 / 100., 0.7).abs() <= 0.15);
    }
    assert_eq!(bounded_step(1., 0.), 0.);
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
