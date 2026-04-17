// error.rs — Sentrix error types shared across all crates.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SentrixError {
    // Blockchain errors
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),
    #[error("Insufficient balance: have {have}, need {need}")]
    InsufficientBalance { have: u64, need: u64 },
    #[error("Invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("Chain validation failed: {0}")]
    ChainValidationFailed(String),
    #[error("Block not found: {0}")]
    BlockNotFound(String),
    #[error("Transaction not found: {0}")]
    TransactionNotFound(String),

    // Consensus errors
    #[error("Unauthorized validator: {0}")]
    UnauthorizedValidator(String),
    #[error("Not your turn to produce block")]
    NotYourTurn,
    #[error("No active validators")]
    NoActiveValidators,

    // Wallet errors
    #[error("Invalid private key")]
    InvalidPrivateKey,
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Keystore error: {0}")]
    KeystoreError(String),
    #[error("Wrong password")]
    WrongPassword,

    // Storage errors
    #[error("Storage error: {0}")]
    StorageError(String),
    #[error("Serialization error: {0}")]
    SerializationError(String),

    // Network errors
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    // General
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Internal error: {0}")]
    Internal(String),
}

pub type SentrixResult<T> = Result<T, SentrixError>;

impl From<serde_json::Error> for SentrixError {
    fn from(e: serde_json::Error) -> Self {
        SentrixError::SerializationError(e.to_string())
    }
}

impl From<std::io::Error> for SentrixError {
    fn from(e: std::io::Error) -> Self {
        SentrixError::StorageError(e.to_string())
    }
}
