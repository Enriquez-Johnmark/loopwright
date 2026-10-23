//! Seam 1 and 4 (M4): encrypted files kept in a file store, and deleted at
//! their deadline (F3).

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::clock;
use loopwright::clock::FixedClock;
use loopwright::files::{FileCipher, FileError, FileStore, Files, LocalDisk, MasterKey};
use loopwright::ids::{FileId, TenantId};
use loopwright::outbox::BoxFuture;
use sqlx::PgPool;
use time::Duration;

fn files(pool: &PgPool, clock: Arc<FixedClock>) -> (Files, PathBuf) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(uuid::Uuid::now_v7().to_string());
    let cipher = FileCipher::new(&MasterKey::from_hex(&"ab".repeat(32)).unwrap());
    let files = Files::new(pool.clone(), clock, Arc::new(LocalDisk::new(&root)), Arc::new(cipher));
    (files, root)
}

fn on_disk(root: &PathBuf) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    for tenant in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(tenant.path()).unwrap().flatten() {
            found.push(std::fs::read(file.path()).unwrap());
        }
    }
    found
}

#[sqlx::test]
async fn f3_a_saved_file_is_encrypted_on_disk_and_reads_back_for_its_firm_only(pool: PgPool) {
    let clock = clock();
    let (files, root) = files(&pool, clock.clone());
    let (santos, reyes) = (TenantId::new(), TenantId::new());
    let photo = b"JPEG of a September sales invoice".to_vec();

    let id = files
        .save(santos, &photo, clock.now_plus(Duration::days(90)))
        .await
        .unwrap();

    assert_eq!(files.read(santos, id).await.unwrap(), photo);
    assert!(matches!(files.read(reyes, id).await, Err(FileError::NotFound)));
    let stored = on_disk(&root);
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].windows(photo.len()).any(|w| w == photo.as_slice()));
}

#[sqlx::test]
async fn f3_a_file_is_deleted_at_its_deadline_and_the_deletion_audited(pool: PgPool) {
    let clock = clock();
    let (files, root) = files(&pool, clock.clone());
    let santos = TenantId::new();
    let due = files
        .save(santos, b"receipt", clock.now_plus(Duration::days(90)))
        .await
        .unwrap();
    let kept = files
        .save(santos, b"statement", clock.now_plus(Duration::days(91)))
        .await
        .unwrap();

    clock.advance(Duration::days(90) - Duration::minutes(1));
    assert_eq!(files.sweep().await.unwrap(), 0);
    clock.advance(Duration::minutes(1));
    assert_eq!(files.sweep().await.unwrap(), 1);
    assert_eq!(files.sweep().await.unwrap(), 0, "a deleted file is deleted once");

    assert!(matches!(files.read(santos, due).await, Err(FileError::NotFound)));
    files.read(santos, kept).await.unwrap();
    assert_eq!(on_disk(&root).len(), 1, "the file itself is gone from disk");
    let key: Option<Vec<u8>> = sqlx::query_scalar("select wrapped_key from stored_files where id = $1")
        .bind(due.as_uuid())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(key.is_none(), "and so is its key");
    let (action, actor): (String, String) =
        sqlx::query_as("select action, actor from audit_events where subject_id = $1 and tenant_id = $2")
            .bind(due.as_uuid())
            .bind(santos.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((action.as_str(), actor.as_str()), ("file.deleted", "system"));
}

#[sqlx::test]
async fn f3_a_deadline_only_ever_moves_earlier(pool: PgPool) {
    let clock = clock();
    let (files, _root) = files(&pool, clock.clone());
    let (santos, reyes) = (TenantId::new(), TenantId::new());
    let id = files
        .save(santos, b"2307", clock.now_plus(Duration::days(90)))
        .await
        .unwrap();

    files
        .delete_no_later_than(santos, id, clock.now_plus(Duration::days(30)))
        .await
        .unwrap();
    files
        .delete_no_later_than(santos, id, clock.now_plus(Duration::days(60)))
        .await
        .unwrap();
    let foreign = files
        .delete_no_later_than(reyes, id, clock.now_plus(Duration::days(1)))
        .await;

    assert!(matches!(foreign, Err(FileError::NotFound)), "{foreign:?}");
    clock.advance(Duration::days(30));
    assert_eq!(files.sweep().await.unwrap(), 1);
}

/// A file store where one file can't be deleted, as with a permissions error.
struct StuckFile {
    disk: LocalDisk,
    stuck: Arc<Mutex<Option<FileId>>>,
}

impl FileStore for StuckFile {
    fn put<'a>(&'a self, tenant: TenantId, id: FileId, bytes: &'a [u8]) -> BoxFuture<'a, io::Result<()>> {
        self.disk.put(tenant, id, bytes)
    }

    fn get(&self, tenant: TenantId, id: FileId) -> BoxFuture<'_, io::Result<Vec<u8>>> {
        self.disk.get(tenant, id)
    }

    fn delete(&self, tenant: TenantId, id: FileId) -> BoxFuture<'_, io::Result<()>> {
        if *self.stuck.lock().unwrap() == Some(id) {
            return Box::pin(async { Err(io::Error::other("permission denied")) });
        }
        self.disk.delete(tenant, id)
    }
}

#[sqlx::test]
async fn f3_a_file_that_cant_be_deleted_doesnt_hold_up_the_others(pool: PgPool) {
    let clock = clock();
    let stuck = Arc::new(Mutex::new(None));
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(uuid::Uuid::now_v7().to_string());
    let store = StuckFile {
        disk: LocalDisk::new(&root),
        stuck: stuck.clone(),
    };
    let cipher = FileCipher::new(&MasterKey::from_hex(&"ab".repeat(32)).unwrap());
    let files = Files::new(pool.clone(), clock.clone(), Arc::new(store), Arc::new(cipher));
    let tenant = TenantId::new();
    let first = files
        .save(tenant, b"receipt", clock.now_plus(Duration::days(1)))
        .await
        .unwrap();
    let second = files
        .save(tenant, b"statement", clock.now_plus(Duration::days(2)))
        .await
        .unwrap();
    *stuck.lock().unwrap() = Some(first);

    clock.advance(Duration::days(3));
    assert_eq!(files.sweep().await.unwrap(), 1);

    assert!(matches!(files.read(tenant, second).await, Err(FileError::NotFound)));
    files.read(tenant, first).await.unwrap();
    *stuck.lock().unwrap() = None;
    assert_eq!(files.sweep().await.unwrap(), 1, "tried again at the next sweep");
}

#[sqlx::test]
async fn f3_a_discarded_file_is_deleted_at_once_and_audited(pool: PgPool) {
    let clock = clock();
    let (files, root) = files(&pool, clock.clone());
    let santos = TenantId::new();
    let id = files
        .save(santos, b"receipt", clock.now_plus(Duration::days(90)))
        .await
        .unwrap();

    files.discard(santos, id).await.unwrap();

    assert!(matches!(files.read(santos, id).await, Err(FileError::NotFound)));
    assert!(on_disk(&root).is_empty());
    let reason: serde_json::Value =
        sqlx::query_scalar("select details -> 'reason' from audit_events where subject_id = $1")
            .bind(id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reason, "discarded");
}
