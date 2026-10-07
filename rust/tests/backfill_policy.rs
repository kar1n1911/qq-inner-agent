use qq_inner_core::{
    config::{defaults, merge, Agent},
    engine::policy::{normalize, normalize_backfill},
};
use serde_json::{json, Value};

fn agent() -> Agent {
    serde_json::from_value(merge(
        &defaults()["agent"],
        &json!({"allowedGroups":["10"],"allowedUsers":["20"],"ignoredUsers":["21"]}),
    ))
    .unwrap()
}

fn event() -> Value {
    json!({"post_type":"message","message_type":"group","self_id":99,
        "user_id":20,"group_id":10,"message_id":123,"time":1000,
        "sender":{"nickname":"Human"},"message":"[CQ:at,qq=99]历史问题"})
}

#[test]
fn backfill_accepts_old_messages_and_preserves_normalized_bytes() {
    let a = agent();
    let e = event();
    let now = 1000. + a.active_window_seconds + 1.;
    assert!(normalize(&e, "99", &a, now).is_none());
    let recovered = normalize_backfill(&e, "99", &a, now).unwrap();
    let live = normalize(&e, "99", &a, 1000.).unwrap();
    assert_eq!(
        serde_json::to_vec(&recovered).unwrap(),
        serde_json::to_vec(&live).unwrap()
    );
    assert_eq!(recovered.ts, 1000.);

    for ts in [now - a.active_window_seconds, now, now + 60.] {
        let mut e = event();
        e["time"] = json!(ts);
        let live = normalize(&e, "99", &a, now).unwrap();
        let backfill = normalize_backfill(&e, "99", &a, now).unwrap();
        assert_eq!(
            serde_json::to_vec(&live).unwrap(),
            serde_json::to_vec(&backfill).unwrap()
        );
        assert_eq!(backfill.ts, ts.min(now));
    }
}

#[test]
fn backfill_retains_admission_and_timestamp_guards() {
    let a = agent();
    for patch in [
        json!({"post_type":"notice"}),
        json!({"message_type":"other"}),
        json!({"user_id":99}),
        json!({"user_id":21}),
        json!({"user_id":null}),
        json!({"self_id":98}),
        json!({"group_id":11}),
        json!({"message_type":"private","user_id":30}),
        json!({"message_id":null}),
        json!({"message_id":""}),
        json!({"message":""}),
        json!({"time":"NaN"}),
        json!({"time":"Infinity"}),
        json!({"time":"-Infinity"}),
        json!({"time":10061}),
    ] {
        let e = merge(&event(), &patch);
        assert!(
            normalize_backfill(&e, "99", &a, 10000.).is_none(),
            "{patch}"
        );
    }
    assert!(normalize_backfill(&event(), "", &a, 10000.).is_none());
    // Keep the existing zero-ID convention; it is not a missing ID.
    let e = merge(&event(), &json!({"message_id":0}));
    assert_eq!(normalize_backfill(&e, "99", &a, 10000.).unwrap().id, "0");
    for ts in [
        json!("NaN"),
        json!("Infinity"),
        json!("-Infinity"),
        json!(10061),
    ] {
        let e = merge(&event(), &json!({"time":ts}));
        assert!(normalize(&e, "99", &a, 10000.).is_none());
    }
}
