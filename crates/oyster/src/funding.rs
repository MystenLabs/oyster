//! Cross-cutting WAL/SUI funding-amount type, shared between the
//! synchronous 402-error path and the asynchronous funding-required
//! webhook payload.
//!
//! Amounts are stored as raw `u64` (FROST / MIST). The custom `Serialize`
//! impl renders them as decimal strings on the wire so `u64` values
//! above 2^53 survive JSON round-trips.

use serde::{Serialize, Serializer, ser::SerializeStruct};
use utoipa::ToSchema;

/// WAL/SUI top-up estimate attached to both the synchronous 402 response
/// body and the `account.funding_required` webhook payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ToSchema)]
pub struct FundingAmount {
    /// Required WAL, in FROST units (1 WAL = 1e9 FROST).
    #[schema(value_type = String, example = "12345")]
    pub wal_frost: u64,
    /// Required SUI, in MIST units (1 SUI = 1e9 MIST).
    #[schema(value_type = String, example = "10000000")]
    pub sui_mist: u64,
}

impl Serialize for FundingAmount {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("FundingAmount", 2)?;
        st.serialize_field("wal_frost", &self.wal_frost.to_string())?;
        st.serialize_field("sui_mist", &self.sui_mist.to_string())?;
        st.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_decimal_strings() {
        let json = serde_json::to_value(FundingAmount {
            wal_frost: 1_234,
            sui_mist: 10_000_000,
        })
        .unwrap();
        assert_eq!(json["wal_frost"], "1234");
        assert_eq!(json["sui_mist"], "10000000");
    }

    #[test]
    fn round_trips_large_u64_values() {
        // Above 2^53 — the precise reason we serialize as strings.
        let big = (1u64 << 60) + 7;
        let json = serde_json::to_value(FundingAmount {
            wal_frost: big,
            sui_mist: 0,
        })
        .unwrap();
        assert_eq!(json["wal_frost"], big.to_string());
    }
}
