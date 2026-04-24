/// Reserved bucket names that would shadow infrastructure routes.
///
/// This list is hardcoded because Axum's `Router` doesn't expose its registered
/// routes, and the registrations use heterogeneous methods (`.route`, `.nest`,
/// `.merge`), so a shared list wouldn't simplify things.
const RESERVED_BUCKET_NAMES: [&str; 4] = ["health", "ready", "metrics", "api"];

/// Maximum number of tags allowed per blob.
pub const MAX_TAGS: usize = 10;
/// Maximum UTF-8 byte length of a tag key.
pub const MAX_TAG_KEY_LEN: usize = 128;
/// Maximum UTF-8 byte length of a tag value.
pub const MAX_TAG_VALUE_LEN: usize = 256;
/// Maximum combined UTF-8 byte size across all tag keys and values in a set.
pub const MAX_TAG_SET_BYTES: usize = 2 * 1024;

// Note: S3 documents tag length limits in UTF-16 code units. We count UTF-8
// bytes instead, which is strictly stricter for non-ASCII input and identical
// for ASCII (the overwhelmingly common case).
fn is_allowed_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ' ' | '+' | '-' | '=' | '.' | '_' | ':' | '/' | '@')
}

/// Validate a single tag key/value pair.
pub fn validate_tag_kv(key: &str, value: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("tag key must be non-empty".into());
    }
    if key.len() > MAX_TAG_KEY_LEN {
        return Err(format!(
            "tag key exceeds {MAX_TAG_KEY_LEN} bytes (got {})",
            key.len()
        ));
    }
    if value.len() > MAX_TAG_VALUE_LEN {
        return Err(format!(
            "tag value exceeds {MAX_TAG_VALUE_LEN} bytes (got {})",
            value.len()
        ));
    }
    for c in key.chars() {
        if !is_allowed_tag_char(c) {
            return Err(format!("tag key contains disallowed character: {c:?}"));
        }
    }
    for c in value.chars() {
        if !is_allowed_tag_char(c) {
            return Err(format!("tag value contains disallowed character: {c:?}"));
        }
    }
    Ok(())
}

