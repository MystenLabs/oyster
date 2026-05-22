//! Per-account storage cap enforcement.
//!
//! Encapsulates the inequality the upload path uses to reject an
//! over-cap upload before doing any on-chain work, given a cap stated in
//! *unencoded* bytes but on-chain usage tracked in *encoded* bytes:
//!
//! Reject when either of
//! 1. `new_unencoded > max_unencoded`, or
//! 2. `used_encoded > f(max_unencoded − new_unencoded)`,
//!
//! holds, where
//! `f = walrus_core::encoding::encoded_blob_length_for_n_shards(n_shards, x, EncodingType::RS2)`.
//!
//! Derivation: equivalent to
//! `f⁻¹(used_encoded) + new_unencoded > max_unencoded` after applying
//! the monotone `f` to both sides — never needs `f⁻¹`. Conservative
//! because `f` is subadditive (each `PooledBlob` carries its own
//! metadata), so `f⁻¹(used_encoded) ≥ used_unencoded`: some legitimate
//! uploads will be rejected when usage is many small blobs. This
//! trade-off is intentional to keep cap enforcement decoupled from the
//! `blobs` table.
//!
//! The helper is pure (no I/O) and unit-testable in isolation.

use std::num::NonZeroU16;

use walrus_core::{EncodingType, encoding::encoded_blob_length_for_n_shards};

/// Reason an upload was rejected by the per-account storage cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapViolation {
    /// The configured cap, in unencoded bytes.
    pub max_unencoded_bytes: u64,
    /// On-chain encoded usage observed at check time, in bytes.
    pub used_encoded_bytes: u64,
    /// Unencoded size of the new blob the caller tried to upload.
    pub new_unencoded_bytes: u64,
}

