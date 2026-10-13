//! Shared, behavior-free primitives. Keep protocol and domain validation at call sites.
pub(crate) mod crypto;
pub(crate) mod score;
pub(crate) mod text;
pub(crate) mod value;

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn representative_call_sites_keep_their_existing_decisions() {
        assert!(!crate::memory::valid_text(&json!("\u{feff}"), 10));
        assert!(crate::memory::valid_text(&json!("\u{0085}"), 10));
        assert_eq!(crate::transport::js_number(&json!("\u{feff}4\u{feff}")), 4.);
        assert_eq!(crate::engine::policy::clip_chars("😀x", 1), "😀");
        let a = HashSet::from(["a".into(), "b".into(), "c".into(), "d".into(), "e".into()]);
        let b = HashSet::from(["a".into(), "b".into(), "f".into(), "g".into(), "h".into()]);
        assert_eq!(crate::memory::sharing::share(&a, &b).to_bits(),
            crate::media::media_select::share(&a, &b).to_bits());
        assert_eq!(crate::memory::sharing::share(&a, &b).to_bits(),
            ((2.0_f64 / 5.0 - 0.35) / 0.4).clamp(0., 1.).to_bits());
    }
}
