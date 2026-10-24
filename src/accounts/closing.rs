//! Closing an account. At once: logins and sessions stop and waiting
//! messages are cancelled. After [`GRACE_DAYS`] everything the account holds
//! is erased; until then the operator can reopen it. Cancelled messages
//! stay cancelled.

use serde_json::json;
use time::{Duration, OffsetDateTime};

use super::Accounts;
use crate::audit::{self, AuditEvent};
use crate::files::{FileError, Files};
use crate::ids::TenantId;
use crate::outbox::{Outbox, OutboxError};
use crate::tenants;

/// Days between closing an account and erasing it.
pub const GRACE_DAYS: i64 = 7;

/// Closing errors.
#[derive(Debug, thiserror::Error)]
pub enum CloseError {
    /// The name typed to confirm doesn't match the account's.
    #[error("type the account's name exactly to close it")]
    NameMismatch,
    /// No such account.
    #[error("account not found")]
    NotFound,
    /// Cancelling a waiting message failed.
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    /// The database failed.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl Accounts {
    /// Closes `tenant`'s account once `typed` matches its name (ignoring
    /// case and surrounding spaces), returning when it will be erased.
    /// `actor` names who did it in the audit log. Closing again only
    /// retries cancelling the waiting messages.
    pub async fn close(&self, tenant: TenantId, typed: &str, actor: &str) -> Result<OffsetDateTime, CloseError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query!(
            "select name, erase_after from accounts where tenant_id = $1 for update",
            tenant.as_uuid()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CloseError::NotFound)?;
        if row.name.trim().to_lowercase() != typed.trim().to_lowercase() {
            return Err(CloseError::NameMismatch);
        }
        let erase_after = if let Some(erase_after) = row.erase_after {
            tx.rollback().await?;
            erase_after
        } else {
            let erase_after = now + Duration::days(GRACE_DAYS);
            sqlx::query!(
                "update accounts set closed_at = $2, erase_after = $3 where tenant_id = $1",
                tenant.as_uuid(),
                now,
                erase_after,
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!("delete from account_sessions where tenant_id = $1", tenant.as_uuid())
                .execute(&mut *tx)
                .await?;
            sqlx::query!(
                "delete from account_login_tokens where tenant_id = $1",
                tenant.as_uuid()
            )
            .execute(&mut *tx)
            .await?;
            audit::record(&mut *tx, &account_event(tenant, now, actor, "account.closed")).await?;
            tx.commit().await?;
            erase_after
        };

        // Closed first, so the product drafts nothing new while these are
        // cancelled.
        let waiting = sqlx::query_scalar!(
            "select id from outbox_messages where tenant_id = $1 and status in ('draft', 'approved')",
            tenant.as_uuid(),
        )
        .fetch_all(&self.pool)
        .await?;
        let outbox = Outbox::new(self.pool.clone(), self.clock.clone());
        for id in waiting {
            match outbox.cancel(tenant, id.into(), actor).await {
                // Already being sent: too late to stop.
                Ok(()) | Err(OutboxError::WrongStatus(_)) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(erase_after)
    }

    /// Reopens the closed account owned by `owner_email` if it hasn't been
    /// erased; `false` if there is none. For the operator.
    pub async fn reopen(&self, owner_email: &str) -> Result<bool, sqlx::Error> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let reopened = sqlx::query_scalar!(
            "update accounts a set closed_at = null, erase_after = null
             from account_members m
             where m.tenant_id = a.tenant_id and m.role = 'owner' and lower(m.email) = lower($1)
               and a.closed_at is not null and a.erase_after > $2
             returning a.tenant_id",
            owner_email.trim(),
            now,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(tenant) = reopened else {
            return Ok(false);
        };
        audit::record(
            &mut *tx,
            &account_event(tenant.into(), now, "operator", "account.reopened"),
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Erases every closed account whose grace period has passed: its files,
    /// its engine rows, and (through `on delete cascade`) the product's rows
    /// that point at it. Returns how many were erased; one that fails is
    /// logged and retried next time.
    pub async fn erase_due(&self, files: &Files) -> Result<usize, FileError> {
        let now = self.clock.now();
        let due = sqlx::query_scalar!("select tenant_id from accounts where erase_after <= $1", now)
            .fetch_all(&self.pool)
            .await?;
        let mut erased = 0;
        for id in due {
            let tenant = TenantId::from(id);
            match self.erase(files, tenant, now).await {
                Ok(()) => {
                    erased += 1;
                    tracing::info!(%tenant, "closed account erased");
                }
                Err(err) => tracing::error!(%tenant, %err, "erasing a closed account failed"),
            }
        }
        Ok(erased)
    }

    async fn erase(&self, files: &Files, tenant: TenantId, now: OffsetDateTime) -> Result<(), FileError> {
        tenants::erase(&self.pool, files, tenant).await?;
        sqlx::query!(
            "delete from accounts where tenant_id = $1 and erase_after <= $2",
            tenant.as_uuid(),
            now
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn account_event<'a>(tenant: TenantId, at: OffsetDateTime, actor: &'a str, action: &'a str) -> AuditEvent<'a> {
    AuditEvent {
        tenant,
        at,
        actor,
        action,
        subject_type: "account",
        subject_id: tenant.as_uuid(),
        details: json!({}),
    }
}
