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
//! uploads will be rejected when usage is many small blobs. This makes
//! the cap an *upper* bound on storable unencoded bytes: an account
//! storing many small blobs hits the cap far below its stated budget,
//! because the gate pays `f`'s fixed per-blob metadata overhead once
//! (against the whole remaining budget) while real usage `Σ f(Lᵢ)` pays
//! it per blob.
//!
//! ## `avg_blob_size` inflation — lower-bound mode
//!
//! To instead guarantee that at least `max_unencoded` unencoded bytes
//! are storable when blobs average `s = avg_blob_size`, inflate the
//! effective encoded budget by the per-blob expansion factor `f(s)/s`
//! (e.g. ~70× at 1 MB, ~11× at 10 MB, asymptote ~4.5×). The cap check
//! becomes: reject when `used_encoded > (f(s)/s)·(max_unencoded −
//! new_unencoded)`. Kept in integer arithmetic without computing `f⁻¹`:
//! with `rem = max_unencoded − new_unencoded` and `fs = f(s)`, reject
//! when `u128(used_encoded)·u128(s) > u128(fs)·u128(rem)` (each factor
//! ≤ ~2⁶³, products fit `u128`).
//!
//! The sentinel `avg_blob_size == 0` means "no inflation" and routes to
//! the original `used_encoded > f(rem)` path, so existing accounts are
//! byte-for-byte unchanged. When `s` exceeds the network's max
//! single-blob size, `f(s)` is `None` → saturates to `u64::MAX` → the
//! inflated gate becomes a silent no-op (matching today's saturation
//! semantics; there is no validation error for an oversized `s`).
//!
//! The helpers are pure (no I/O) and unit-testable in isolation.

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

/// Encoded-byte budget that `max_unencoded` unencoded bytes corresponds
/// to under the `avg_blob_size` model. This is the threshold a lowered
/// cap must respect (see `routes::admin::update_max_storage`) — kept in
/// lockstep with the gate in [`enforce_storage_cap`] so a cap change can
/// never admit data the gate would reject, nor orphan stored data.
///
/// * `avg_blob_size == 0` (sentinel): returns `f(max_unencoded)`, the
///   original upper-bound threshold.
/// * `avg_blob_size == s > 0`: returns the inflated budget
///   `f(s)/s · max_unencoded`, computed via a `u128` intermediate and
///   saturated to `u64::MAX`.
///
/// `f(...) == None` (the reference value exceeds the single-blob encoder
/// ceiling for `n_shards`) saturates to `u64::MAX`, making the
/// corresponding gate a no-op.
pub fn effective_encoded_budget(
    max_unencoded: u64,
    avg_blob_size: u64,
    n_shards: NonZeroU16,
) -> u64 {
    if avg_blob_size == 0 {
        return encoded_blob_length_for_n_shards(n_shards, max_unencoded, EncodingType::RS2)
            .unwrap_or(u64::MAX);
    }
    let fs = encoded_blob_length_for_n_shards(n_shards, avg_blob_size, EncodingType::RS2)
        .unwrap_or(u64::MAX);
    let budget = u128::from(fs) * u128::from(max_unencoded) / u128::from(avg_blob_size);
    u64::try_from(budget).unwrap_or(u64::MAX)
}

