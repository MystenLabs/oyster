//! Pearl — a custodial key management and signing service for Sui.

/// gRPC authentication interceptor.
pub mod auth;
/// Service configuration loaded from environment variables.
pub mod config;
/// Database access layer (SQLite via sqlx).
pub mod db;
/// Deterministic key derivation via HKDF-SHA256.
pub mod derivation;
/// Service error types.
pub mod error;
/// gRPC service implementation.
pub mod grpc;
/// Data models for database rows.
pub mod models;
/// Transaction signing using derived Ed25519 keys.
pub mod signing;
