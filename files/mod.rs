//! Encrypted file storage (F3). A file's encrypted bytes live in a
//! [`FileStore`]; its row in `stored_files` holds its wrapped key and the
//! time by which it must be deleted.

mod cipher;
mod store;

use std::sync::Arc;

use sqlx::PgPool;
use time::OffsetDateTime;

pub use cipher::{FileCipher, MasterKey, Sealed};
pub use store::{FileStore, LocalDisk};

use crate::engine::clock::Clock;
use crate::engine::ids::{FileId, TenantId};

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
    /// No such file for this tenant, or it was deleted.
    #[error("file not found")]
    NotFound,
    /// The file store failed.
    #[error("file store error: {0}")]
    Store(#[from] std::io::Error),
    /// The database failed.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Encrypted files, kept until their deadline.
#[derive(Clone)]
pub struct Files {
    pool: PgPool,
    clock: Arc<dyn Clock>,
    store: Arc<dyn FileStore>,
    cipher: Arc<FileCipher>,
}

impl Files {
    /// Files kept in `store`, encrypted by `cipher`.
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>, store: Arc<dyn FileStore>, cipher: Arc<FileCipher>) -> Self {
        Self {
            pool,
            clock,
            store,
            cipher,
        }
    }

    /// Encrypts and keeps `plain` for `tenant` until `delete_by` at the latest.
    pub async fn save(&self, tenant: TenantId, plain: &[u8], delete_by: OffsetDateTime) -> Result<FileId, FileError> {
        let id = FileId::new();
        let sealed = self.cipher.seal(tenant, id, plain)?;
        let size = i32::try_from(plain.len()).map_err(|_| FileError::Store(std::io::ErrorKind::FileTooLarge.into()))?;
        // The row goes first, so every stored file has a deadline that the
        // sweep will act on, even if we crash right after storing it.
        sqlx::query!(
            "insert into stored_files (id, tenant_id, size_bytes, wrapped_key, created_at, delete_by)
             values ($1, $2, $3, $4, $5, $6)",
            id.as_uuid(),
            tenant.as_uuid(),
            size,
            sealed.wrapped_key,
            self.clock.now(),
            delete_by,
        )
        .execute(&self.pool)
        .await?;
        if let Err(err) = self.store.put(tenant, id, &sealed.ciphertext).await {
            self.forget(tenant, id).await.ok();
            return Err(err.into());
        }
        Ok(id)
    }

    /// The decrypted contents of file `id` of `tenant`.
    pub async fn read(&self, tenant: TenantId, id: FileId) -> Result<Vec<u8>, FileError> {
        let wrapped_key = sqlx::query_scalar!(
            "select wrapped_key from stored_files where id = $1 and tenant_id = $2 and deleted_at is null",
            id.as_uuid(),
            tenant.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await?
        .flatten()
        .ok_or(FileError::NotFound)?;
        let ciphertext = match self.store.get(tenant, id).await {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(FileError::NotFound),
            other => other?,
        };
        self.cipher.open(
            tenant,
            id,
            &Sealed {
                ciphertext,
                wrapped_key,
            },
        )
    }

    /// Removes the stored file, then its key.
    async fn forget(&self, tenant: TenantId, id: FileId) -> Result<(), FileError> {
        self.store.delete(tenant, id).await?;
        sqlx::query!(
            "update stored_files set deleted_at = $3, wrapped_key = null
             where id = $1 and tenant_id = $2 and deleted_at is null",
            id.as_uuid(),
            tenant.as_uuid(),
            self.clock.now(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
