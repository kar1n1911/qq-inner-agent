use std::collections::HashSet;

const OVERLAP_LOW: f64 = 0.35;
const OVERLAP_HIGH: f64 = 0.75;

/// Symmetric shared-feature score, independent of any chat or port policy.
pub(crate) fn set_overlap(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.;
    }
    ((a.intersection(b).count() as f64 / a.len().min(b.len()) as f64 - OVERLAP_LOW)
        / (OVERLAP_HIGH - OVERLAP_LOW))
        .clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    #[test]
    fn score_bounds_and_symmetry() {
        let a = HashSet::from(["a".into(), "b".into(), "c".into()]);
        let b = HashSet::from(["a".into(), "b".into()]);
        assert_eq!(super::set_overlap(&a, &b), super::set_overlap(&b, &a));
        assert_eq!(super::set_overlap(&a, &HashSet::new()), 0.);
        assert_eq!(super::set_overlap(&a, &a), 1.);
    }
}
