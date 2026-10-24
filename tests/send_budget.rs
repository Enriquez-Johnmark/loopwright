//! Seam 1 (M6): the daily send budget under Resend Free's 100-a-day cap.
//! Messages with a deadline (Reminders) stop at the budget, nearest
//! deadline first; ones without (login links, notices) may use a small
//! reserve above it.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::sync::Arc;

use common::{RecordingMailer, clock, sample_email};
use nudgewright::clock::Clock;
use nudgewright::ids::TenantId;
use nudgewright::mail::Email;
use nudgewright::outbox::{NoExtraChecks, Outbox, SendBudget, Sender};
use sqlx::PgPool;
use time::Duration;

/// An approved message to `to`, with a deadline `days` from now if any.
async fn approved(
    pool: &PgPool,
    outbox: &Outbox,
    clock: &Arc<nudgewright::clock::FixedClock>,
    to: &str,
    days: Option<i64>,
) {
    let tenant = TenantId::new();
    let email = Email {
        to_address: to.to_owned(),
        ..sample_email()
    };
    let mut tx = pool.begin().await.unwrap();
    let expires = days.map(|days| clock.now() + Duration::days(days));
    let id = outbox.create_draft_in(&mut tx, tenant, &email, expires).await.unwrap();
    tx.commit().await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "a@firm.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
}

#[sqlx::test]
async fn send_budget_holds_reminders_past_it_nearest_deadline_first(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    for (to, days) in [("d5@x.test", 5), ("d2@x.test", 2), ("d9@x.test", 9), ("d1@x.test", 1)] {
        approved(&pool, &outbox, &clock, to, Some(days)).await;
    }
    approved(&pool, &outbox, &clock, "login1@x.test", None).await;
    let mailer = RecordingMailer::idempotent();
    let sender = Sender::new(pool.clone(), mailer.clone(), clock.clone(), Arc::new(NoExtraChecks))
        .with_budget(SendBudget { per_day: 3, reserve: 1 });
    let sent = |mailer: &RecordingMailer| -> Vec<String> {
        mailer.calls().into_iter().map(|(email, _)| email.to_address).collect()
    };

    sender.drain_once(50).await.unwrap();
    assert_eq!(sent(&mailer), ["login1@x.test", "d1@x.test", "d2@x.test"]);

    // The Reminders' budget is used up; a login link still has the reserve.
    approved(&pool, &outbox, &clock, "login2@x.test", None).await;
    approved(&pool, &outbox, &clock, "login3@x.test", None).await;
    sender.drain_once(50).await.unwrap();
    assert_eq!(sent(&mailer).len(), 4);
    assert_eq!(sent(&mailer)[3], "login2@x.test");

    clock.advance(Duration::hours(24));
    sender.drain_once(50).await.unwrap();
    assert_eq!(sent(&mailer)[4..], ["login3@x.test", "d5@x.test", "d9@x.test"]);
}

#[sqlx::test]
async fn a_reminder_held_past_its_deadline_expires_instead_of_going_out_late(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    approved(&pool, &outbox, &clock, "d1@x.test", Some(1)).await;
    approved(&pool, &outbox, &clock, "d2@x.test", Some(2)).await;
    approved(&pool, &outbox, &clock, "d3@x.test", Some(3)).await;
    let mailer = RecordingMailer::idempotent();
    let sender = Sender::new(pool.clone(), mailer.clone(), clock.clone(), Arc::new(NoExtraChecks))
        .with_budget(SendBudget { per_day: 1, reserve: 0 });
    sender.drain_once(50).await.unwrap();

    clock.advance(Duration::hours(50)); // d2's deadline has passed, d3's hasn't
    sender.drain_once(50).await.unwrap();
    outbox.expire_overdue().await.unwrap();

    let sent: Vec<String> = mailer.calls().into_iter().map(|(email, _)| email.to_address).collect();
    assert_eq!(sent, ["d1@x.test", "d3@x.test"]);
    let expired: Vec<String> = sqlx::query_scalar("select to_address from outbox_messages where status = 'expired'")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(expired, ["d2@x.test"]);
}
