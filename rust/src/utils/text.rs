/// JavaScript trim whitespace: includes BOM, excludes U+0085.
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| matches!(c, '\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}

/// Truncate by Unicode scalar values. Rust cannot hold isolated UTF-16 surrogates;
/// callers requiring UTF-16 unit counts stay separate.
pub(crate) fn clip_chars(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn js_trim_only_trims_js_whitespace() {
        assert_eq!(super::js_trim("\u{feff} x \u{feff}"), "x");
        assert_eq!(super::js_trim("\u{0085}x\u{0085}"), "\u{0085}x\u{0085}");
        assert_eq!(super::js_trim("\u{feff}"), "");
        assert_eq!(super::js_trim(""), "");
    }

    #[test]
    fn scalar_clip_differs_from_utf16_count() {
        assert_eq!(super::clip_chars("😀x", 1), "😀");
        assert_eq!("😀".encode_utf16().count(), 2);
    }
}
