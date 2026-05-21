//! Read on-chain Sui object state for objects Oyster cares about.
//!
//! Currently exposes one helper: [`read_storage_pool_state`], which
//! pulls `StoragePoolInnerV1::{storage.storage_size, used_encoded_bytes}`
//! off the on-chain `StoragePool` via gRPC `StateService.ListDynamicFields`.

use std::error::Error;

use futures::TryStreamExt;
use prost_types::Value;
use sui_rpc::{
    Client as SuiGrpcClient,
    field::FieldMask,
    proto::sui::rpc::v2::ListDynamicFieldsRequest,
};
use sui_types::base_types::ObjectID;

/// On-chain `StoragePoolInnerV1` snapshot. Encoded-byte counters only;
/// other fields (blob_count, the `blobs` table) are deliberately not
/// surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnChainStoragePoolState {
    /// `storage.storage_size` — total encoded bytes reserved by the pool.
    pub reserved_encoded_bytes: u64,
    /// `used_encoded_bytes` — sum of all registered blobs' encoded sizes.
    pub used_encoded_bytes: u64,
}

/// Fetch the on-chain `StoragePoolInnerV1` for `pool_id`.
///
/// Walrus stores the inner state in a single `dynamic_field` keyed by
/// `u64` (the `StoragePool.version`) — see `storage_pool::create`. We
/// look it up by walking the parent's dynamic fields via
/// `StateService.ListDynamicFields` and filtering by `value_type`
/// (robust to package upgrades, matching the `parse_object_type`
/// suffix-match pattern used elsewhere in this crate).
pub async fn read_storage_pool_state(
    rpc_url: &str,
    pool_id: ObjectID,
) -> Result<OnChainStoragePoolState, Box<dyn Error + Send + Sync>> {
    let client = SuiGrpcClient::new(rpc_url)?;
    let request = ListDynamicFieldsRequest::default()
        .with_parent(pool_id.to_string())
        .with_page_size(50)
        .with_read_mask(FieldMask {
            paths: vec!["value_type".to_string(), "field_object.json".to_string()],
        });
    let stream = client.list_dynamic_fields(request);
    futures::pin_mut!(stream);
    while let Some(field) = stream.try_next().await? {
        let value_type = match field.value_type.as_deref() {
            Some(t) => t,
            None => continue,
        };
        if !value_type.ends_with("::storage_pool::StoragePoolInnerV1") {
            continue;
        }
        let field_json = field.field_object.as_ref().and_then(|c| c.json.as_deref());
        let Some(field_json) = field_json else {
            continue;
        };
        // The dynamic-field wrapper `Field<u64, StoragePoolInnerV1>`
        // renders as `{ id, name, value: <inner> }`; the inner Move
        // struct is the `value` sub-field.
        let inner =
            unwrap_field_value(field_json).ok_or_else(|| -> Box<dyn Error + Send + Sync> {
                format!("could not locate Field.value on dynamic field for pool {pool_id}").into()
            })?;
        return parse_storage_pool_inner_v1(inner).ok_or_else(|| {
            format!("could not parse StoragePoolInnerV1 JSON for pool {pool_id}").into()
        });
    }
    Err(format!("no StoragePoolInnerV1 dynamic field found on pool {pool_id}").into())
}

/// Unwrap the `value` sub-field of a `0x2::dynamic_field::Field<K, V>`
/// rendered as JSON, returning the inner `V` struct value.
fn unwrap_field_value(field_object_json: &Value) -> Option<&Value> {
    let s = unwrap_struct(field_object_json)?;
    s.fields.get("value")
}

/// Pure parser for the prost JSON shape gRPC returns for
/// `StoragePoolInnerV1`. Exposed for unit testing without a Sui RPC.
pub fn parse_storage_pool_inner_v1(value: &Value) -> Option<OnChainStoragePoolState> {
    let outer = unwrap_struct(value)?;
    let storage = unwrap_struct(outer.fields.get("storage")?)?;
    let reserved = parse_u64_value(storage.fields.get("storage_size")?)?;
    let used = parse_u64_value(outer.fields.get("used_encoded_bytes")?)?;
    Some(OnChainStoragePoolState {
        reserved_encoded_bytes: reserved,
        used_encoded_bytes: used,
    })
}

