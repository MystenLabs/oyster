use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::error::AppError;

const DEFAULT_PAGE_SIZE: i64 = 20;
const MAX_PAGE_SIZE: i64 = 100;

/// Decoded cursor containing the position for keyset pagination.
#[derive(Debug)]
pub struct CursorData {
    /// The `created_at` value of the last item on the previous page.
    pub created_at: String,
    /// The `id` of the last item on the previous page.
    pub id: String,
}

/// Decode a base64url-encoded pagination cursor.
pub fn decode_cursor(cursor: &str) -> Result<CursorData, AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::BadRequest("invalid cursor".into()))?;
    let s = String::from_utf8(bytes).map_err(|_| AppError::BadRequest("invalid cursor".into()))?;
    let (created_at, id) = s
        .split_once('|')
        .ok_or_else(|| AppError::BadRequest("invalid cursor".into()))?;
    Ok(CursorData {
        created_at: created_at.to_string(),
        id: id.to_string(),
    })
}

/// Encode a pagination cursor from a `created_at` and `id` pair.
pub fn encode_cursor(created_at: &str, id: &str) -> String {
    let raw = format!("{created_at}|{id}");
    URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// Clamp an optional page size to `[1, 100]`, defaulting to 20.
pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE)
}
