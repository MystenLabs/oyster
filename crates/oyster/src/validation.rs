/// Reserved bucket names that would shadow infrastructure routes.
///
/// This list is hardcoded because Axum's `Router` doesn't expose its registered
/// routes, and the registrations use heterogeneous methods (`.route`, `.nest`,
/// `.merge`), so a shared list wouldn't simplify things.
const RESERVED_BUCKET_NAMES: [&str; 4] = ["health", "ready", "metrics", "api"];

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
}
