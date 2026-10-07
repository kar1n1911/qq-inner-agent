mod affect_support;
use qq_inner_core::persona::recall::{Budget, Request};
use serde_json::json;
#[test]
fn optional_request_budget_and_scoped_evidence() {
    let s = affect_support::store();
    assert!(!serde_json::from_value::<Request>(json!({})).unwrap().needed);
    let request: Request =
        serde_json::from_value(json!({"needed":true,"query":"约定","window":100000})).unwrap();
    let mut budget = Budget::default();
    let hits = budget.retrieve(&s, "a", &request, 20, 4000).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["timestamp"], 100.);
    assert!(hits[0]["id"].is_string());
    assert!(budget
        .retrieve(&s, "a", &request, 20, 4000)
        .unwrap()
        .is_empty());
    assert_eq!(
        Budget::default()
            .retrieve(&s, "a", &request, 1, 4000)
            .unwrap()
            .len(),
        1
    );
    assert!(Budget::default()
        .retrieve(&s, "a", &request, 20, 2)
        .unwrap()
        .is_empty());
    let anchor: Request =
        serde_json::from_value(json!({"needed":true,"aroundMessageId":"2","window":1})).unwrap();
    assert!(Budget::default()
        .retrieve(&s, "b", &anchor, 20, 4000)
        .unwrap()
        .is_empty());
    assert_eq!(
        Budget::default()
            .retrieve(&s, "a", &anchor, 20, 4000)
            .unwrap()
            .len(),
        1
    );
    for i in 3..40 {
        s.message(&json!({"chat":"a","id":i.to_string(),"sender":"u","text":"约定\\\"".repeat(300),"ts":100.+i as f64,"self":false})).unwrap();
    }
    let results = Budget::default()
        .retrieve(&s, "a", &request, usize::MAX, usize::MAX)
        .unwrap();
    assert!(results.len() <= 20);
    assert!(serde_json::to_string(&results).unwrap().chars().count() <= 4000);
}
