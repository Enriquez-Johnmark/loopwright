//! Where encrypted files are kept. Local disk for now; Cloudflare R2 at M6
//! (`docs/architecture.md`, Hosting).

use std::io;
use std::path::PathBuf;

use tokio::io::AsyncWriteExt;

use crate::engine::ids::{FileId, TenantId};
use crate::engine::outbox::BoxFuture;

/// Somewhere to keep encrypted files, by tenant and file ID.
pub trait FileStore: Send + Sync {
    /// Keeps `bytes` as file `id` of `tenant`.
    fn put<'a>(&'a self, tenant: TenantId, id: FileId, bytes: &'a [u8]) -> BoxFuture<'a, io::Result<()>>;
    /// The bytes kept as file `id` of `tenant`.
    fn get(&self, tenant: TenantId, id: FileId) -> BoxFuture<'_, io::Result<Vec<u8>>>;
    /// Removes file `id` of `tenant`; removing a file that isn't there succeeds.
    fn delete(&self, tenant: TenantId, id: FileId) -> BoxFuture<'_, io::Result<()>>;
}

/// Files in a folder on the server's disk, one folder per tenant.
#[derive(Debug, Clone)]
pub struct LocalDisk {
    root: PathBuf,
}

impl LocalDisk {
    /// Files kept under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Only typed IDs make up the path, so no input can climb out of `root`.
    fn path(&self, tenant: TenantId, id: FileId) -> PathBuf {
        self.root.join(tenant.to_string()).join(id.to_string())
    }
}

impl FileStore for LocalDisk {
    fn put<'a>(&'a self, tenant: TenantId, id: FileId, bytes: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            let path = self.path(tenant, id);
            let partial = path.with_extension("part");
            if let Some(folder) = path.parent() {
                tokio::fs::create_dir_all(folder).await?;
            }
            // Written whole and flushed to disk, then renamed and the rename
            // flushed, so a crash never leaves half a file under the real name.
            let mut file = tokio::fs::File::create(&partial).await?;
            file.write_all(bytes).await?;
            file.sync_all().await?;
            tokio::fs::rename(&partial, &path).await?;
            if let Some(folder) = path.parent() {
                tokio::fs::File::open(folder).await?.sync_all().await?;
            }
            Ok(())
        })
    }

    fn get(&self, tenant: TenantId, id: FileId) -> BoxFuture<'_, io::Result<Vec<u8>>> {
        Box::pin(tokio::fs::read(self.path(tenant, id)))
    }

    fn delete(&self, tenant: TenantId, id: FileId) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            match tokio::fs::remove_file(self.path(tenant, id)).await {
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        })
    }
}
