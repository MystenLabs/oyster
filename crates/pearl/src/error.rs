#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("account not found")]
    AccountNotFound,

    #[error("invalid credentials")]
    InvalidCredentials,

    #[error("invalid private key: {0}")]
    InvalidPrivateKey(String),

    #[error("invalid transaction data: {0}")]
    InvalidTransactionData(String),

    #[error("signing error: {0}")]
    SigningError(String),

    #[error("pending transaction not found")]
    PendingTransactionNotFound,

    #[error("pending transaction already resolved")]
    PendingTransactionAlreadyResolved,

    #[error("sui rpc error: {0}")]
    SuiRpc(String),
}
