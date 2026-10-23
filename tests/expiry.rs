//! Seam 4 (M3), engine part: Drafts expire at their deadline (D4), and a
//! Draft can be discarded.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::sync::Arc;

use common::{RecordingMailer, clock, sample_email};
use loopwright::ids::TenantId;
use loopwright::outbox::{MessageStatus, NoExtraChecks, Outbox, OutboxError, Sender};
use sqlx::PgPool;
use time::Duration;

#[sqlx::test]
async fn d4_a_draft_not_approved_by_its_deadline_expires_and_is_never_sent(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let mut tx = pool.begin().await.unwrap();
    let id = outbox
        .create_draft_in(
            &mut tx,
            tenant,
            &sample_email(),
            Some(clock.now_plus(Duration::hours(1))),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();

    clock.advance(Duration::hours(1));
    let late = outbox
        .approve(tenant, id, "ana@santos.test", &seen, &NoExtraChecks)
        .await;

    assert!(matches!(late, Err(OutboxError::Expired)), "{late:?}");
    assert_eq!(outbox.get(tenant, id).await.unwrap().status, MessageStatus::Expired);
    let mailer = RecordingMailer::idempotent();
    Sender::new(pool, mailer.clone(), clock, Arc::new(NoExtraChecks))
        .drain_once(10)
        .await
        .unwrap();
    assert!(mailer.calls().is_empty());
}

#[sqlx::test]
async fn d4_overdue_drafts_are_swept_to_expired_and_others_left_alone(pool: PgPool) {
    let clock = clock();
    let outbox = Outbox::new(pool.clone(), clock.clone());
    let tenant = TenantId::new();
    let mut tx = pool.begin().await.unwrap();
    let overdue = outbox
        .create_draft_in(
            &mut tx,
            tenant,
            &sample_email(),
            Some(clock.now_plus(Duration::minutes(5))),
        )
        .await
        .unwrap();
    let later = outbox
        .create_draft_in(
            &mut tx,
            tenant,
            &sample_email(),
            Some(clock.now_plus(Duration::days(2))),
        )
        .await
        .unwrap();
    let approved = outbox
        .create_draft_in(
            &mut tx,
            tenant,
            &sample_email(),
            Some(clock.now_plus(Duration::minutes(5))),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let seen = outbox.get(tenant, approved).await.unwrap().content_hash();
    outbox
        .approve(tenant, approved, "ana@santos.test", &seen, &NoExtraChecks)
        .await
        .unwrap();

    clock.advance(Duration::hours(1));
    assert_eq!(outbox.expire_overdue().await.unwrap(), 2);

    assert_eq!(
        outbox.get(tenant, overdue).await.unwrap().status,
        MessageStatus::Expired
    );
    assert_eq!(outbox.get(tenant, later).await.unwrap().status, MessageStatus::Draft);
    assert_eq!(
        outbox.get(tenant, approved).await.unwrap().status,
        MessageStatus::Expired,
        "approved, but held (by the send budget) past its deadline: never sent late"
    );
}

#[sqlx::test]
async fn a_discarded_draft_can_never_be_approved(pool: PgPool) {
    let outbox = Outbox::new(pool.clone(), clock());
    let tenant = TenantId::new();
    let id = outbox.create_draft(tenant, &sample_email()).await.unwrap();
    let seen = outbox.get(tenant, id).await.unwrap().content_hash();

    outbox.cancel(tenant, id, "ana@santos.test").await.unwrap();

    assert_eq!(outbox.get(tenant, id).await.unwrap().status, MessageStatus::Cancelled);
    let approve = outbox
        .approve(tenant, id, "ana@santos.test", &seen, &NoExtraChecks)
        .await;
    assert!(
        matches!(approve, Err(OutboxError::WrongStatus(MessageStatus::Cancelled))),
        "{approve:?}"
    );
    assert!(matches!(
        outbox.cancel(TenantId::new(), id, "x").await,
        Err(OutboxError::NotFound)
    ));
}
