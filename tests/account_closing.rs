//! Closing an account: at once it stops logins and waiting messages; after
//! a grace period everything it holds is erased, unless the operator
//! reopens it first.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{clock, sample_email};
use nudgewright::accounts::{Accounts, CloseError, GRACE_DAYS, LoginOutcome, Logins};
use nudgewright::clock::FixedClock;
use nudgewright::files::{FileCipher, Files, LocalDisk, MasterKey};
use nudgewright::ids::TenantId;
use nudgewright::mail::Email;
use nudgewright::outbox::{MessageStatus, NoExtraChecks, Outbox};
use sqlx::PgPool;
use time::Duration;

fn files(pool: &PgPool, clock: Arc<FixedClock>) -> Files {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(uuid::Uuid::now_v7().to_string());
    let cipher = FileCipher::new(&MasterKey::from_hex(&"ab".repeat(32)).unwrap());
    Files::new(pool.clone(), clock, Arc::new(LocalDisk::new(&root)), Arc::new(cipher))
}

fn compose(link: &nudgewright::accounts::LoginLink<'_>) -> Email {
    Email {
        to_address: link.to_address.to_owned(),
        from_name: "Product".to_owned(),
        reply_to: None,
        subject: "Log in".to_owned(),
        body_text: format!("https://app.product.test/login/{}", link.token),
    }
}

async fn account(accounts: &Accounts) -> TenantId {
    accounts.create("Reyes Creative", "ana@reyes.test").await.unwrap()
}

#[sqlx::test]
async fn close_with_the_wrong_name_changes_nothing(pool: PgPool) {
    let accounts = Accounts::new(pool, clock());
    let tenant = account(&accounts).await;

    let err = accounts.close(tenant, "Reyes", "ana@reyes.test").await.unwrap_err();

    assert!(matches!(err, CloseError::NameMismatch), "got {err:?}");
}

#[sqlx::test]
async fn closing_stops_login_links(pool: PgPool) {
    let clock = clock();
    let accounts = Accounts::new(pool.clone(), clock.clone());
    let tenant = account(&accounts).await;

    accounts
        .close(tenant, " reyes creative ", "ana@reyes.test")
        .await
        .unwrap();

    let outcome = Logins::new(pool, clock)
        .send_link("ana@reyes.test", compose, &NoExtraChecks)
        .await
        .unwrap();
    assert_eq!(outcome, LoginOutcome::NotSent);
}

#[sqlx::test]
async fn closing_cancels_waiting_messages(pool: PgPool) {
    let clock = clock();
    let accounts = Accounts::new(pool.clone(), clock.clone());
    let tenant = account(&accounts).await;
    let outbox = Outbox::new(pool, clock);
    let draft = outbox.create_draft(tenant, &sample_email()).await.unwrap();

    accounts
        .close(tenant, "Reyes Creative", "ana@reyes.test")
        .await
        .unwrap();

    assert_eq!(
        outbox.get(tenant, draft).await.unwrap().status,
        MessageStatus::Cancelled
    );
}

#[sqlx::test]
async fn reopening_within_the_grace_period_restores_logins(pool: PgPool) {
    let clock = clock();
    let accounts = Accounts::new(pool.clone(), clock.clone());
    let tenant = account(&accounts).await;
    accounts
        .close(tenant, "Reyes Creative", "ana@reyes.test")
        .await
        .unwrap();
    clock.advance(Duration::days(GRACE_DAYS - 1));

    let reopened = accounts.reopen("ANA@reyes.test").await.unwrap();

    assert!(reopened && accounts.get(tenant).await.unwrap().closed_at.is_none());
}

#[sqlx::test]
async fn nothing_is_erased_during_the_grace_period(pool: PgPool) {
    let clock = clock();
    let accounts = Accounts::new(pool.clone(), clock.clone());
    let tenant = account(&accounts).await;
    accounts
        .close(tenant, "Reyes Creative", "ana@reyes.test")
        .await
        .unwrap();
    clock.advance(Duration::days(GRACE_DAYS - 1));

    let erased = accounts.erase_due(&files(&pool, clock)).await.unwrap();

    assert_eq!((erased, accounts.get(tenant).await.is_ok()), (0, true));
}

#[sqlx::test]
async fn after_the_grace_period_the_account_and_its_messages_are_erased(pool: PgPool) {
    let clock = clock();
    let accounts = Accounts::new(pool.clone(), clock.clone());
    let tenant = account(&accounts).await;
    Outbox::new(pool.clone(), clock.clone())
        .create_draft(tenant, &sample_email())
        .await
        .unwrap();
    accounts
        .close(tenant, "Reyes Creative", "ana@reyes.test")
        .await
        .unwrap();
    clock.advance(Duration::days(GRACE_DAYS) + Duration::seconds(1));

    accounts.erase_due(&files(&pool, clock)).await.unwrap();

    let left = sqlx::query_scalar::<_, i64>(
        "select (select count(*) from accounts) + (select count(*) from account_members)
              + (select count(*) from outbox_messages where tenant_id = $1)",
    )
    .bind(tenant.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test]
async fn an_erased_account_cannot_be_reopened(pool: PgPool) {
    let clock = clock();
    let accounts = Accounts::new(pool.clone(), clock.clone());
    let tenant = account(&accounts).await;
    accounts
        .close(tenant, "Reyes Creative", "ana@reyes.test")
        .await
        .unwrap();
    clock.advance(Duration::days(GRACE_DAYS) + Duration::seconds(1));
    accounts.erase_due(&files(&pool, clock)).await.unwrap();

    assert!(!accounts.reopen("ana@reyes.test").await.unwrap());
}