fn unwrap_struct(v: &Value) -> Option<&prost_types::Struct> {
    match v.kind.as_ref()? {
        prost_types::value::Kind::StructValue(s) => Some(s),
        _ => None,
    }
}

/// Sui's JSON encoder commonly renders `u64` as a JSON string to avoid
/// precision loss; accept either string or number representations.
fn parse_u64_value(v: &Value) -> Option<u64> {
    match v.kind.as_ref()? {
        prost_types::value::Kind::StringValue(s) => s.parse().ok(),
        prost_types::value::Kind::NumberValue(n) if *n >= 0.0 && n.is_finite() => Some(*n as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use prost_types::{Struct, Value, value::Kind};

    use super::*;

    fn string_value(s: &str) -> Value {
        Value {
            kind: Some(Kind::StringValue(s.to_string())),
        }
    }

    fn number_value(n: f64) -> Value {
        Value {
            kind: Some(Kind::NumberValue(n)),
        }
    }

    fn struct_value(fields: &[(&str, Value)]) -> Value {
        let mut s = Struct::default();
        for (k, v) in fields {
            s.fields.insert((*k).to_string(), v.clone());
        }
        Value {
            kind: Some(Kind::StructValue(s)),
        }
    }

    #[test]
    fn parses_string_u64s() {
        let inner = struct_value(&[
            (
                "storage",
                struct_value(&[("storage_size", string_value("1048576"))]),
            ),
            ("used_encoded_bytes", string_value("512")),
        ]);
        let parsed = parse_storage_pool_inner_v1(&inner).expect("must parse");
        assert_eq!(parsed.reserved_encoded_bytes, 1_048_576);
        assert_eq!(parsed.used_encoded_bytes, 512);
    }

    #[test]
    fn parses_number_u64s() {
        let inner = struct_value(&[
            (
                "storage",
                struct_value(&[("storage_size", number_value(2048.0))]),
            ),
            ("used_encoded_bytes", number_value(7.0)),
        ]);
        let parsed = parse_storage_pool_inner_v1(&inner).expect("must parse");
        assert_eq!(parsed.reserved_encoded_bytes, 2048);
        assert_eq!(parsed.used_encoded_bytes, 7);
    }

    #[test]
    fn missing_storage_returns_none() {
        let inner = struct_value(&[("used_encoded_bytes", string_value("0"))]);
        assert!(parse_storage_pool_inner_v1(&inner).is_none());
    }

    #[test]
    fn missing_used_returns_none() {
        let inner = struct_value(&[(
            "storage",
            struct_value(&[("storage_size", string_value("1024"))]),
        )]);
        assert!(parse_storage_pool_inner_v1(&inner).is_none());
    }

    #[test]
    fn missing_storage_size_returns_none() {
        let inner = struct_value(&[
            ("storage", struct_value(&[("foo", string_value("1024"))])),
            ("used_encoded_bytes", string_value("0")),
        ]);
        assert!(parse_storage_pool_inner_v1(&inner).is_none());
    }

    #[test]
    fn non_struct_input_returns_none() {
        assert!(parse_storage_pool_inner_v1(&string_value("nope")).is_none());
    }

    #[test]
    fn bad_u64_string_returns_none() {
        let inner = struct_value(&[
            (
                "storage",
                struct_value(&[("storage_size", string_value("not-a-number"))]),
            ),
            ("used_encoded_bytes", string_value("0")),
        ]);
        assert!(parse_storage_pool_inner_v1(&inner).is_none());
    }

    #[test]
    fn unwrap_field_value_returns_inner_value() {
        let inner = struct_value(&[
            (
                "storage",
                struct_value(&[("storage_size", string_value("4096"))]),
            ),
            ("used_encoded_bytes", string_value("100")),
        ]);
        let field_obj = struct_value(&[
            ("id", struct_value(&[("id", string_value("0xdeadbeef"))])),
            ("name", string_value("1")),
            ("value", inner),
        ]);
        let extracted = unwrap_field_value(&field_obj).expect("must find value");
        let parsed = parse_storage_pool_inner_v1(extracted).expect("must parse");
        assert_eq!(parsed.reserved_encoded_bytes, 4096);
        assert_eq!(parsed.used_encoded_bytes, 100);
    }

    #[test]
    fn unwrap_field_value_missing_returns_none() {
        let field_obj = struct_value(&[("name", string_value("1"))]);
        assert!(unwrap_field_value(&field_obj).is_none());
    }
}
