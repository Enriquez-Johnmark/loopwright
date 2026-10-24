//! Accounts: who may log in to a tenant, and the account's life from
//! sign-up to erasure. One account is one tenant; its owner's address is
//! its login address.
//!
//! The engine holds the rules and the tables. Products keep their own
//! pages, routes and email wording, and point their per-tenant tables at
//! `accounts (tenant_id)` so erasing an account removes their rows too.

mod logins;
mod members;

use std::sync::Arc;

use sqlx::{PgConnection, PgPool};
use time::OffsetDateTime;

use crate::clock::Clock;
use crate::ids::{TenantId, id_type};
use crate::outbox::links;

pub use logins::{
    LINK_LIFETIME, LOGIN_APPROVER, LoginError, LoginLink, LoginOutcome, Logins, MAX_ACCOUNT_LINKS_PER_HOUR,
    MAX_LINKS_PER_HOUR, SESSION_LIFETIME, Session,
};
pub use members::{MAX_STAFF, Member, MemberError, Members, Role};

id_type!(
    /// One person who logs in to an account.
    MemberId
);

/// An account as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Its tenant.
    pub tenant: TenantId,
    /// The name its messages are signed with.
    pub name: String,
    /// Its owner's address, which logs in.
    pub owner_email: String,
    /// When it was created.
    pub created_at: OffsetDateTime,
    /// When it was closed, if it is.
    pub closed_at: Option<OffsetDateTime>,
}

/// Account errors.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// A name or address that can't be used.
    #[error("{0}")]
    Invalid(String),
    /// The address already logs in to an account.
    #[error("that address already logs in to an account")]
    EmailTaken,
    /// No such account.
    #[error("account not found")]
    NotFound,
    /// The database failed.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// The accounts.
#[derive(Clone)]
pub struct Accounts {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl Accounts {
    /// Accounts in `pool`.
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    /// Creates an open account named `name`, owned by `owner_email`.
    pub async fn create(&self, name: &str, owner_email: &str) -> Result<TenantId, AccountError> {
        let mut tx = self.pool.begin().await?;
        let tenant = create_in(&mut tx, self.clock.now(), name, owner_email).await?;
        tx.commit().await?;
        Ok(tenant)
    }

    /// The account of `tenant`, open or closed.
    pub async fn get(&self, tenant: TenantId) -> Result<Account, AccountError> {
        let row = sqlx::query!(
            "select a.name, a.created_at, a.closed_at, m.email
             from accounts a join account_members m on m.tenant_id = a.tenant_id and m.role = 'owner'
             where a.tenant_id = $1",
            tenant.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AccountError::NotFound)?;
        Ok(Account {
            tenant,
            name: row.name,
            owner_email: row.email,
            created_at: row.created_at,
            closed_at: row.closed_at,
        })
    }
}

/// Creates an account and its owner in the caller's transaction.
pub async fn create_in(
    tx: &mut PgConnection,
    now: OffsetDateTime,
    name: &str,
    owner_email: &str,
) -> Result<TenantId, AccountError> {
    let (name, owner_email) = (name.trim(), owner_email.trim());
    check_name(name).map_err(AccountError::Invalid)?;
    if !is_email_address(owner_email) {
        return Err(AccountError::Invalid(format!(
            "{owner_email:?} is not an email address"
        )));
    }
    let tenant = TenantId::new();
    sqlx::query!(
        "insert into accounts (tenant_id, name, created_at) values ($1, $2, $3)",
        tenant.as_uuid(),
        name,
        now,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "insert into account_members (id, tenant_id, email, role, created_at) values ($1, $2, $3, 'owner', $4)",
        MemberId::new().as_uuid(),
        tenant.as_uuid(),
        owner_email,
        now,
    )
    .execute(&mut *tx)
    .await
    .map_err(email_taken)?;
    Ok(tenant)
}

/// Whether `name` can sign an account's messages: 1 to 100 characters, no
/// control characters, and nothing that reads as a link, which the outbox's
/// pre-send checks would refuse in every message.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err("a name is 1 to 100 characters".to_owned());
    }
    if !links(name).is_empty() {
        return Err(format!("{name:?} looks like a web address; use the business name"));
    }
    Ok(())
}

/// Whether `email` looks like an address a login link can go to.
pub fn is_email_address(email: &str) -> bool {
    email.len() <= 254 && email.contains('@') && !email.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Turns the unique-address violation into [`AccountError::EmailTaken`].
fn email_taken(err: sqlx::Error) -> AccountError {
    match err.as_database_error().and_then(|db| db.constraint()) {
        Some("account_members_email") => AccountError::EmailTaken,
        _ => AccountError::Database(err),
    }
}
