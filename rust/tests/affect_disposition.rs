mod affect_support;
use qq_inner_core::persona::affect::{self, disposition, Disposition::*};
#[test]
fn quadrants_and_circuit() {
    assert_eq!(disposition(-1., -1.), Angry);
    assert_eq!(disposition(-1., 1.), Withdrawn);
    assert_eq!(disposition(1., 1.), Scrutinizing);
    assert_eq!(disposition(1., -1.), Supportive);
    assert!(Angry.motivation() > 1. && Withdrawn.motivation() < Scrutinizing.motivation());
    assert_eq!(Scrutinizing.adjust_length("medium", false), "long");
    assert_eq!(Angry.adjust_length("medium", false), "short");
    let s = affect_support::store();
    for _ in 0..3 {
        assert!(affect::burst_allowed(&s, "a", "1").unwrap());
        affect::reserve_burst(&s, "a", "1").unwrap();
    }
    assert!(!affect::burst_allowed(&s, "a", "1").unwrap());
    assert!(affect::reserve_burst(&s, "a", "1").is_err());
    assert!(affect::burst_allowed(&s, "b", "1").unwrap());
    assert!(affect::burst_allowed(&s, "a", "3").unwrap());
}

#[test]
fn length_adjustments_preserve_variation_and_addressed_floor() {
    for length in ["tiny", "short", "medium", "long"] {
        assert_eq!(Supportive.adjust_length(length, false), length);
        assert_eq!(Withdrawn.adjust_length(length, false), length);
    }
    assert_eq!(Angry.adjust_length("long", true), "medium");
    assert_eq!(Angry.adjust_length("short", true), "short");
    assert_eq!(Angry.adjust_length("short", false), "tiny");
    assert_eq!(Angry.adjust_length("tiny", false), "tiny");
    assert_eq!(Scrutinizing.adjust_length("tiny", false), "short");
    assert_eq!(Scrutinizing.adjust_length("short", true), "medium");
    assert_eq!(Scrutinizing.adjust_length("long", true), "long");
}
