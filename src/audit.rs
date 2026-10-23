//! The append-only audit log: who did what to which record, and when.

use serde_json::Value;
use sqlx::PgExecutor;
use time::OffsetDateTime;
use uuid::Uuid;

use super::ids::TenantId;

/// The actor recorded for things the program does by itself.
pub const SYSTEM: &str = "system";

/// One audit entry to record.
#[derive(Debug)]
pub struct AuditEvent<'a> {
    /// Whose data this concerns.
    pub tenant: TenantId,
    /// When it happened (from the Clock).
    pub at: OffsetDateTime,
    /// Who did it: a person's identifier, or `"system"`.
    pub actor: &'a str,
    /// What happened, e.g. `"outbox.approved"`.
    pub action: &'a str,
    /// The kind of record, e.g. `"outbox_message"`.
    pub subject_type: &'a str,
    /// The record's ID.
    pub subject_id: Uuid,
    /// Extra facts worth keeping.
    pub details: Value,
}

/// Appends `event` to the audit log, inside the caller's transaction where
/// one is used, so the entry and the change commit together.
pub async fn record(executor: impl PgExecutor<'_>, event: &AuditEvent<'_>) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "insert into audit_events (id, tenant_id, at, actor, action, subject_type, subject_id, details)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
        Uuid::now_v7(),
        event.tenant.as_uuid(),
        event.at,
        event.actor,
        event.action,
        event.subject_type,
        event.subject_id,
        event.details,
    )
    .execute(executor)
    .await?;
    Ok(())
}
