//! Seam 1: the outbox API with a fake mailer (D2, and E1's retry rules).

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::sync::Arc;

use common::{RecordingMailer, clock, sample_email};
use nudgewright::clock::FixedClock;
use nudgewright::ids::{MessageId, TenantId};
use nudgewright::mail::{Email, MailError};
use nudgewright::outbox::{MessageStatus, NoExtraChecks, Outbox, OutboxError, Sender};
use sqlx::PgPool;
use time::Duration;

#[sqlx::test]
async fn d2_approved_draft_is_sent_with_its_exact_content(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "bookkeeper@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
    let mailer = RecordingMailer::idempotent();

    Sender::new(pool, mailer.clone(), clock, Arc::new(NoExtraChecks))
        .drain_once(10)
        .await
        .unwrap();

    let calls = mailer.calls();
    assert_eq!(calls.len(), 1, "expected one send, got {calls:?}");
    assert_eq!(calls[0].0.body_text, sample_email().body_text);
}

#[sqlx::test]
async fn d2_unapproved_draft_is_never_sent(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    outbox.create_draft(TenantId::new(), &sample_email()).await.unwrap();
    let mailer = RecordingMailer::idempotent();

    Sender::new(pool, mailer.clone(), clock, Arc::new(NoExtraChecks))
        .drain_once(10)
        .await
        .unwrap();

    assert_eq!(mailer.calls().len(), 0, "an unapproved draft was sent");
}

#[sqlx::test]
async fn d2_edit_after_approval_voids_it_so_nothing_is_sent(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "bookkeeper@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
    let edited = Email {
        body_text: "Edited after approval".to_owned(),
        ..sample_email()
    };
    outbox
        .edit(tenant, id, &edited, "bookkeeper@santos.test")
        .await
        .unwrap();
    let mailer = RecordingMailer::idempotent();

    Sender::new(pool, mailer.clone(), clock, Arc::new(NoExtraChecks))
        .drain_once(10)
        .await
        .unwrap();

    assert_eq!(
        mailer.calls().len(),
        0,
        "an edited message went out on its old approval"
    );
}

#[sqlx::test]
async fn d2_content_changed_behind_the_outbox_is_blocked_not_sent(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "bookkeeper@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
    // A bug or bad actor rewrites the body directly, bypassing `edit`.
    sqlx::query("update outbox_messages set body_text = 'Pay to this account instead' where id = $1")
        .bind(id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let mailer = RecordingMailer::idempotent();

    Sender::new(pool, mailer.clone(), clock, Arc::new(NoExtraChecks))
        .drain_once(10)
        .await
        .unwrap();

    assert_eq!(outbox.get(tenant, id).await.unwrap().status, MessageStatus::Blocked);
}

#[sqlx::test]
async fn d2_approving_content_the_approver_never_saw_is_refused(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock);
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    let edited = Email {
        subject: "Changed while the approver was looking".to_owned(),
        ..sample_email()
    };
    outbox
        .edit(tenant, id, &edited, "bookkeeper@santos.test")
        .await
        .unwrap();

    let result = outbox
        .approve(tenant, id, "bookkeeper@santos.test", &seen, &NoExtraChecks)
        .await;

    assert!(
        matches!(result, Err(OutboxError::StaleApproval)),
        "expected StaleApproval, got {result:?}"
    );
}

#[sqlx::test]
async fn e1_retry_after_an_unknown_outcome_reuses_the_same_idempotency_key(pool: PgPool) {
    let clock = clock();
    let tenant = TenantId::new();
    approved_message(&pool, &clock, tenant).await;
    let mailer = RecordingMailer::idempotent().then_fail(MailError::Unknown("timeout".to_owned()));
    let sender = Sender::new(pool, mailer.clone(), clock.clone(), Arc::new(NoExtraChecks));
    sender.drain_once(10).await.unwrap();
    clock.advance(Duration::minutes(2));

    sender.drain_once(10).await.unwrap();

    let keys: Vec<String> = mailer.calls().into_iter().map(|(_, key)| key).collect();
    assert!(
        keys.len() == 2 && keys[0] == keys[1],
        "expected two attempts with one key, got {keys:?}"
    );
}

