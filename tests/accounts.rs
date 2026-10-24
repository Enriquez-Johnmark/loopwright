//! Accounts: creating one with its owner, and the rules on names and
//! addresses.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use common::clock;
use nudgewright::accounts::{AccountError, Accounts};
use sqlx::PgPool;

#[sqlx::test]
async fn create_makes_an_open_account_owned_by_its_login_address(pool: PgPool) {
    let accounts = Accounts::new(pool, clock());

    let tenant = accounts.create("Reyes Creative", "ana@reyes.test").await.unwrap();

    let account = accounts.get(tenant).await.unwrap();
    assert_eq!(
        (account.name.as_str(), account.owner_email.as_str(), account.closed_at),
        ("Reyes Creative", "ana@reyes.test", None)
    );
}

#[sqlx::test]
async fn create_refuses_an_address_that_already_logs_in(pool: PgPool) {
    let accounts = Accounts::new(pool, clock());
    accounts.create("Reyes Creative", "ana@reyes.test").await.unwrap();

    let err = accounts.create("Another", "ANA@reyes.test").await.unwrap_err();

    assert!(matches!(err, AccountError::EmailTaken), "got {err:?}");
}

#[sqlx::test]
async fn create_refuses_a_name_that_reads_as_a_link(pool: PgPool) {
    let accounts = Accounts::new(pool, clock());

    let err = accounts.create("reyes-pay.ph", "ana@reyes.test").await.unwrap_err();

    assert!(matches!(err, AccountError::Invalid(_)), "got {err:?}");
}

#[sqlx::test]
async fn create_refuses_something_that_is_not_an_email_address(pool: PgPool) {
    let accounts = Accounts::new(pool, clock());

    let err = accounts.create("Reyes Creative", "ana at reyes").await.unwrap_err();

    assert!(matches!(err, AccountError::Invalid(_)), "got {err:?}");
}

#[sqlx::test]
async fn get_of_an_unknown_tenant_is_not_found(pool: PgPool) {
    let accounts = Accounts::new(pool, clock());

    let err = accounts.get(nudgewright::ids::TenantId::new()).await.unwrap_err();

    assert!(matches!(err, AccountError::NotFound), "got {err:?}");
}

#[sqlx::test]
async fn create_refuses_a_list_of_addresses(pool: PgPool) {
    let accounts = Accounts::new(pool, clock());

    let err = accounts
        .create("Reyes Creative", "ana@reyes.test,spy@else.test")
        .await
        .unwrap_err();

    assert!(matches!(err, AccountError::Invalid(_)), "got {err:?}");
}

#[test]
fn an_address_has_exactly_one_at_sign_with_text_on_both_sides() {
    let accepted: Vec<bool> = ["ana@reyes.test", "@reyes.test", "ana@", "a@b@c", "Ana <ana@reyes.test>"]
        .into_iter()
        .map(nudgewright::accounts::is_email_address)
        .collect();

    assert_eq!(accepted, [true, false, false, false, false]);
}
