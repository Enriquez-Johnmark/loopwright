//! Sends approved messages, at most once each (E1), and only with the exact
//! content that was approved (D2).
//!
//! One sender runs at a time, guarded by a Postgres advisory lock held on its
//! own connection (not a pooled one), so the lock dies with the connection.
//! When a sender starts and finds a message still marked `sending`, the
//! process that was sending it died mid-call: its outcome is unknown.
//!
//! The sender, like the job workers, works across all tenants; every update
//! still names the row's tenant.

use std::sync::Arc;

use serde_json::json;
use sqlx::{Connection, PgConnection, PgPool, Postgres, Transaction};
use time::{Duration, OffsetDateTime};

use super::checks::{self, CheckFailed, PreSendCheck};
use super::{ContentHash, MessageStatus, OutboxError, event};
use crate::engine::audit::{self, AuditEvent};
use crate::engine::clock::Clock;
use crate::engine::ids::{MessageId, TenantId};
use crate::engine::mail::{Email, MailError, Mailer, SendReceipt};

/// Advisory-lock key that makes the sender a single instance.
const SENDER_LOCK: i64 = 0x6b6f_6d70_6f75_7462;
/// Resend blocks duplicate keys for 24 hours; stop retrying an unknown
/// outcome an hour before that.
const DUPLICATE_WINDOW: Duration = Duration::hours(23);
/// Attempts before a message that keeps failing is left for a person.
const MAX_ATTEMPTS: i32 = 5;
const UNCONFIRMED_REASON: &str = "may have been sent; check before resending";

/// What one drain did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainReport {
    /// Another sender held the lock; nothing was done.
    pub busy: bool,
    /// Messages the provider accepted.
    pub sent: usize,
    /// Messages that failed (some will retry).
    pub failed: usize,
    /// Messages refused because their content no longer matched the
    /// approval, or a pre-send check failed (D7).
    pub blocked: usize,
    /// Messages that may or may not have gone out and need a person.
    pub unconfirmed: usize,
}

/// How many messages may start sending in any 24 hours, under a provider's
/// daily cap. Messages with a deadline (`expires_at`, e.g. Reminders) stop
/// at `per_day`; ones without (login links, notices) may also use the
/// `reserve` above it, so a full day of Reminders never locks anyone out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendBudget {
    /// Messages with a deadline, per 24 hours.
    pub per_day: i64,
    /// Extra room for messages without one.
    pub reserve: i64,
}

impl SendBudget {
    /// Resend Free allows 100 a day: 90 Reminders, 10 kept for the rest.
    pub const RESEND_FREE: Self = Self {
        per_day: 90,
        reserve: 10,
    };
}

/// How many messages started sending in the 24 hours before `now`, across
/// all tenants: the provider's cap is for the whole account.
pub async fn started_last_day(pool: &PgPool, now: OffsetDateTime) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"select count(*) as "count!" from outbox_messages where first_send_at > $1"#,
        now - Duration::hours(24),
    )
    .fetch_one(pool)
    .await
}

/// Sends approved messages through a [`Mailer`].
pub struct Sender<M> {
    pool: PgPool,
    mailer: M,
    clock: Arc<dyn Clock>,
    product_checks: Arc<dyn PreSendCheck>,
    budget: Option<SendBudget>,
}

/// A message this sender has marked `sending` and now owns.
struct Claimed {
    id: MessageId,
    tenant: TenantId,
    attempt: i32,
    first_send_at: OffsetDateTime,
    email: Email,
}

enum Claim {
    Send(Claimed),
    Refused,
    NothingDue,
}

/// What a failed attempt leads to.
struct NextStep {
    status: MessageStatus,
    retry_at: Option<OffsetDateTime>,
    may_have_sent: bool,
}

impl<M: Mailer> Sender<M> {
    /// A sender on `pool` using `mailer`, checking every message against
    /// the engine's rules and `product_checks` just before sending (D7).
    pub fn new(pool: PgPool, mailer: M, clock: Arc<dyn Clock>, product_checks: Arc<dyn PreSendCheck>) -> Self {
        Self {
            pool,
            mailer,
            clock,
            product_checks,
            budget: None,
        }
    }

