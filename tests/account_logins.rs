//! Login by one-time email link, and sessions.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::sync::Arc;

use common::clock;
use nudgewright::accounts::{
    Accounts, LINK_LIFETIME, LOGIN_APPROVER, LoginLink, LoginOutcome, Logins, MAX_LINKS_PER_HOUR, Members, Role,
    SESSION_LIFETIME,
};
use nudgewright::clock::FixedClock;
use nudgewright::ids::TenantId;
use nudgewright::mail::Email;
use nudgewright::outbox::NoExtraChecks;
use sqlx::PgPool;
use time::Duration;

async fn account(pool: &PgPool, clock: &Arc<FixedClock>) -> TenantId {
    Accounts::new(pool.clone(), clock.clone())
        .create("Reyes Creative", "ana@reyes.test")
        .await
        .unwrap()
}

/// The product's wording: the token in a link on its own domain.
fn compose(link: &LoginLink<'_>) -> Email {
    Email {
        to_address: link.to_address.to_owned(),
        from_name: "Product".to_owned(),
        reply_to: None,
        subject: format!("Log in to {}", link.account_name),
        body_text: format!("https://app.product.test/login/{}", link.token),
    }
}

/// Sends a login link to `email` and returns the token from the email.
async fn token_for(logins: &Logins, pool: &PgPool, email: &str) -> String {
    assert_eq!(
        logins.send_link(email, compose, &NoExtraChecks).await.unwrap(),
        LoginOutcome::Sent
    );
    let body = sqlx::query_scalar::<_, String>(
        "select body_text from outbox_messages where lower(to_address) = lower($1) order by created_at desc limit 1",
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .unwrap();
    body.rsplit('/').next().unwrap().to_owned()
}

#[sqlx::test]
async fn send_link_approves_an_email_to_the_member(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock);

    logins
        .send_link("ANA@reyes.test", compose, &NoExtraChecks)
        .await
        .unwrap();

    let row = sqlx::query_as::<_, (String, String, Option<String>)>(
        "select to_address, status, approved_by from outbox_messages",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "ana@reyes.test".to_owned(),
            "approved".to_owned(),
            Some(LOGIN_APPROVER.to_owned())
        )
    );
}

#[sqlx::test]
async fn send_link_to_an_unknown_address_sends_nothing(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock);

    let outcome = logins
        .send_link("stranger@else.test", compose, &NoExtraChecks)
        .await
        .unwrap();

    let count = sqlx::query_scalar::<_, i64>("select count(*) from outbox_messages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((outcome, count), (LoginOutcome::NotSent, 0));
}

#[sqlx::test]
async fn the_link_always_goes_to_the_member_whatever_the_product_writes(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock);

    logins
        .send_link(
            "ana@reyes.test",
            |link| Email {
                to_address: "attacker@else.test".to_owned(),
                ..compose(link)
            },
            &NoExtraChecks,
        )
        .await
        .unwrap();

    let to = sqlx::query_scalar::<_, String>("select to_address from outbox_messages")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(to, "ana@reyes.test");
}

#[sqlx::test]
async fn send_link_stops_at_the_hourly_limit_per_address(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock);
    for _ in 0..MAX_LINKS_PER_HOUR {
        logins
            .send_link("ana@reyes.test", compose, &NoExtraChecks)
            .await
            .unwrap();
    }

    let outcome = logins
        .send_link("ana@reyes.test", compose, &NoExtraChecks)
        .await
        .unwrap();

    assert_eq!(outcome, LoginOutcome::NotSent);
}

#[sqlx::test]
async fn a_link_starts_a_session_for_its_member(pool: PgPool) {
    let clock = clock();
    let tenant = account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock);
    let token = token_for(&logins, &pool, "ana@reyes.test").await;

    let secret = logins.log_in(&token).await.unwrap().unwrap();

    let session = logins.session(&secret).await.unwrap().unwrap();
    assert_eq!(
        (session.tenant, session.email.as_str(), session.role),
        (tenant, "ana@reyes.test", Role::Owner)
    );
}

#[sqlx::test]
async fn a_link_works_only_once(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock);
    let token = token_for(&logins, &pool, "ana@reyes.test").await;
    logins.log_in(&token).await.unwrap().unwrap();

    let second = logins.log_in(&token).await.unwrap();

    assert_eq!(second, None);
}

#[sqlx::test]
async fn a_link_expires(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock.clone());
    let token = token_for(&logins, &pool, "ana@reyes.test").await;
    clock.advance(LINK_LIFETIME + Duration::seconds(1));

    let late = logins.log_in(&token).await.unwrap();

    assert_eq!(late, None);
}

#[sqlx::test]
async fn a_session_expires(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock.clone());
    let token = token_for(&logins, &pool, "ana@reyes.test").await;
    let secret = logins.log_in(&token).await.unwrap().unwrap();
    clock.advance(SESSION_LIFETIME + Duration::seconds(1));

    let session = logins.session(&secret).await.unwrap();

    assert_eq!(session, None);
}

#[sqlx::test]
async fn log_out_ends_the_session(pool: PgPool) {
    let clock = clock();
    account(&pool, &clock).await;
    let logins = Logins::new(pool.clone(), clock);
    let token = token_for(&logins, &pool, "ana@reyes.test").await;
    let secret = logins.log_in(&token).await.unwrap().unwrap();

    logins.log_out(&secret).await.unwrap();

    assert_eq!(logins.session(&secret).await.unwrap(), None);
}

#[sqlx::test]
async fn removing_a_staff_member_ends_their_session(pool: PgPool) {
    let clock = clock();
    let tenant = account(&pool, &clock).await;
    let members = Members::new(pool.clone(), clock.clone());
    let ben = members
        .add_staff(tenant, "ben@reyes.test", "ana@reyes.test")
        .await
        .unwrap();
    let logins = Logins::new(pool.clone(), clock);
    let token = token_for(&logins, &pool, "ben@reyes.test").await;
    let secret = logins.log_in(&token).await.unwrap().unwrap();

    members.remove_staff(tenant, ben.id, "ana@reyes.test").await.unwrap();

    assert_eq!(logins.session(&secret).await.unwrap(), None);
}