/// Evaluate the per-account storage cap against the proposed upload.
///
/// Returns `Ok(())` when the upload may proceed, and
/// `Err(CapViolation { … })` when it must be rejected.
///
/// When `encoded_blob_length_for_n_shards` returns `None` (the
/// unencoded reference value exceeds the largest blob this network's
/// `n_shards` can encode as a single blob), the threshold is treated
/// as `u64::MAX`: the cap is effectively unreachable through any
/// realistic accumulation of encoded usage, so the check becomes a
/// no-op rather than a spurious rejection. This case happens in
/// practice on Walrus testbeds with small `n_shards` and the default
/// 5 GB cap, where a single-blob 5 GB encoded length doesn't fit a
/// `u16` symbol size — but `used_encoded_bytes` is still bounded above
/// by `u64::MAX`, so the strict-`>` comparison below is safe.
pub fn enforce_storage_cap(
    max_unencoded_bytes: u64,
    used_encoded_bytes: u64,
    new_unencoded_bytes: u64,
    n_shards: NonZeroU16,
) -> Result<(), CapViolation> {
    // Short-circuit: the new blob alone exceeds the cap. This branch
    // doesn't need any on-chain reads or `f` invocation, and lets the
    // upload path skip lazy-creating a `StoragePool` for first-write
    // accounts whose very first upload is over-sized.
    if new_unencoded_bytes > max_unencoded_bytes {
        return Err(CapViolation {
            max_unencoded_bytes,
            used_encoded_bytes,
            new_unencoded_bytes,
        });
    }
    let remaining_unencoded = max_unencoded_bytes - new_unencoded_bytes;
    let threshold =
        encoded_blob_length_for_n_shards(n_shards, remaining_unencoded, EncodingType::RS2)
            .unwrap_or(u64::MAX);
    if used_encoded_bytes > threshold {
        return Err(CapViolation {
            max_unencoded_bytes,
            used_encoded_bytes,
            new_unencoded_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n_shards_7() -> NonZeroU16 {
        NonZeroU16::new(7).expect("7 is non-zero")
    }

    /// Threshold convenience for the assertions below — wraps
    /// `encoded_blob_length_for_n_shards` and unwraps because the test
    /// inputs are well-formed.
    fn threshold(max_unencoded: u64, new_unencoded: u64) -> u64 {
        encoded_blob_length_for_n_shards(
            n_shards_7(),
            max_unencoded - new_unencoded,
            EncodingType::RS2,
        )
        .expect("synthetic inputs encode")
    }

    #[test]
    fn enforce_storage_cap_rejects_when_new_exceeds_max() {
        let err = enforce_storage_cap(1_000, 0, 1_001, n_shards_7()).expect_err("must reject");
        assert_eq!(err.max_unencoded_bytes, 1_000);
        assert_eq!(err.new_unencoded_bytes, 1_001);
        assert_eq!(err.used_encoded_bytes, 0);
    }

    // With n_shards=7, the RS2 max blob size is ~983 KiB (15 source
    // symbols × u16::MAX symbol bytes), so the per-test sizes below
    // stay well under that ceiling.

    #[test]
    fn enforce_storage_cap_rejects_when_encoded_usage_too_high() {
        let max_unencoded = 100_000u64;
        let new_unencoded = 1_000u64;
        let t = threshold(max_unencoded, new_unencoded);
        let used_encoded = t + 1;
        let err = enforce_storage_cap(max_unencoded, used_encoded, new_unencoded, n_shards_7())
            .expect_err("must reject");
        assert_eq!(err.max_unencoded_bytes, max_unencoded);
        assert_eq!(err.used_encoded_bytes, used_encoded);
        assert_eq!(err.new_unencoded_bytes, new_unencoded);
    }

    #[test]
    fn enforce_storage_cap_allows_under_cap() {
        let max_unencoded = 100_000u64;
        let new_unencoded = 1_000u64;
        let t = threshold(max_unencoded, new_unencoded);
        let used_encoded = t.saturating_sub(1);
        enforce_storage_cap(max_unencoded, used_encoded, new_unencoded, n_shards_7())
            .expect("must accept");
    }

    #[test]
    fn enforce_storage_cap_zero_new_unencoded() {
        let max_unencoded = 100_000u64;
        let new_unencoded = 0u64;
        let t = threshold(max_unencoded, new_unencoded);
        // At the threshold (not strictly above), allowed.
        enforce_storage_cap(max_unencoded, t, new_unencoded, n_shards_7()).expect("must accept");
        // Strictly above, rejected.
        enforce_storage_cap(max_unencoded, t + 1, new_unencoded, n_shards_7())
            .expect_err("must reject");
    }

    #[test]
    fn enforce_storage_cap_at_boundary() {
        let max_unencoded = 100_000u64;
        let new_unencoded = 1_000u64;
        let t = threshold(max_unencoded, new_unencoded);
        // strict `>` per spec → equal-to-threshold is allowed.
        enforce_storage_cap(max_unencoded, t, new_unencoded, n_shards_7()).expect("must accept");
    }

    #[test]
    fn enforce_storage_cap_saturates_when_threshold_overflows() {
        // With n_shards=7, encoded_blob_length_for_n_shards returns
        // None for inputs that would need a symbol size > u16::MAX.
        // The helper saturates the threshold to u64::MAX, so the
        // upload is allowed even though the unencoded reference value
        // is unrepresentable as a single encoded blob.
        let max_unencoded = u64::from(u32::MAX); // ~4.3 GB
        // Sanity-check: f returns None at this size with n_shards=7.
        assert!(
            encoded_blob_length_for_n_shards(n_shards_7(), max_unencoded, EncodingType::RS2)
                .is_none()
        );
        enforce_storage_cap(max_unencoded, 1_000_000, 0, n_shards_7()).expect("must accept");
        // The new-blob short-circuit still fires for new > max.
        enforce_storage_cap(max_unencoded, 0, max_unencoded + 1, n_shards_7())
            .expect_err("must reject");
    }
}
