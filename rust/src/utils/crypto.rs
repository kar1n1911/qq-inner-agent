/// RFC 4122 UUID v4 using the existing random source.
pub(crate) fn uuid_v4() -> String {
    let mut bytes: [u8; 16] = rand::random();
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

#[cfg(test)]
mod tests {
    #[test]
    fn uuid_v4_keeps_version_variant_and_shape() {
        let value = super::uuid_v4();
        assert_eq!(value.len(), 36);
        assert_eq!(value.chars().nth(14), Some('4'));
        assert!(matches!(value.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
        assert!(value.bytes().enumerate().all(|(i, b)| [8, 13, 18, 23].contains(&i) && b == b'-' ||
            ![8, 13, 18, 23].contains(&i) && b.is_ascii_hexdigit()));
    }
}
