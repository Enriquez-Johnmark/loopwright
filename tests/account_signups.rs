//! Requests for an account from a public sign-up page, approved by the
//! operator.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use common::clock;
use nudgewright::accounts::{Accounts, EXPIRES_AFTER_DAYS, MAX_PER_HOUR, SignupError, Signups};
use sqlx::PgPool;
use time::Duration;

#[sqlx::test]
async fn approving_a_request_creates_the_account(pool: PgPool) {
    let clock = clock();
    let signups = Signups::new(pool.clone(), clock.clone());
    signups.request("Reyes Creative", "ana@reyes.test", true).await.unwrap();
    let request = signups.pending().await.unwrap().remove(0);

    let (tenant, _) = signups.approve(request.id).await.unwrap();

    let account = Accounts::new(pool, clock).get(tenant).await.unwrap();
    assert_eq!(
        (account.name.as_str(), account.owner_email.as_str()),
        ("Reyes Creative", "ana@reyes.test")
    );
}

#[sqlx::test]
async fn an_approved_request_leaves_the_queue(pool: PgPool) {
    let signups = Signups::new(pool, clock());
    signups.request("Reyes Creative", "ana@reyes.test", true).await.unwrap();
    let request = signups.pending().await.unwrap().remove(0);

    signups.approve(request.id).await.unwrap();

    assert!(signups.pending().await.unwrap().is_empty());
}

#[sqlx::test]
async fn a_request_lists_every_problem(pool: PgPool) {
    let signups = Signups::new(pool, clock());

    let err = signups.request("", "not an address", false).await.unwrap_err();

    let SignupError::Invalid(problems) = err else {
        panic!("expected Invalid, got {err:?}");
    };
    assert_eq!(problems.len(), 3, "got {problems:?}");
}

#[sqlx::test]
async fn a_request_from_an_address_that_logs_in_is_answered_but_not_kept(pool: PgPool) {
    let clock = clock();
    Accounts::new(pool.clone(), clock.clone())
        .create("Reyes Creative", "ana@reyes.test")
        .await
        .unwrap();
    let signups = Signups::new(pool, clock);

    signups.request("Again", "ANA@reyes.test", true).await.unwrap();

    assert!(signups.pending().await.unwrap().is_empty());
}

#[sqlx::test]
async fn a_second_request_from_the_same_address_keeps_one(pool: PgPool) {
    let signups = Signups::new(pool, clock());
    signups.request("Reyes Creative", "ana@reyes.test", true).await.unwrap();

    signups.request("Reyes Studio", "ana@reyes.test", true).await.unwrap();

    assert_eq!(signups.pending().await.unwrap().len(), 1);
}

#[sqlx::test]
async fn requests_stop_at_the_hourly_limit(pool: PgPool) {
    let signups = Signups::new(pool, clock());
    for n in 0..MAX_PER_HOUR {
        signups
            .request("Studio", &format!("owner{n}@studio.test"), true)
            .await
            .unwrap();
    }

    let err = signups.request("Studio", "late@studio.test", true).await.unwrap_err();

    assert!(matches!(err, SignupError::TooMany), "got {err:?}");
}

#[sqlx::test]
async fn old_requests_expire(pool: PgPool) {
    let clock = clock();
    let signups = Signups::new(pool, clock.clone());
    signups.request("Reyes Creative", "ana@reyes.test", true).await.unwrap();
    clock.advance(Duration::days(EXPIRES_AFTER_DAYS) + Duration::seconds(1));

    signups.request("Other Studio", "cara@other.test", true).await.unwrap();

    let emails: Vec<_> = signups.pending().await.unwrap().into_iter().map(|r| r.email).collect();
    assert_eq!(emails, ["cara@other.test"]);
}

#[sqlx::test]
async fn declining_a_request_removes_it(pool: PgPool) {
    let signups = Signups::new(pool, clock());
    signups.request("Reyes Creative", "ana@reyes.test", true).await.unwrap();
    let request = signups.pending().await.unwrap().remove(0);

    signups.decline(request.id).await.unwrap();

    assert!(signups.pending().await.unwrap().is_empty());
}

#[sqlx::test]
async fn a_request_from_the_owner_of_a_closed_account_is_answered_but_not_kept(pool: PgPool) {
    let clock = clock();
    let accounts = Accounts::new(pool.clone(), clock.clone());
    let tenant = accounts.create("Reyes Creative", "ana@reyes.test").await.unwrap();
    accounts
        .close(tenant, "Reyes Creative", "ana@reyes.test")
        .await
        .unwrap();
    let signups = Signups::new(pool, clock);

    signups.request("Reyes Again", "ana@reyes.test", true).await.unwrap();

    assert!(signups.pending().await.unwrap().is_empty());
}
