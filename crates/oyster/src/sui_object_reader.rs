//! Read on-chain Sui object state for objects Oyster cares about.
//!
//! Currently exposes two helpers:
//! - [`read_storage_pool_state`] pulls `StoragePoolInnerV1::{storage.storage_size,
//!   used_encoded_bytes}` off the on-chain `StoragePool` via gRPC
//!   `StateService.ListDynamicFields`.
//! - [`lookup_pooled_blob_object_id`] walks the pool's `blobs: ObjectTable<u256,
//!   PooledBlob>` dynamic-object fields searching for a `PooledBlob` keyed by
//!   the given content-addressed `BlobId`. Used by the register-PTB self-heal
//!   path when `dynamic_field::add` aborts with `EFieldAlreadyExists`.

use std::error::Error;

use futures::TryStreamExt;
use prost_types::Value;
use sui_rpc::{
    Client as SuiGrpcClient,
    field::FieldMask,
    proto::sui::rpc::v2::{ListDynamicFieldsRequest, dynamic_field::DynamicFieldKind},
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
    let inner = read_storage_pool_inner_json(rpc_url, pool_id).await?;
    parse_storage_pool_inner_v1(&inner)
        .ok_or_else(|| format!("could not parse StoragePoolInnerV1 JSON for pool {pool_id}").into())
}

/// Fetch the `StoragePoolInnerV1` `prost_types::Value` (i.e. the
/// dynamic-field's `value` substructure) for `pool_id` so callers can
/// extract different sub-fields without re-issuing the gRPC walk.
pub async fn read_storage_pool_inner_json(
    rpc_url: &str,
    pool_id: ObjectID,
) -> Result<Value, Box<dyn Error + Send + Sync>> {
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
        return Ok(inner.clone());
    }
    Err(format!("no StoragePoolInnerV1 dynamic field found on pool {pool_id}").into())
}

/// Look up the `PooledBlob` ObjectID for `blob_id` inside the
/// `blobs: ObjectTable<u256, PooledBlob>` of the pool, returning `None`
/// when no entry exists.
///
/// The match is exact: the dynamic-object field's `name.value` is a
/// BCS-encoded `u256` (32 little-endian bytes) and `walrus_core::BlobId`
/// is `#[repr(transparent)]` over `[u8; 32]`. Walrus's PTB builder
/// passes `blob_metadata.blob_id` straight into `pt_builder.pure(...)`
/// (see `walrus-sui/.../transaction_builder/pooled_blob_ops.rs`), so
/// the wire bytes match exactly.
///
/// This is a recovery-path helper; worst-case it walks every blob in
/// the pool (paginated 50 at a time, matching the page size used by
/// [`read_storage_pool_state`]).
pub async fn lookup_pooled_blob_object_id(
    rpc_url: &str,
    pool_id: ObjectID,
    blob_id: &walrus_core::BlobId,
) -> Result<Option<ObjectID>, Box<dyn Error + Send + Sync>> {
    let inner = read_storage_pool_inner_json(rpc_url, pool_id).await?;
    let blobs_table_id =
        parse_blobs_object_table_id(&inner).ok_or_else(|| -> Box<dyn Error + Send + Sync> {
            format!(
                "could not extract blobs ObjectTable UID from StoragePoolInnerV1 for pool {pool_id}"
            )
            .into()
        })?;

    let client = SuiGrpcClient::new(rpc_url)?;
    let request = ListDynamicFieldsRequest::default()
        .with_parent(blobs_table_id.to_string())
        .with_page_size(50)
        .with_read_mask(FieldMask {
            paths: vec![
                "kind".to_string(),
                "name".to_string(),
                "child_id".to_string(),
            ],
        });
    let stream = client.list_dynamic_fields(request);
    futures::pin_mut!(stream);
    while let Some(field) = stream.try_next().await? {
        // ObjectTable stores entries as dynamic *object* fields, so
        // `child_id` is populated (not `value`).
        if field.kind() != DynamicFieldKind::Object {
            continue;
        }
        let Some(name_bcs) = field.name.as_ref().and_then(|b| b.value.as_ref()) else {
            continue;
        };
        if name_bcs.as_ref() != blob_id.0.as_slice() {
            continue;
        }
        let Some(child_id_str) = field.child_id.as_deref() else {
            continue;
        };
        let parsed: ObjectID = child_id_str
            .parse()
            .map_err(|e| format!("malformed child_id {child_id_str}: {e}"))?;
        return Ok(Some(parsed));
    }
    Ok(None)
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

/// Pure parser for `StoragePoolInnerV1.blobs.id` (the `UID` of the
/// `ObjectTable<u256, PooledBlob>` collection) rendered as JSON.
///
/// Sui's gRPC JSON encoder flattens `UID` to its underlying address
/// string, so the path is `blobs.id` rather than `blobs.id.id.bytes`.
/// Returns `None` when the path is missing or the address string can't
/// be parsed as an `ObjectID`. Exposed for unit testing.
pub fn parse_blobs_object_table_id(value: &Value) -> Option<ObjectID> {
    let outer = unwrap_struct(value)?;
    let blobs = unwrap_struct(outer.fields.get("blobs")?)?;
    let id = blobs.fields.get("id")?;
    match id.kind.as_ref()? {
        prost_types::value::Kind::StringValue(s) => s.parse().ok(),
        _ => None,
    }
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

    /// Pad a hex literal out to a full 64-char Sui address.
    fn obj_id(short: &str) -> String {
        let trimmed = short.trim_start_matches("0x");
        format!("0x{:0>64}", trimmed)
    }

    #[test]
    fn parse_blobs_object_table_id_extracts_uid() {
        let table_uid = obj_id("0xabc123");
        let inner = struct_value(&[
            (
                "storage",
                struct_value(&[("storage_size", string_value("1024"))]),
            ),
            ("used_encoded_bytes", string_value("0")),
            (
                "blobs",
                struct_value(&[
                    // Sui's JSON encoder flattens `UID` to its inner
                    // address string, so the path is `blobs.id` (not
                    // `blobs.id.id.bytes`).
                    ("id", string_value(&table_uid)),
                    ("size", string_value("0")),
                ]),
            ),
        ]);
        let parsed = parse_blobs_object_table_id(&inner).expect("must parse");
        let expected: ObjectID = table_uid.parse().expect("valid object id");
        assert_eq!(parsed, expected);
    }

    #[test]
    fn parse_blobs_object_table_id_missing_returns_none() {
        let inner = struct_value(&[
            (
                "storage",
                struct_value(&[("storage_size", string_value("1024"))]),
            ),
            ("used_encoded_bytes", string_value("0")),
        ]);
        assert!(parse_blobs_object_table_id(&inner).is_none());
    }

    #[test]
    fn parse_blobs_object_table_id_bad_uid_string_returns_none() {
        let inner = struct_value(&[(
            "blobs",
            struct_value(&[
                ("id", string_value("not-an-id")),
                ("size", string_value("0")),
            ]),
        )]);
        assert!(parse_blobs_object_table_id(&inner).is_none());
    }
}
