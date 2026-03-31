//! Strongly-typed app identifier backed by a UUID.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A strongly-typed Oyster app identifier (UUID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(transparent)]
pub struct AppId(Uuid);

impl AppId {
    /// The well-known "internal" app ID (nil UUID).
    pub const INTERNAL: Self = Self(Uuid::nil());

    /// Generate a new random app ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AppId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for AppId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

impl sqlx::Type<sqlx::Any> for AppId {
    fn type_info() -> sqlx::any::AnyTypeInfo {
        <String as sqlx::Type<sqlx::Any>>::type_info()
    }

    fn compatible(ty: &sqlx::any::AnyTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Any>>::compatible(ty)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Any> for &AppId {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Any as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let s = self.to_string();
        <String as sqlx::Encode<'q, sqlx::Any>>::encode(s, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Any> for AppId {
    fn decode(
        value: <sqlx::Any as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as sqlx::Decode<'r, sqlx::Any>>::decode(value)?;
        Ok(s.parse()?)
    }
}
