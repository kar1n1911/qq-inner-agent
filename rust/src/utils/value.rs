use serde_json::Value;

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

pub(crate) fn num(value: &Value, key: &str) -> f64 {
    number_or(value, key, 0.)
}

pub(crate) fn number_or(value: &Value, key: &str, fallback: f64) -> f64 {
    value[key].as_f64().unwrap_or(fallback)
}

pub(crate) fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn neutral_missing_values_keep_call_site_fallbacks() {
        let value = json!({"s":"", "n":2, "a":[1]});
        assert_eq!(super::text(&value, "s"), "");
        assert_eq!(super::text(&value, "missing"), "");
        assert_eq!(super::num(&value, "missing"), 0.);
        assert_eq!(super::number_or(&value, "missing", 0.6), 0.6);
        assert_eq!(super::array(&value["a"]).len(), 1);
        assert!(super::array(&value["missing"]).is_empty());
    }
}