#[sqlx::test]
async fn e1_unknown_outcome_without_duplicate_blocking_is_unconfirmed_and_never_retried(pool: PgPool) {
    let clock = clock();
    let tenant = TenantId::new();
    approved_message(&pool, &clock, tenant).await;
    let mailer = RecordingMailer::non_idempotent().then_fail(MailError::Unknown("timeout".to_owned()));
    let sender = Sender::new(pool, mailer.clone(), clock.clone(), Arc::new(NoExtraChecks));
    sender.drain_once(10).await.unwrap();
    clock.advance(Duration::hours(1));

    sender.drain_once(10).await.unwrap();

    assert_eq!(mailer.calls().len(), 1, "a send that may have gone out was retried");
}

#[sqlx::test]
async fn e1_unknown_outcome_past_the_duplicate_window_becomes_unconfirmed(pool: PgPool) {
    let clock = clock();
    let tenant = TenantId::new();
    let id = approved_message(&pool, &clock, tenant).await;
    let mailer = RecordingMailer::idempotent().then_fail(MailError::Unknown("timeout".to_owned()));
    let sender = Sender::new(pool.clone(), mailer, clock.clone(), Arc::new(NoExtraChecks));
    sender.drain_once(10).await.unwrap();
    clock.advance(Duration::hours(24));

    sender.drain_once(10).await.unwrap();

    let status = Outbox::new(pool, clock).get(tenant, id).await.unwrap().status;
    assert_eq!(status, MessageStatus::Unconfirmed);
}

#[sqlx::test]
async fn e1_a_send_that_certainly_failed_is_retried_even_without_duplicate_blocking(pool: PgPool) {
    let clock = clock();
    let tenant = TenantId::new();
    let id = approved_message(&pool, &clock, tenant).await;
    let mailer = RecordingMailer::non_idempotent().then_fail(MailError::NotSent("429".to_owned()));
    let sender = Sender::new(pool.clone(), mailer, clock.clone(), Arc::new(NoExtraChecks));
    sender.drain_once(10).await.unwrap();
    clock.advance(Duration::minutes(2));

    sender.drain_once(10).await.unwrap();

    let status = Outbox::new(pool, clock).get(tenant, id).await.unwrap().status;
    assert_eq!(status, MessageStatus::Sent);
}

#[sqlx::test]
async fn e1_a_rejected_send_is_not_retried(pool: PgPool) {
    let clock = clock();
    let tenant = TenantId::new();
    approved_message(&pool, &clock, tenant).await;
    let mailer = RecordingMailer::idempotent().then_fail(MailError::Rejected("invalid to".to_owned()));
    let sender = Sender::new(pool, mailer.clone(), clock.clone(), Arc::new(NoExtraChecks));
    sender.drain_once(10).await.unwrap();
    clock.advance(Duration::hours(3));

    sender.drain_once(10).await.unwrap();

    assert_eq!(mailer.calls().len(), 1, "a rejected email was retried");
}

#[sqlx::test]
async fn e1_unknown_outcome_that_runs_out_of_retries_ends_unconfirmed_not_failed(pool: PgPool) {
    let clock = clock();
    let tenant = TenantId::new();
    let id = approved_message(&pool, &clock, tenant).await;
    let mut mailer = RecordingMailer::idempotent();
    for _ in 0..5 {
        mailer = mailer.then_fail(MailError::Unknown("timeout".to_owned()));
    }
    let sender = Sender::new(pool.clone(), mailer, clock.clone(), Arc::new(NoExtraChecks));
    for _ in 0..5 {
        sender.drain_once(10).await.unwrap();
        clock.advance(Duration::hours(3));
    }

    let status = Outbox::new(pool, clock).get(tenant, id).await.unwrap().status;
    assert_eq!(status, MessageStatus::Unconfirmed);
}

#[sqlx::test]
async fn e1_provider_already_holding_the_key_means_unconfirmed(pool: PgPool) {
    let clock = clock();
    let tenant = TenantId::new();
    let id = approved_message(&pool, &clock, tenant).await;
    let mailer = RecordingMailer::idempotent().then_fail(MailError::KeyAlreadyUsed("409".to_owned()));

    Sender::new(pool.clone(), mailer, clock.clone(), Arc::new(NoExtraChecks))
        .drain_once(10)
        .await
        .unwrap();

    let status = Outbox::new(pool, clock).get(tenant, id).await.unwrap().status;
    assert_eq!(status, MessageStatus::Unconfirmed);
}

async fn approved_message(pool: &PgPool, clock: &Arc<FixedClock>, tenant: TenantId) -> MessageId {
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();
    outbox
        .approve(tenant, id, "bookkeeper@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();
    id
}
