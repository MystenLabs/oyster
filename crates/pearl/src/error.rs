/// Pearl service errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The derived private key is invalid.
    #[error("invalid private key: {0}")]
    InvalidPrivateKey(String),

    /// The provided transaction data could not be deserialized.
    #[error("invalid transaction data: {0}")]
    InvalidTransactionData(String),

    /// An error occurred during transaction signing.
    #[error("signing error: {0}")]
    SigningError(String),

    /// An error occurred during key derivation.
    #[error("key derivation error: {0}")]
    DerivationError(String),
}
