//! The outbox: every outgoing message moves through one state machine, and
//! only an approved message whose content still matches what was approved
//! can be sent (D2). Sending lives in [`Sender`].
//!
//! ```text
//! draft ──approve──► approved ──► sending ──► sent
//!   │                   │            ├──► failed (retry with the SAME key)
//!   │                   │            └──► unconfirmed (may have gone out; a person checks)
//!   │                   └──► blocked (content changed behind the outbox)
//!   ├─ edit → draft again (old approval void)
//!   └─ deadline passes → expired          cancelled ◄── client replied / everything uploaded
//! ```

mod sender;

use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;

use super::audit::{self, AuditEvent};
use super::clock::Clock;
use super::ids::{MessageId, TenantId};
use super::mail::Email;

pub use sender::{DrainReport, Sender};

/// Where a message is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    /// Waiting for approval.
    Draft,
    /// Approved; will be sent.
    Approved,
    /// Handed to the provider; outcome not yet recorded.
    Sending,
    /// The provider accepted it.
    Sent,
    /// The last attempt failed; it retries if a retry is safe and due.
    Failed,
    /// Refused at send time because its content no longer matched the approval.
    Blocked,
    /// May or may not have been sent; a person must check before resending.
    Unconfirmed,
    /// Its deadline passed before it was approved or sent.
    Expired,
    /// No longer needed (e.g. the client replied).
    Cancelled,
}

impl MessageStatus {
    const ALL: [(Self, &'static str); 9] = [
        (Self::Draft, "draft"),
        (Self::Approved, "approved"),
        (Self::Sending, "sending"),
        (Self::Sent, "sent"),
        (Self::Failed, "failed"),
        (Self::Blocked, "blocked"),
        (Self::Unconfirmed, "unconfirmed"),
        (Self::Expired, "expired"),
        (Self::Cancelled, "cancelled"),
    ];

    fn as_str(self) -> &'static str {
        Self::ALL.iter().find(|(s, _)| *s == self).map_or("", |(_, name)| name)
    }

    fn parse(name: &str) -> Result<Self, OutboxError> {
        Self::ALL
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(s, _)| *s)
            .ok_or_else(|| OutboxError::CorruptRow(format!("unknown status {name:?}")))
    }
}

/// A fingerprint of an email's exact content. An approval binds to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// The fingerprint of `email`.
    pub fn of(email: &Email) -> Self {
        let mut hasher = Sha256::new();
        // Length-prefix every field so no two different emails can share
        // the same byte stream.
        for field in [
            email.to_address.as_str(),
            email.from_name.as_str(),
            email.reply_to.as_deref().unwrap_or(""),
            email.subject.as_str(),
            email.body_text.as_str(),
        ] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.update([u8::from(email.reply_to.is_some())]);
        Self(hasher.finalize().into())
    }

    fn from_db(bytes: &[u8]) -> Result<Self, OutboxError> {
        <[u8; 32]>::try_from(bytes)
            .map(Self)
            .map_err(|_| OutboxError::CorruptRow("approved_hash is not 32 bytes".to_owned()))
    }

    /// The hash as bytes, e.g. to put in an approval form.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A message as stored.
#[derive(Debug, Clone)]
pub struct Message {
    /// Its ID.
    pub id: MessageId,
    /// Whose it is.
    pub tenant: TenantId,
    /// Where it is in its life.
    pub status: MessageStatus,
    /// Its content.
    pub email: Email,
    /// When it was sent, if it was.
    pub sent_at: Option<OffsetDateTime>,
    /// The last error, if any.
    pub last_error: Option<String>,
}

impl Message {
    /// The fingerprint of the content as it is now. Show this alongside the
    /// content when asking for approval, and pass it back to [`Outbox::approve`].
    pub fn content_hash(&self) -> ContentHash {
        ContentHash::of(&self.email)
    }
}

/// Outbox errors.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    /// No such message for this tenant. Another tenant's message is reported
    /// the same way (A2).
    #[error("message not found")]
    NotFound,
    /// The message content isn't acceptable.
    #[error("invalid message: {0}")]
    InvalidMessage(String),
    /// The content changed after the approver looked at it.
    #[error("the message changed since it was shown; review it again")]
    StaleApproval,
    /// The message isn't in a state that allows this.
    #[error("not allowed while the message is {0:?}")]
    WrongStatus(MessageStatus),
    /// A stored row doesn't make sense.
    #[error("corrupt outbox row: {0}")]
    CorruptRow(String),
    /// Database failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Creating, editing and approving outbox messages.
#[derive(Clone)]
pub struct Outbox {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl Outbox {
    /// An outbox on `pool`, reading time from `clock`.
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    /// Stores `email` as a draft awaiting approval.
    pub async fn create_draft(&self, tenant: TenantId, email: &Email) -> Result<MessageId, OutboxError> {
        let mut tx = self.pool.begin().await?;
        let id = self.create_draft_in(&mut tx, tenant, email).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// [`Outbox::create_draft`] inside the caller's transaction, so the
    /// Draft commits or rolls back with the caller's own changes.
    pub async fn create_draft_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant: TenantId,
        email: &Email,
    ) -> Result<MessageId, OutboxError> {
        validate(email)?;
        let id = MessageId::new();
        let now = self.clock.now();
        sqlx::query!(
            "insert into outbox_messages
                (id, tenant_id, status, to_address, from_name, reply_to, subject, body_text, created_at, updated_at)
             values ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $8)",
            id.as_uuid(),
            tenant.as_uuid(),
            email.to_address,
            email.from_name,
            email.reply_to,
            email.subject,
            email.body_text,
            now,
        )
        .execute(&mut **tx)
        .await?;
        audit::record(
            &mut **tx,
            &event(tenant, now, audit::SYSTEM, "outbox.draft_created", id),
        )
        .await?;
        Ok(id)
    }

