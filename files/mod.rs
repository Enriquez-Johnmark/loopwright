//! Encrypted file storage (F3).

mod cipher;

pub use cipher::{FileCipher, MasterKey, Sealed};

/// Problems with keys or stored files.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    /// The master key isn't 64 hex digits.
    #[error("the master key must be 64 hex digits (`kompleto gen-key` makes one)")]
    BadMasterKey,
    /// The file or its key was changed, swapped or belongs elsewhere.
    #[error("the file can't be decrypted")]
    Undecryptable,
    /// The system's random number source failed.
    #[error("no randomness available: {0}")]
    Random(String),
}
