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

use crate::engine::audit::{self, AuditEvent};
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
            self.forget(tenant, id, "storing failed").await.ok();
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

    /// Brings file `id`'s deadline forward to `at`. A later `at` changes
    /// nothing: a deadline only ever moves earlier.
    pub async fn delete_no_later_than(
        &self,
        tenant: TenantId,
        id: FileId,
        at: OffsetDateTime,
    ) -> Result<(), FileError> {
        let updated = sqlx::query!(
            "update stored_files set delete_by = least(delete_by, $3)
             where id = $1 and tenant_id = $2 and deleted_at is null",
            id.as_uuid(),
            tenant.as_uuid(),
            at,
        )
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(FileError::NotFound);
        }
        Ok(())
    }

    /// Deletes every file past its deadline; returns how many. This is a
    /// system-wide worker: it sweeps every tenant's files, and names the
    /// file's tenant in every write.
    pub async fn sweep(&self) -> Result<usize, FileError> {
        let due = sqlx::query!(
            "select id, tenant_id from stored_files
             where deleted_at is null and delete_by <= $1
             order by delete_by limit 500",
            self.clock.now(),
        )
        .fetch_all(&self.pool)
        .await?;
        let mut deleted = 0;
        for row in due {
            match self.forget(row.tenant_id.into(), row.id.into(), "deadline").await {
                Ok(true) => deleted += 1,
                Ok(false) => {}
                // One file that can't be deleted mustn't hold up the others;
                // it's tried again at the next sweep.
                Err(err) => tracing::error!(tenant = %row.tenant_id, file = %row.id, %err, "deleting a file failed"),
            }
        }
        Ok(deleted)
    }

    /// Removes the stored file, then its key, and records why; false if it
    /// was already gone. Safe to repeat after a crash between the two.
    async fn forget(&self, tenant: TenantId, id: FileId, reason: &str) -> Result<bool, FileError> {
        self.store.delete(tenant, id).await?;
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query!(
            "update stored_files set deleted_at = $3, wrapped_key = null
             where id = $1 and tenant_id = $2 and deleted_at is null",
            id.as_uuid(),
            tenant.as_uuid(),
            now,
        )
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Ok(false);
        }
        audit::record(
            &mut *tx,
            &AuditEvent {
                tenant,
                at: now,
                actor: audit::SYSTEM,
                action: "file.deleted",
                subject_type: "stored_file",
                subject_id: id.as_uuid(),
                details: serde_json::json!({ "reason": reason }),
            },
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}
