mod affect_support;
use qq_inner_core::{
    engine::policy::pick_length_target,
    persona::affect::{self, disposition, motivation, Disposition::*, LengthBias},
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
