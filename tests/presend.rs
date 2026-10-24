//! Seam 3 (M3): pre-send checks (D7) at approval and at sending. The
//! engine's own rules apply to every message; a product adds its own
//! through `PreSendCheck`.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::sync::Arc;

use common::{RecordingMailer, clock, sample_email};
use nudgewright::ids::{MessageId, TenantId};
use nudgewright::mail::Email;
use nudgewright::outbox::{
    BoxFuture, CheckFailed, MessageStatus, NoExtraChecks, Outbox, OutboxError, PreSendCheck, Sender,
};
use sqlx::PgPool;

/// A product rule that refuses everything, as if the recipient had changed.
struct RefuseAll;

impl PreSendCheck for RefuseAll {
    fn check<'a>(&'a self, _: TenantId, _: MessageId, _: &'a Email) -> BoxFuture<'a, Result<(), CheckFailed>> {
        Box::pin(async {
            Err(CheckFailed::Refused(
                "the recipient isn't the client's stored email".to_owned(),
            ))
        })
    }
}

async fn approve(pool: &PgPool, email: &Email) -> Result<(), OutboxError> {
    let outbox = Outbox::new(pool.clone(), clock());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, email).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "ana@santos.test", &seen, &NoExtraChecks)
        .await
}

#[sqlx::test]
async fn d7_approval_is_refused_for_two_links_or_an_overlong_body(pool: PgPool) {
    let two_links = Email {
        body_text: "Upload here: https://product.test/u/a or https://product.test/u/b".to_owned(),
        ..sample_email()
    };
    let too_long = Email {
        body_text: "x".repeat(2_001),
        ..sample_email()
    };

    for (case, email) in [("two links", two_links), ("over 2,000 characters", too_long)] {
        let refused = approve(&pool, &email).await;
        assert!(
            matches!(refused, Err(OutboxError::CheckFailed(_))),
            "{case}: {refused:?}"
        );
    }
    approve(
        &pool,
        &Email {
            body_text: "x".repeat(2_000),
            ..sample_email()
        },
    )
    .await
    .unwrap();
}

#[sqlx::test]
async fn d7_a_product_rule_refuses_approval_with_its_reason(pool: PgPool) {
    let outbox = Outbox::new(pool.clone(), clock());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();

    let refused = outbox.approve(tenant, id, "ana@santos.test", &seen, &RefuseAll).await;

    assert!(
        matches!(&refused, Err(OutboxError::CheckFailed(reason)) if reason.contains("stored email")),
        "{refused:?}"
    );
    assert_eq!(outbox.get(tenant, id).await.unwrap().status, MessageStatus::Draft);
}

#[sqlx::test]
async fn d7_the_check_runs_again_just_before_sending(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "ana@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
    let mailer = RecordingMailer::idempotent();

    // Something changed between approval and sending.
    let report = Sender::new(pool, mailer.clone(), clock, Arc::new(RefuseAll))
        .drain_once(10)
        .await
        .unwrap();

    assert_eq!(report.blocked, 1);
    assert!(mailer.calls().is_empty(), "a message failing its check is never sent");
    let message = outbox.get(tenant, id).await.unwrap();
    assert_eq!(message.status, MessageStatus::Blocked);
    assert!(message.last_error.unwrap().contains("stored email"));
}

#[sqlx::test]
async fn d7_a_message_that_may_have_gone_out_stays_unconfirmed_whatever_the_check_says(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "ana@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
    // As if a send was interrupted mid-call and is due for a retry.
    sqlx::query("update outbox_messages set status = 'failed', may_have_sent = true, retry_at = $2, first_send_at = $2 where id = $1")
        .bind(id.as_uuid())
        .bind(clock.now_plus(time::Duration::minutes(-1)))
        .execute(&pool)
        .await
        .unwrap();
    let mailer = RecordingMailer::non_idempotent();

    Sender::new(pool, mailer.clone(), clock, Arc::new(RefuseAll))
        .drain_once(10)
        .await
        .unwrap();

    assert!(mailer.calls().is_empty());
    assert_eq!(outbox.get(tenant, id).await.unwrap().status, MessageStatus::Unconfirmed);
}

/// A product rule whose own lookup fails, as in a database timeout.
struct CantCheck;

impl PreSendCheck for CantCheck {
    fn check<'a>(&'a self, _: TenantId, _: MessageId, _: &'a Email) -> BoxFuture<'a, Result<(), CheckFailed>> {
        Box::pin(async { Err(CheckFailed::Unavailable("pool timed out".to_owned())) })
    }
}

#[sqlx::test]
async fn d7_a_check_that_couldnt_run_leaves_the_message_to_try_again(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    assert!(matches!(
        outbox.approve(tenant, id, "ana@santos.test", &seen, &CantCheck).await,
        Err(OutboxError::CheckUnavailable(_))
    ));
    outbox
        .approve(tenant, id, "ana@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
    let mailer = RecordingMailer::idempotent();

    let report = Sender::new(pool.clone(), mailer.clone(), clock.clone(), Arc::new(CantCheck))
        .drain_once(10)
        .await
        .unwrap();

    assert_eq!(report.blocked, 0);
    assert_eq!(
        outbox.get(tenant, id).await.unwrap().status,
        MessageStatus::Approved,
        "left, still approved, to try again"
    );
    clock.advance(time::Duration::minutes(1));
    Sender::new(pool, mailer.clone(), clock, Arc::new(NoExtraChecks))
        .drain_once(10)
        .await
        .unwrap();
    assert_eq!(mailer.calls().len(), 1);
}

/// A product rule that holds every message until a set time, as quiet
/// hours would.
struct NotBefore {
    clock: Arc<nudgewright::clock::FixedClock>,
    until: time::OffsetDateTime,
}

impl PreSendCheck for NotBefore {
    fn check<'a>(&'a self, _: TenantId, _: MessageId, _: &'a Email) -> BoxFuture<'a, Result<(), CheckFailed>> {
        use nudgewright::clock::Clock as _;
        let result = if self.clock.now() < self.until {
            Err(CheckFailed::NotUntil {
                until: self.until,
                reason: "outside sending hours".to_owned(),
            })
        } else {
            Ok(())
        };
        Box::pin(async move { result })
    }
}

#[sqlx::test]
async fn d7_a_message_held_until_a_time_is_approved_and_sent_then_not_before(pool: PgPool) {
    let clock = clock();
    let holds = Arc::new(NotBefore {
        clock: clock.clone(),
        until: clock.now_plus(time::Duration::hours(3)),
    });
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    // Only the timing waits, so it can be approved now.
    outbox
        .approve(tenant, id, "ana@santos.test", &seen, holds.as_ref())
        .await
        .unwrap();
    let mailer = RecordingMailer::idempotent();
    let sender = Sender::new(pool, mailer.clone(), clock.clone(), holds);

    sender.drain_once(10).await.unwrap();
    clock.advance(time::Duration::hours(2));
    let early = sender.drain_once(10).await.unwrap();
    let held = outbox.get(tenant, id).await.unwrap();
    clock.advance(time::Duration::hours(1));
    sender.drain_once(10).await.unwrap();

    assert_eq!(early, Default::default(), "not even claimed before its time");
    assert_eq!(held.status, MessageStatus::Approved);
    assert!(held.last_error.unwrap().contains("outside sending hours"));
    assert_eq!(mailer.calls().len(), 1);
}
