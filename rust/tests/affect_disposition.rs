mod affect_support;
use qq_inner_core::persona::affect::{self, disposition, Disposition::*};

#[test]
fn dead_zone_neutralizes_each_coordinate_independently() {
    for valence in [-0.049, -0.01, -0.002614, 0., 0.01, 0.049] {
        for rationality in [-0.049, -0.01, 0., 0.000829, 0.01, 0.049] {
            assert_eq!(disposition(valence, rationality), Scrutinizing);
        }
    }
    assert_eq!(disposition(-0.2, 0.01), Withdrawn);
    assert_eq!(disposition(-0.2, -0.01), Withdrawn);
    assert_eq!(disposition(-0.01, -0.2), Supportive);
    assert_eq!(disposition(-0.2, -0.2), Angry);
    // Only magnitudes strictly below the threshold are neutralized.
    assert_eq!(disposition(-0.05, -0.05), Angry);
    assert_eq!(disposition(-0.05, 0.05), Withdrawn);
    assert_eq!(disposition(0.05, -0.05), Supportive);
    assert_eq!(disposition(0.05, 0.05), Scrutinizing);
}

#[test]
fn motivation_preserves_gains_and_softens_withdrawal() {
    assert_eq!(Withdrawn.motivation(), 0.5);
    assert_eq!(Angry.motivation(), 1.4);
    assert_eq!(Scrutinizing.motivation(), 0.7);
    assert_eq!(Supportive.motivation(), 1.2);
}

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