/// Evaluate the per-account storage cap against the proposed upload.
///
/// Returns `Ok(())` when the upload may proceed, and
/// `Err(CapViolation { … })` when it must be rejected.
///
/// `avg_blob_size` selects the cap semantics: `0` is the no-inflation
/// sentinel (the historical upper-bound check), and any `s > 0` inflates
/// the encoded budget by `f(s)/s` so the cap behaves as a lower bound for
/// blobs averaging `s` (see the module docs).
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
    avg_blob_size: u64,
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
    // Reject when on-chain encoded usage exceeds the encoded budget the
    // remaining unencoded allowance corresponds to. Using the SAME
    // [`effective_encoded_budget`] helper as the admin orphan/shrink
    // threshold guarantees the gate and that threshold never disagree
    // (no rounding-level orphan window). `avg_blob_size == 0` reduces to
    // the historical `used > f(rem)` upper-bound gate; a non-zero value
    // inflates the budget by `f(s)/s` for the lower-bound semantics. A
    // saturated budget (`u64::MAX`, when `f(...) == None`) makes the
    // strict-`>` comparison a no-op.
    let budget = effective_encoded_budget(remaining_unencoded, avg_blob_size, n_shards);
    if used_encoded_bytes > budget {
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
        let err = enforce_storage_cap(1_000, 0, 1_001, 0, n_shards_7()).expect_err("must reject");
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
        let err = enforce_storage_cap(max_unencoded, used_encoded, new_unencoded, 0, n_shards_7())
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
        enforce_storage_cap(max_unencoded, used_encoded, new_unencoded, 0, n_shards_7())
            .expect("must accept");
    }

    #[test]
    fn enforce_storage_cap_zero_new_unencoded() {
        let max_unencoded = 100_000u64;
        let new_unencoded = 0u64;
        let t = threshold(max_unencoded, new_unencoded);
        // At the threshold (not strictly above), allowed.
        enforce_storage_cap(max_unencoded, t, new_unencoded, 0, n_shards_7()).expect("must accept");
        // Strictly above, rejected.
        enforce_storage_cap(max_unencoded, t + 1, new_unencoded, 0, n_shards_7())
            .expect_err("must reject");
    }

    #[test]
    fn enforce_storage_cap_at_boundary() {
        let max_unencoded = 100_000u64;
        let new_unencoded = 1_000u64;
        let t = threshold(max_unencoded, new_unencoded);
        // strict `>` per spec → equal-to-threshold is allowed.
        enforce_storage_cap(max_unencoded, t, new_unencoded, 0, n_shards_7()).expect("must accept");
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
        enforce_storage_cap(max_unencoded, 1_000_000, 0, 0, n_shards_7()).expect("must accept");
        // The new-blob short-circuit still fires for new > max.
        enforce_storage_cap(max_unencoded, 0, max_unencoded + 1, 0, n_shards_7())
            .expect_err("must reject");
    }

    /// Inflated budget for the assertions below — wraps
    /// `effective_encoded_budget` with the test's `n_shards`.
    fn budget(max_unencoded: u64, avg: u64) -> u64 {
        effective_encoded_budget(max_unencoded, avg, n_shards_7())
    }

    #[test]
    fn effective_encoded_budget_sentinel_matches_f() {
        // avg == 0 reproduces the bare f(max_unencoded) threshold.
        let max_unencoded = 100_000u64;
        assert_eq!(budget(max_unencoded, 0), threshold(max_unencoded, 0));
    }

    #[test]
    fn effective_encoded_budget_inflates_for_small_avg() {
        // A small average blob size carries a large per-blob expansion
        // factor f(s)/s, so the inflated budget exceeds f(max_unencoded).
        let max_unencoded = 500_000u64;
        let inflated = budget(max_unencoded, 1_000);
        assert!(
            inflated > threshold(max_unencoded, 0),
            "inflated budget {inflated} must exceed baseline {}",
            threshold(max_unencoded, 0)
        );
    }

    #[test]
    fn enforce_storage_cap_inflation_raises_encoded_ceiling() {
        // Inflation raises the encoded admission ceiling: at a
        // used_encoded just past the baseline f(rem) threshold, avg==0
        // rejects while a small avg still admits (its higher ceiling
        // delivers the lower-bound guarantee on *unencoded* capacity —
        // the account can keep storing small blobs whose per-blob
        // overhead would otherwise trip the bare-f gate early).
        let max_unencoded = 200_000u64;
        let new_unencoded = 1_000u64;
        let t = threshold(max_unencoded, new_unencoded);
        // Baseline (avg==0) rejects strictly above f(rem).
        enforce_storage_cap(max_unencoded, t + 1, new_unencoded, 0, n_shards_7())
            .expect_err("baseline must reject");
        // A small avg inflates the ceiling, so the same usage is admitted.
        enforce_storage_cap(max_unencoded, t + 1, new_unencoded, 1_000, n_shards_7())
            .expect("inflated ceiling must accept");
    }

    #[test]
    fn enforce_storage_cap_inflation_eventually_rejects() {
        // The inflated gate still rejects once used_encoded exceeds the
        // inflated budget for the remaining unencoded bytes.
        let max_unencoded = 200_000u64;
        let new_unencoded = 1_000u64;
        let avg = 1_000u64;
        let b = budget(max_unencoded - new_unencoded, avg);
        enforce_storage_cap(max_unencoded, b, new_unencoded, avg, n_shards_7())
            .expect("at budget must accept");
        enforce_storage_cap(max_unencoded, b + 1, new_unencoded, avg, n_shards_7())
            .expect_err("just past inflated budget must reject");
    }

    #[test]
    fn enforce_storage_cap_inflation_saturates_when_f_none() {
        // avg beyond the single-blob encoder ceiling → f(avg) == None →
        // the inflated budget saturates. When the remaining unencoded
        // allowance is ≥ avg, the budget clamps to u64::MAX, so the
        // strict-`>` gate is a true no-op (admits even u64::MAX-1 used).
        let avg = u64::from(u32::MAX); // f(avg) is None at n_shards=7.
        assert!(
            encoded_blob_length_for_n_shards(n_shards_7(), avg, EncodingType::RS2).is_none(),
            "precondition: f(avg) is None"
        );
        // rem == avg → MAX·rem/avg == MAX → clamps to u64::MAX.
        let max_unencoded = avg;
        assert_eq!(
            effective_encoded_budget(max_unencoded, avg, n_shards_7()),
            u64::MAX,
            "saturated budget must clamp to u64::MAX"
        );
        enforce_storage_cap(max_unencoded, u64::MAX - 1, 0, avg, n_shards_7())
            .expect("saturated gate must accept");
        // The new > max short-circuit still fires regardless of avg.
        enforce_storage_cap(max_unencoded, 0, max_unencoded + 1, avg, n_shards_7())
            .expect_err("new > max still rejects");
    }

    #[test]
    fn enforce_storage_cap_inflation_overflow_safe_near_u64_max() {
        // Extreme used_encoded with a large avg must not panic; the
        // u128 products keep the comparison well-defined.
        let max_unencoded = u64::MAX;
        let avg = 10_000_000u64;
        // Should not panic; result is a definite accept/reject.
        let _ = enforce_storage_cap(max_unencoded, u64::MAX, 0, avg, n_shards_7());
        let _ = effective_encoded_budget(max_unencoded, avg, n_shards_7());
    }
}