    /// Fetches one of `tenant`'s messages.
    pub async fn get(&self, tenant: TenantId, id: MessageId) -> Result<Message, OutboxError> {
        let row = sqlx::query!(
            "select status, to_address, from_name, reply_to, subject, body_text, sent_at, last_error
             from outbox_messages where id = $1 and tenant_id = $2",
            id.as_uuid(),
            tenant.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(OutboxError::NotFound)?;
        Ok(Message {
            id,
            tenant,
            status: MessageStatus::parse(&row.status)?,
            email: Email {
                to_address: row.to_address,
                from_name: row.from_name,
                reply_to: row.reply_to,
                subject: row.subject,
                body_text: row.body_text,
            },
            sent_at: row.sent_at,
            last_error: row.last_error,
        })
    }

    /// Replaces a draft's or an approved message's content. Any approval is
    /// void: the message is a draft again.
    pub async fn edit(&self, tenant: TenantId, id: MessageId, email: &Email) -> Result<(), OutboxError> {
        validate(email)?;
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let (status, _) = lock(&mut tx, tenant, id).await?;
        if !matches!(status, MessageStatus::Draft | MessageStatus::Approved) {
            return Err(OutboxError::WrongStatus(status));
        }
        sqlx::query!(
            "update outbox_messages
             set to_address = $3, from_name = $4, reply_to = $5, subject = $6, body_text = $7,
                 status = 'draft', approved_hash = null, approved_at = null, approved_by = null, updated_at = $8
             where id = $1 and tenant_id = $2",
            id.as_uuid(),
            tenant.as_uuid(),
            email.to_address,
            email.from_name,
            email.reply_to,
            email.subject,
            email.body_text,
            now,
        )
        .execute(&mut *tx)
        .await?;
        let ev = AuditEvent {
            details: json!({ "approval_voided": status == MessageStatus::Approved }),
            ..event(tenant, now, audit::SYSTEM, "outbox.edited", id)
        };
        audit::record(&mut *tx, &ev).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Approves a draft. `seen` is the [`ContentHash`] of the content the
    /// approver was shown; if the content has changed since, nothing is
    /// approved (D2).
    pub async fn approve(
        &self,
        tenant: TenantId,
        id: MessageId,
        approver: &str,
        seen: &ContentHash,
    ) -> Result<(), OutboxError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let (status, current) = lock(&mut tx, tenant, id).await?;
        if status != MessageStatus::Draft {
            return Err(OutboxError::WrongStatus(status));
        }
        if ContentHash::of(&current) != *seen {
            return Err(OutboxError::StaleApproval);
        }
        sqlx::query!(
            "update outbox_messages
             set status = 'approved', approved_hash = $3, approved_at = $4, approved_by = $5, updated_at = $4
             where id = $1 and tenant_id = $2",
            id.as_uuid(),
            tenant.as_uuid(),
            seen.as_bytes().as_slice(),
            now,
            approver,
        )
        .execute(&mut *tx)
        .await?;
        audit::record(&mut *tx, &event(tenant, now, approver, "outbox.approved", id)).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Locks one of `tenant`'s messages for this transaction and returns its
/// status and content.
async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    tenant: TenantId,
    id: MessageId,
) -> Result<(MessageStatus, Email), OutboxError> {
    let row = sqlx::query!(
        "select status, to_address, from_name, reply_to, subject, body_text
         from outbox_messages where id = $1 and tenant_id = $2 for update",
        id.as_uuid(),
        tenant.as_uuid(),
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(OutboxError::NotFound)?;
    let email = Email {
        to_address: row.to_address,
        from_name: row.from_name,
        reply_to: row.reply_to,
        subject: row.subject,
        body_text: row.body_text,
    };
    Ok((MessageStatus::parse(&row.status)?, email))
}

/// An audit entry about outbox message `id`, with no extra details.
fn event<'a>(tenant: TenantId, at: OffsetDateTime, actor: &'a str, action: &'a str, id: MessageId) -> AuditEvent<'a> {
    AuditEvent {
        tenant,
        at,
        actor,
        action,
        subject_type: "outbox_message",
        subject_id: id.as_uuid(),
        details: json!({}),
    }
}

/// Header fields must be single lines, so a crafted value can't add headers
/// or addresses.
fn validate(email: &Email) -> Result<(), OutboxError> {
    let single_line = [
        ("to address", email.to_address.as_str()),
        ("sender name", email.from_name.as_str()),
        ("reply-to", email.reply_to.as_deref().unwrap_or("")),
        ("subject", email.subject.as_str()),
    ];
    if let Some((field, _)) = single_line.iter().find(|(_, v)| v.contains(['\r', '\n'])) {
        return Err(OutboxError::InvalidMessage(format!("{field} must be a single line")));
    }
    if !email.to_address.contains('@') {
        return Err(OutboxError::InvalidMessage(
            "to address is not an email address".to_owned(),
        ));
    }
    if email.body_text.trim().is_empty() {
        return Err(OutboxError::InvalidMessage("body is empty".to_owned()));
    }
    Ok(())
}
