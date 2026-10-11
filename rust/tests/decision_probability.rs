use qq_inner_core::{
    engine::decision::topic_probability,
    media::media_select::{Config, GroupActivity},
};

fn group(quiet: bool, since_human: f64) -> GroupActivity {
    GroupActivity {
        activity: if quiet { 0. } else { 1. },
        rate: 1.,
        since_human,
        quiet,
        awake: true,
        short_density: 0.,
        media_rate: 0.,
    }
}

#[test]
fn a4_defaults_thresholds_cap_and_disabled_media() {
    let c = Config::default();
    assert!(!c.enabled);
    assert_eq!((c.p_active, c.p_quiet, c.quiet_cap), (0.04, 0.02, 0.15));
    for quiet in [false, true] {
        let mut g = group(quiet, 299.999);
        assert_eq!(topic_probability(&g, 300., 300., &c), 0.);
        g.since_human = 300.;
        let expected = if quiet { 0.0025 } else { 0.005 };
        assert!((topic_probability(&g, 300., 300., &c) - expected).abs() < 1e-12);
        assert_eq!(topic_probability(&g, 0., 300., &c), 0.);
        assert_eq!(topic_probability(&g, -1., 300., &c), 0.);
        g.awake = false;
        assert_eq!(topic_probability(&g, 300., 300., &c), 0.);
    }
    let capped = Config {
        quiet_cap: 0.01,
        ..c.clone()
    };
    for t in [300., 900., 3600., 86400., f64::INFINITY] {
        assert!(topic_probability(&group(true, t), t, 300., &capped) <= 0.01);
        assert!(
            topic_probability(&group(true, t), t, 300., &capped)
                < topic_probability(&group(true, t), t, 300., &c)
        );
    }
    assert_eq!(
        topic_probability(&group(true, f64::INFINITY), f64::INFINITY, 300., &c),
        0.15
    );
    for quiet in [false, true] {
        let zero = Config {
            p_active: 0.,
            p_quiet: 0.,
            ..c.clone()
        };
        assert_eq!(
            topic_probability(&group(quiet, f64::INFINITY), f64::INFINITY, 300., &zero),
            0.
        );
    }
}

#[test]
fn a4_l697_longer_silence_never_lowers_probability_and_knobs_are_independent() {
    // docs/working/prompt-and-learning-design.md L697: independent active/quiet
    // probabilities. Hold the regime fixed when checking the time direction.
    let c = Config::default();
    for quiet in [false, true] {
        for pause in [1., 60., 300., 3600.] {
            let mut previous = 0.;
            for i in 1..=1000 {
                let t = pause * i as f64;
                let p = topic_probability(&group(quiet, t), t, pause, &c);
                assert!(p > previous, "ramps must still rise beyond 3 * pause");
                assert!(topic_probability(&group(quiet, t), t + 1., pause, &c) >= p);
                assert!(topic_probability(&group(quiet, t + 1.), t, pause, &c) >= p);
                previous = p;
            }
        }
        let g = group(quiet, 600.);
        let baseline = topic_probability(&g, 600., 300., &c);
        let active = Config {
            p_active: 0.,
            ..c.clone()
        };
        let cold = Config {
            p_quiet: 0.,
            ..c.clone()
        };
        assert_eq!(
            topic_probability(&g, 600., 300., &active),
            if quiet { baseline } else { 0. }
        );
        assert_eq!(
            topic_probability(&g, 600., 300., &cold),
            if quiet { 0. } else { baseline }
        );
    }
}

#[test]
fn a4_distribution_is_more_conservative_than_old_quiet_formula() {
    let c = Config::default();
    let mut old_hits = 0;
    let mut new_hits = 0;
    // Deterministic uniform draw grid; compare all previously eligible states.
    for pause in [1_f64, 60., 300., 3600.] {
        let scale = pause.max(60.);
        for n in [1., 2., 3., 10., 100.] {
            let t = n * pause;
            for own in [0., 30., 300., 3000., 30000.] {
                let old = 0.15 * (t / (3. * scale)).clamp(0., 1.) * (own / scale).clamp(0., 1.);
                let new = topic_probability(&group(true, t), own, pause, &c);
                assert!(new <= old);
                for i in 0..10000 {
                    let draw = i as f64 / 10000.;
                    old_hits += usize::from(draw < old);
                    new_hits += usize::from(draw < new);
                }
            }
        }
    }
    assert!(new_hits > 0 && new_hits < old_hits);
    // L697 intentionally admits active groups, formerly blocked altogether.
    assert!(topic_probability(&group(false, 600.), 600., 300., &c) > 0.);
}
