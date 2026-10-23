//! Seam 1: the outbox API with a fake mailer (D2, and E1's retry rules).

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::sync::Arc;

use common::{RecordingMailer, clock, sample_email};
use loopwright::ids::TenantId;
use loopwright::mail::Email;
use loopwright::outbox::{MessageStatus, NoExtraChecks, Outbox, OutboxError, Sender};
use sqlx::PgPool;

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