/// Validate a complete set of tags applied together.
///
/// Enforces per-entry charset/length limits, an overall set-size cap, and
/// rejects duplicate keys.
pub fn validate_tag_set(tags: &[(String, String)]) -> Result<(), String> {
    if tags.len() > MAX_TAGS {
        return Err(format!(
            "at most {MAX_TAGS} tags allowed (got {})",
            tags.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut total = 0usize;
    for (k, v) in tags {
        validate_tag_kv(k, v)?;
        if !seen.insert(k.as_str()) {
            return Err(format!("duplicate tag key: {k}"));
        }
        total += k.len() + v.len();
    }
    if total > MAX_TAG_SET_BYTES {
        return Err(format!(
            "tag set exceeds {MAX_TAG_SET_BYTES} bytes (got {total})"
        ));
    }
    Ok(())
}

/// Validate a bucket name against S3 naming rules.
///
/// Rules:
/// - 3-63 characters
/// - Only lowercase letters, numbers, and hyphens
/// - Must start and end with a letter or number
/// - No consecutive hyphens
/// - Cannot be formatted as an IP address
/// - Cannot be a reserved infrastructure route name
pub fn validate_bucket_name(name: &str) -> Result<(), String> {
    let len = name.len();
    if !(3..=63).contains(&len) {
        return Err(format!(
            "bucket name must be between 3 and 63 characters, got {len}"
        ));
    }

    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err("bucket name must start with a lowercase letter or number".into());
    }
    if !bytes[len - 1].is_ascii_lowercase() && !bytes[len - 1].is_ascii_digit() {
        return Err("bucket name must end with a lowercase letter or number".into());
    }

    for (i, &b) in bytes.iter().enumerate() {
        if !b.is_ascii_lowercase() && !b.is_ascii_digit() && b != b'-' {
            return Err(
                "bucket name can only contain lowercase letters, numbers, and hyphens".into(),
            );
        }
        if b == b'-' && i > 0 && bytes[i - 1] == b'-' {
            return Err("bucket name cannot contain consecutive hyphens".into());
        }
    }

    // Reject IP address format (e.g., 192.168.5.4)
    if name.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("bucket name cannot be formatted as an IP address".into());
    }

    if RESERVED_BUCKET_NAMES.contains(&name) {
        return Err(format!("bucket name '{name}' is reserved"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        assert!(validate_bucket_name("my-bucket").is_ok());
        assert!(validate_bucket_name("abc").is_ok());
        assert!(validate_bucket_name("bucket-123").is_ok());
        assert!(validate_bucket_name("a1b2c3").is_ok());
    }

    #[test]
    fn too_short() {
        assert!(validate_bucket_name("ab").is_err());
    }

    #[test]
    fn too_long() {
        let name = "a".repeat(64);
        assert!(validate_bucket_name(&name).is_err());
    }

    #[test]
    fn uppercase_rejected() {
        assert!(validate_bucket_name("My-Bucket").is_err());
    }

    #[test]
    fn starts_with_hyphen() {
        assert!(validate_bucket_name("-bucket").is_err());
    }

    #[test]
    fn ends_with_hyphen() {
        assert!(validate_bucket_name("bucket-").is_err());
    }

    #[test]
    fn consecutive_hyphens() {
        assert!(validate_bucket_name("my--bucket").is_err());
    }

    #[test]
    fn ip_address_rejected() {
        assert!(validate_bucket_name("192.168.5.4").is_err());
    }

    #[test]
    fn dots_rejected() {
        // Dots are not in the allowed set (lowercase, digits, hyphens)
        assert!(validate_bucket_name("my.bucket").is_err());
    }

    #[test]
    fn reserved_names_rejected() {
        for name in &["health", "ready", "metrics", "api"] {
            assert!(
                validate_bucket_name(name).is_err(),
                "expected '{name}' to be rejected as reserved"
            );
        }
    }

    #[test]
    fn reserved_name_substrings_allowed() {
        for name in &["healthy", "api-store", "my-metrics"] {
            assert!(
                validate_bucket_name(name).is_ok(),
                "expected '{name}' to be allowed"
            );
        }
    }

    #[test]
    fn tag_set_valid() {
        let tags = vec![
            ("env".into(), "prod".into()),
            ("team".into(), "platform".into()),
        ];
        assert!(validate_tag_set(&tags).is_ok());
    }

    #[test]
    fn tag_set_empty_valid() {
        assert!(validate_tag_set(&[]).is_ok());
    }

    #[test]
    fn tag_set_rejects_too_many() {
        let tags: Vec<_> = (0..11).map(|i| (format!("k{i}"), "v".into())).collect();
        assert!(validate_tag_set(&tags).is_err());
    }

    #[test]
    fn tag_set_rejects_long_key() {
        let tags = vec![("a".repeat(MAX_TAG_KEY_LEN + 1), "v".into())];
        assert!(validate_tag_set(&tags).is_err());
    }

    #[test]
    fn tag_set_rejects_long_value() {
        let tags = vec![("k".into(), "v".repeat(MAX_TAG_VALUE_LEN + 1))];
        assert!(validate_tag_set(&tags).is_err());
    }

    #[test]
    fn tag_set_rejects_total_bytes() {
        let tags: Vec<_> = (0..9)
            .map(|i| (format!("key{i:02}"), "v".repeat(240)))
            .collect();
        assert!(validate_tag_set(&tags).is_err());
    }

    #[test]
    fn tag_set_rejects_bad_char() {
        let tags = vec![("bad!".into(), "v".into())];
        assert!(validate_tag_set(&tags).is_err());
    }

    #[test]
    fn tag_set_rejects_duplicate() {
        let tags = vec![("a".into(), "1".into()), ("a".into(), "2".into())];
        assert!(validate_tag_set(&tags).is_err());
    }

    #[test]
    fn tag_kv_allows_empty_value() {
        assert!(validate_tag_kv("k", "").is_ok());
    }

    #[test]
    fn tag_kv_rejects_empty_key() {
        assert!(validate_tag_kv("", "v").is_err());
    }
}
