// error.rs — Storage error types

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("MDBX error: {0}")]
    Mdbx(String),

    #[error("Encoding error: {0}")]
    Encode(String),

    #[error("Decoding error: {0}")]
    Decode(String),

    #[error("Key not found: {table}/{key}")]
    NotFound { table: String, key: String },

    #[error("Table not found: {0}")]
    TableNotFound(String),

    #[error("Storage error: {0}")]
    Other(String),
}

impl From<libmdbx::Error> for StorageError {
    fn from(e: libmdbx::Error) -> Self {
        StorageError::Mdbx(e.to_string())
    }
}

impl From<bincode::Error> for StorageError {
    fn from(e: bincode::Error) -> Self {
        StorageError::Encode(e.to_string())
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(e: serde_json::Error) -> Self {
        StorageError::Encode(e.to_string())
    }
}

impl From<StorageError> for sentrix_primitives::SentrixError {
    fn from(e: StorageError) -> Self {
        sentrix_primitives::SentrixError::StorageError(e.to_string())
    }
}

pub type StorageResult<T> = Result<T, StorageError>;
