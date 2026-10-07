mod affect_support;
use qq_inner_core::persona::affect::{self, Dimension::*};
#[test]
#[should_panic(expected = "agreement must never aggregate")]
fn agreement_never_becomes_affinity() {
    let s = affect_support::store();
    affect::update(&s, "a", "person:u", Affinity, Agreement, 1., 1., "1", 100.).unwrap();
}
#[test]
fn independent_ratings_do_not_change_relationship() {
    let s = affect_support::store();
    affect::rate(&s, "a", "1", -1., 1., 1., 100.).unwrap();
    assert_eq!(
        affect::read(&s, "a", "person:u", Affinity, 100.).unwrap(),
        0.
    );
    assert!(!affect::content_allowed("好感度0.7"));
    assert!(affect::content_allowed("我不认同这件事"));
}
