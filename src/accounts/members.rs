//! The people who log in to an account: one owner, whose address is the
//! account's login address, and up to [`MAX_STAFF`] staff. Every query is
//! scoped by the tenant.

use std::sync::Arc;

use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;

use super::{MemberId, is_email_address};
use crate::audit::{self, AuditEvent};
use crate::clock::Clock;
use crate::ids::TenantId;

/// The most staff an account can have, besides its owner.
pub const MAX_STAFF: i64 = 5;

/// What a member may do. Products decide what each role may see; the
/// engine only keeps the owner from being removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The account's owner, whose address is its login address.
    Owner,
    /// Someone the owner added.
    Staff,
}

impl Role {
    /// `owner` or `staff`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Staff => "staff",
        }
    }

    pub(crate) fn parse(text: &str) -> Self {
        if text == "owner" { Self::Owner } else { Self::Staff }
    }
}

/// One person who logs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Their ID.
    pub id: MemberId,
    /// The address their login links go to.
    pub email: String,
    /// What they may do.
    pub role: Role,
    /// When they were added.
    pub added_at: OffsetDateTime,
}

/// Member errors.
#[derive(Debug, thiserror::Error)]
pub enum MemberError {
    /// Not an email address.
    #[error("{0}")]
    Invalid(String),
    /// The address already logs in to an account.
    #[error("that address already logs in to an account")]
    EmailTaken,
    /// The account already has its staff.
    #[error("an account can have at most {MAX_STAFF} staff")]
    TooManyStaff,
    /// No such member of this account, or the account is closed.
    #[error("member not found")]
    NotFound,
    /// The owner can't be removed.
    #[error("the owner can't be removed")]
    IsOwner,
    /// The database failed.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// The members of each account.
#[derive(Clone)]
pub struct Members {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl Members {
    /// Members in `pool`.
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    /// `tenant`'s members, owner first, then staff by when they were added.
    pub async fn list(&self, tenant: TenantId) -> Result<Vec<Member>, MemberError> {
        let rows = sqlx::query!(
            "select id, email, role, created_at from account_members where tenant_id = $1
             order by role = 'owner' desc, created_at, id",
            tenant.as_uuid(),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| Member {
                id: row.id.into(),
                email: row.email,
                role: Role::parse(&row.role),
                added_at: row.created_at,
            })
            .collect())
    }

    /// Adds `email` to `tenant`'s staff; `by` names who did it in the audit
    /// log.
    pub async fn add_staff(&self, tenant: TenantId, email: &str, by: &str) -> Result<Member, MemberError> {
        let email = email.trim();
        if !is_email_address(email) {
            return Err(MemberError::Invalid(format!("{email:?} is not an email address")));
        }
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        // One change per account at a time, so two can't both squeeze under the limit.
        sqlx::query!(
            "select tenant_id from accounts where tenant_id = $1 and closed_at is null for update",
            tenant.as_uuid()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(MemberError::NotFound)?;
        let staff = sqlx::query_scalar!(
            r#"select count(*) as "count!" from account_members where tenant_id = $1 and role = 'staff'"#,
            tenant.as_uuid()
        )
        .fetch_one(&mut *tx)
        .await?;
        if staff >= MAX_STAFF {
            return Err(MemberError::TooManyStaff);
        }
        let id = MemberId::new();
        sqlx::query!(
            "insert into account_members (id, tenant_id, email, role, created_at) values ($1, $2, $3, 'staff', $4)",
            id.as_uuid(),
            tenant.as_uuid(),
            email,
            now,
        )
        .execute(&mut *tx)
        .await
        .map_err(|err| match err.as_database_error().and_then(|db| db.constraint()) {
            Some("account_members_email") => MemberError::EmailTaken,
            _ => MemberError::Database(err),
        })?;
        let event = member_event(tenant, now, by, "account.member_added", id, email);
        audit::record(&mut *tx, &event).await?;
        tx.commit().await?;
        Ok(Member {
            id,
            email: email.to_owned(),
            role: Role::Staff,
            added_at: now,
        })
    }

    /// Removes a staff member from `tenant`; their sessions and login links
    /// go with them. `by` names who did it in the audit log.
    pub async fn remove_staff(&self, tenant: TenantId, member: MemberId, by: &str) -> Result<(), MemberError> {
        let mut tx = self.pool.begin().await?;
        let role = sqlx::query_scalar!(
            "select role from account_members where tenant_id = $1 and id = $2 for update",
            tenant.as_uuid(),
            member.as_uuid(),
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(MemberError::NotFound)?;
        if Role::parse(&role) == Role::Owner {
            return Err(MemberError::IsOwner);
        }
        let email = sqlx::query_scalar!(
            "delete from account_members where tenant_id = $1 and id = $2 returning email",
            tenant.as_uuid(),
            member.as_uuid(),
        )
        .fetch_one(&mut *tx)
        .await?;
        let event = member_event(tenant, self.clock.now(), by, "account.member_removed", member, &email);
        audit::record(&mut *tx, &event).await?;
        tx.commit().await?;
        Ok(())
    }
}

fn member_event<'a>(
    tenant: TenantId,
    at: OffsetDateTime,
    actor: &'a str,
    action: &'a str,
    member: MemberId,
    email: &str,
) -> AuditEvent<'a> {
    AuditEvent {
        tenant,
        at,
        actor,
        action,
        subject_type: "account_member",
        subject_id: member.as_uuid(),
        details: json!({ "email": email }),
    }
}
