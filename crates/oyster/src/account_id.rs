//! Strongly-typed account identifier backed by a UUID v4.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A strongly-typed Oyster account identifier (UUID v4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct AccountId(Uuid);

impl Default for AccountId {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountId {
    /// Generate a new random account ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Return the raw 16-byte UUID representation.
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for AccountId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

impl sqlx::Type<sqlx::Any> for AccountId {
    fn type_info() -> sqlx::any::AnyTypeInfo {
        <String as sqlx::Type<sqlx::Any>>::type_info()
    }

    fn compatible(ty: &sqlx::any::AnyTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Any>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Any> for &AccountId {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Any as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let s = self.to_string();
        <String as sqlx::Encode<'q, sqlx::Any>>::encode(s, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Any> for AccountId {
    fn decode(
        value: <sqlx::Any as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<'r, sqlx::Any>>::decode(value)?;
        Ok(s.parse()?)
    }
}