    /// Sends no more than `budget` allows; without one, there's no limit.
    #[must_use]
    pub fn with_budget(mut self, budget: SendBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Settles any send interrupted by a crash, then sends up to `limit` due
    /// messages.
    pub async fn drain_once(&self, limit: usize) -> Result<DrainReport, OutboxError> {
        // A dedicated connection: if this future is dropped or the process
        // dies, the connection closes and Postgres releases the lock.
        let mut lock_conn = PgConnection::connect_with(&self.pool.connect_options()).await?;
        let locked = sqlx::query_scalar!("select pg_try_advisory_lock($1)", SENDER_LOCK)
            .fetch_one(&mut lock_conn)
            .await?
            .unwrap_or(false);
        if !locked {
            return Ok(DrainReport {
                busy: true,
                ..DrainReport::default()
            });
        }
        let result = self.drain_locked(limit).await;
        // Closing the session releases the lock; a failed close drops the
        // socket, which releases it too.
        if let Err(err) = lock_conn.close().await {
            tracing::warn!(%err, "closing the sender's lock connection");
        }
        result
    }

    async fn drain_locked(&self, limit: usize) -> Result<DrainReport, OutboxError> {
        let mut report = DrainReport::default();
        self.settle_interrupted_sends().await?;
        for _ in 0..limit {
            let mut tx = self.pool.begin().await?;
            let claim = self.claim(&mut tx, &mut report).await?;
            tx.commit().await?;
            match claim {
                Claim::NothingDue => break,
                Claim::Refused => {}
                Claim::Send(claimed) => {
                    // The key is the message ID: the same on every attempt,
                    // and stored before the provider is called.
                    let outcome = self.mailer.send(&claimed.email, &claimed.id.to_string()).await;
                    failpoint("after_send_before_record");
                    self.record(&claimed, outcome, &mut report).await?;
                }
            }
        }
        Ok(report)
    }

    /// Any message still `sending` belongs to a sender that died mid-call.
    /// Its email may have gone out; mark it so and let `claim` decide.
    async fn settle_interrupted_sends(&self) -> Result<(), OutboxError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let interrupted = sqlx::query!(
            "update outbox_messages
             set status = 'failed', may_have_sent = true, retry_at = $1, updated_at = $1,
                 last_error = 'interrupted mid-send'
             where status = 'sending'
             returning id, tenant_id",
            now,
        )
        .fetch_all(&mut *tx)
        .await?;
        for row in interrupted {
            let ev = event(
                row.tenant_id.into(),
                now,
                audit::SYSTEM,
                "outbox.send_interrupted",
                row.id.into(),
            );
            audit::record(&mut *tx, &ev).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn claim(&self, tx: &mut Transaction<'_, Postgres>, report: &mut DrainReport) -> Result<Claim, OutboxError> {
        let now = self.clock.now();
        // A retry was counted when it first started, so it may always go.
        let (dated, undated) = match self.budget {
            None => (true, true),
            Some(budget) => {
                let started = started_last_day(&self.pool, now).await?;
                (started < budget.per_day, started < budget.per_day + budget.reserve)
            }
        };
        // Nearest deadline first; messages without one before all. One held
        // past its deadline never goes out late: `expire_overdue` ends it.
        let Some(row) = sqlx::query!(
            "select id, tenant_id, to_address, from_name, reply_to, subject, body_text,
                    approved_hash, may_have_sent, first_send_at, send_attempts
             from outbox_messages
             where (status = 'approved' or (status = 'failed' and retry_at <= $1))
               and (first_send_at is not null
                    or (expires_at is null and $3) or (expires_at > $1 and $2))
             order by expires_at nulls first, approved_at
             limit 1
             for update skip locked",
            now,
            dated,
            undated,
        )
        .fetch_optional(&mut **tx)
        .await?
        else {
            return Ok(Claim::NothingDue);
        };
        let id = MessageId::from(row.id);
        let tenant = TenantId::from(row.tenant_id);
        let email = Email {
            to_address: row.to_address,
            from_name: row.from_name,
            reply_to: row.reply_to,
            subject: row.subject,
            body_text: row.body_text,
        };

        let approved = row.approved_hash.as_deref().map(ContentHash::from_db).transpose()?;
        if approved.as_ref() != Some(&ContentHash::of(&email)) {
            report.blocked += 1;
            refuse(
                tx,
                tenant,
                id,
                now,
                MessageStatus::Blocked,
                "content changed after approval",
            )
            .await?;
            return Ok(Claim::Refused);
        }
        // Before the pre-send check: a message that may already have gone
        // out needs a person whatever the check would say now.
        if row.may_have_sent && !self.may_retry_unknown(row.first_send_at, now) {
            report.unconfirmed += 1;
            refuse(tx, tenant, id, now, MessageStatus::Unconfirmed, UNCONFIRMED_REASON).await?;
            return Ok(Claim::Refused);
        }
        match checks::run(self.product_checks.as_ref(), tenant, id, &email).await {
            Ok(()) => {}
            Err(CheckFailed::Refused(reason)) => {
                report.blocked += 1;
                refuse(tx, tenant, id, now, MessageStatus::Blocked, &reason).await?;
                return Ok(Claim::Refused);
            }
            Err(CheckFailed::Unavailable(reason)) => {
                // Not the message's fault: try again shortly, without
                // counting an attempt.
                report.failed += 1;
                defer(tx, tenant, id, now + backoff(1), &reason).await?;
                return Ok(Claim::Refused);
            }
        }

        let first_send_at = row.first_send_at.unwrap_or(now);
        sqlx::query!(
            "update outbox_messages
             set status = 'sending', send_attempts = send_attempts + 1,
                 first_send_at = $3, send_started_at = $4, retry_at = null, updated_at = $4
             where id = $1 and tenant_id = $2",
            id.as_uuid(),
            tenant.as_uuid(),
            first_send_at,
            now,
        )
        .execute(&mut **tx)
        .await?;
        audit::record(&mut **tx, &event(tenant, now, audit::SYSTEM, "outbox.send_started", id)).await?;
        Ok(Claim::Send(Claimed {
            id,
            tenant,
            attempt: row.send_attempts + 1,
            first_send_at,
            email,
        }))
    }

    /// A send whose earlier outcome is unknown may be retried only through a
    /// provider that blocks duplicate keys, and only inside its window.
    fn may_retry_unknown(&self, first_send_at: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
        self.mailer.blocks_duplicate_keys() && first_send_at.is_some_and(|first| now - first < DUPLICATE_WINDOW)
    }

    async fn record(
        &self,
        claimed: &Claimed,
        outcome: Result<SendReceipt, MailError>,
        report: &mut DrainReport,
    ) -> Result<(), OutboxError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        // Fence on (status, attempt): if another sender has since taken this
        // message over, its record wins and this late result is dropped.
        let Some(row) = sqlx::query!(
            "select may_have_sent from outbox_messages
             where id = $1 and tenant_id = $2 and status = 'sending' and send_attempts = $3
             for update",
            claimed.id.as_uuid(),
            claimed.tenant.as_uuid(),
            claimed.attempt,
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            tracing::warn!(id = %claimed.id, "send result arrived after another sender took over; not recorded");
            return Ok(());
        };

        let (action, details) = match outcome {
            Ok(receipt) => {
                sqlx::query!(
                    "update outbox_messages
                     set status = 'sent', provider_message_id = $3, sent_at = $4, last_error = null, updated_at = $4
                     where id = $1 and tenant_id = $2",
                    claimed.id.as_uuid(),
                    claimed.tenant.as_uuid(),
                    receipt.provider_message_id,
                    now,
                )
                .execute(&mut *tx)
                .await?;
                report.sent += 1;
                (
                    "outbox.sent",
                    json!({ "provider_message_id": receipt.provider_message_id }),
                )
            }
            Err(err) => {
                let next = self.after_failure(&err, row.may_have_sent, claimed, now);
                if next.status == MessageStatus::Unconfirmed {
                    report.unconfirmed += 1;
                } else {
                    report.failed += 1;
                }
                sqlx::query!(
                    "update outbox_messages
                     set status = $3, retry_at = $4, may_have_sent = $5, last_error = $6, updated_at = $7
                     where id = $1 and tenant_id = $2",
                    claimed.id.as_uuid(),
                    claimed.tenant.as_uuid(),
                    next.status.as_str(),
                    next.retry_at,
                    next.may_have_sent,
                    err.to_string(),
                    now,
                )
                .execute(&mut *tx)
                .await?;
                (
                    "outbox.send_failed",
                    json!({ "error": err.to_string(), "retry_at": next.retry_at }),
                )
            }
        };
        let ev = AuditEvent {
            details,
            ..event(claimed.tenant, now, audit::SYSTEM, action, claimed.id)
        };
        audit::record(&mut *tx, &ev).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Decides what follows a failed attempt. A message that may have gone
    /// out and won't be retried always ends `unconfirmed`, never plain
    /// `failed`, so nobody resends it without checking.
    fn after_failure(
        &self,
        err: &MailError,
        already_maybe_sent: bool,
        claimed: &Claimed,
        now: OffsetDateTime,
    ) -> NextStep {
        let may_have_sent = already_maybe_sent || matches!(err, MailError::Unknown(_) | MailError::KeyAlreadyUsed(_));
        let retry_is_safe = match err {
            MailError::Rejected(_) | MailError::KeyAlreadyUsed(_) => false,
            MailError::NotSent(_) | MailError::Unknown(_) => {
                !may_have_sent || self.may_retry_unknown(Some(claimed.first_send_at), now)
            }
        };
        let retry_at = (retry_is_safe && claimed.attempt < MAX_ATTEMPTS).then(|| now + backoff(claimed.attempt));
        let status = if retry_at.is_none() && may_have_sent {
            MessageStatus::Unconfirmed
        } else {
            MessageStatus::Failed
        };
        NextStep {
            status,
            retry_at,
            may_have_sent,
        }
    }
}

/// Takes a message out of the send path with `status`, and records why.
async fn refuse(
    tx: &mut Transaction<'_, Postgres>,
    tenant: TenantId,
    id: MessageId,
    now: OffsetDateTime,
    status: MessageStatus,
    reason: &str,
) -> Result<(), OutboxError> {
    sqlx::query!(
        "update outbox_messages set status = $3, retry_at = null, last_error = $4, updated_at = $5
         where id = $1 and tenant_id = $2",
        id.as_uuid(),
        tenant.as_uuid(),
        status.as_str(),
        reason,
        now,
    )
    .execute(&mut **tx)
    .await?;
    let ev = AuditEvent {
        details: json!({ "status": status.as_str(), "reason": reason }),
        ..event(tenant, now, audit::SYSTEM, "outbox.refused", id)
    };
    audit::record(&mut **tx, &ev).await?;
    Ok(())
}

/// Wait before the retry that follows attempt number `attempt`: 1 min,
/// 5 min, 30 min, then 2 h.
fn backoff(attempt: i32) -> Duration {
    match attempt {
        ..=1 => Duration::minutes(1),
        2 => Duration::minutes(5),
        3 => Duration::minutes(30),
        _ => Duration::hours(2),
    }
}

/// Test-only crash point for the E1 test: aborts the process when
/// `KOMPLETO_FAILPOINT` names this point. Compiled out of normal builds.
#[cfg(feature = "failpoints")]
fn failpoint(name: &str) {
    if std::env::var("KOMPLETO_FAILPOINT").is_ok_and(|v| v == name) {
        std::process::abort();
    }
}

#[cfg(not(feature = "failpoints"))]
fn failpoint(_name: &str) {}

/// Leaves a message for a later attempt after its pre-send check couldn't
/// run. It keeps its approval and attempt count.
async fn defer(
    tx: &mut Transaction<'_, Postgres>,
    tenant: TenantId,
    id: MessageId,
    retry_at: OffsetDateTime,
    reason: &str,
) -> Result<(), OutboxError> {
    sqlx::query!(
        "update outbox_messages set status = 'failed', retry_at = $3, last_error = $4, updated_at = $3
         where id = $1 and tenant_id = $2",
        id.as_uuid(),
        tenant.as_uuid(),
        retry_at,
        format!("couldn't run the pre-send check: {reason}"),
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}
