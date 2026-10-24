//! Requests for an account from a public sign-up page. A request creates
//! nothing and sends nothing: the operator approves it, which creates the
//! account. The answer to a request is the same whether or not the address
//! already has an account.
//!
//! Requests belong to no tenant, so these queries aren't scoped by one;
//! only the operator reads them.

use std::sync::Arc;

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::{AccountError, check_name, create_in, is_email_address};
use crate::clock::Clock;
use crate::ids::{TenantId, id_type};

id_type!(
    /// One account request.
    RequestId
);

/// The most requests waiting at once.
pub const MAX_PENDING: i64 = 200;
/// The most new requests an hour, from everyone.
pub const MAX_PER_HOUR: i64 = 20;
/// Requests nobody approved are deleted after this many days.
pub const EXPIRES_AFTER_DAYS: i64 = 30;

/// A request waiting for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRequest {
    /// Its ID.
    pub id: RequestId,
    /// The account name asked for.
    pub name: String,
    /// The address that will own the account.
    pub email: String,
    /// When it was made.
    pub requested_at: OffsetDateTime,
}

/// Sign-up errors.
#[derive(Debug, thiserror::Error)]
pub enum SignupError {
    /// Every problem with the request, for the form to show.
    #[error("{}", .0.join("; "))]
    Invalid(Vec<String>),
    /// The queue is full, or too many requests came this hour.
    #[error("too many requests are waiting")]
    TooMany,
    /// No such request.
    #[error("request not found")]
    NotFound,
    /// Creating the account failed.
    #[error(transparent)]
    Account(#[from] AccountError),
    /// The database failed.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Account requests.
#[derive(Clone)]
pub struct Signups {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl Signups {
    /// Requests in `pool`.
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    /// Records a request for an account named `name`, owned by `email`.
    /// `Ok` also when the address already logs in, so the answer doesn't
    /// reveal who has an account; such a request isn't kept.
    pub async fn request(&self, name: &str, email: &str, accepted_terms: bool) -> Result<(), SignupError> {
        let (name, email) = (name.trim(), email.trim());
        let mut problems = Vec::new();
        if let Err(problem) = check_name(name) {
            problems.push(format!("Name: {problem}"));
        }
        if !is_email_address(email) {
            problems.push(format!("Email: {email:?} is not an email address"));
        }
        if !accepted_terms {
            problems.push("Accept the terms".to_owned());
        }
        if !problems.is_empty() {
            return Err(SignupError::Invalid(problems));
        }
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        // One request at a time, so the limits hold under parallel requests.
        sqlx::query!("lock table account_signup_requests in share row exclusive mode")
            .execute(&mut *tx)
            .await?;
        sqlx::query!(
            "delete from account_signup_requests where created_at < $1",
            now - Duration::days(EXPIRES_AFTER_DAYS)
        )
        .execute(&mut *tx)
        .await?;
        let counts = sqlx::query!(
            r#"select count(*) as "pending!", count(*) filter (where created_at > $1) as "last_hour!"
               from account_signup_requests"#,
            now - Duration::hours(1),
        )
        .fetch_one(&mut *tx)
        .await?;
        // Checked before anything about the address, so a full queue answers
        // members and strangers alike.
        if counts.pending >= MAX_PENDING || counts.last_hour >= MAX_PER_HOUR {
            return Err(SignupError::TooMany);
        }
        let member = sqlx::query_scalar!(
            r#"select exists (
                   select 1 from account_members m join accounts a on a.tenant_id = m.tenant_id
                   where a.closed_at is null and lower(m.email) = lower($1)
               ) as "member!""#,
            email
        )
        .fetch_one(&mut *tx)
        .await?;
        if !member {
            sqlx::query!(
                "insert into account_signup_requests (id, name, email, accepted_at, created_at)
                 values ($1, $2, $3, $4, $4)
                 on conflict (lower(email)) do nothing",
                RequestId::new().as_uuid(),
                name,
                email,
                now,
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Every request waiting, oldest first.
    pub async fn pending(&self) -> Result<Vec<AccountRequest>, SignupError> {
        let rows = sqlx::query!(
            "select id, name, email, created_at from account_signup_requests
             where created_at >= $1 order by created_at, id",
            self.clock.now() - Duration::days(EXPIRES_AFTER_DAYS),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| AccountRequest {
                id: row.id.into(),
                name: row.name,
                email: row.email,
                requested_at: row.created_at,
            })
            .collect())
    }

    /// Creates the account a request asked for and removes the request.
    pub async fn approve(&self, id: RequestId) -> Result<(TenantId, AccountRequest), SignupError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query!(
            "select name, email, created_at from account_signup_requests where id = $1 for update",
            id.as_uuid()
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SignupError::NotFound)?;
        let request = AccountRequest {
            id,
            name: row.name,
            email: row.email,
            requested_at: row.created_at,
        };
        let tenant = create_in(&mut tx, self.clock.now(), &request.name, &request.email).await?;
        sqlx::query!("delete from account_signup_requests where id = $1", id.as_uuid())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((tenant, request))
    }

    /// Removes a request without creating anything.
    pub async fn decline(&self, id: RequestId) -> Result<(), SignupError> {
        let deleted = sqlx::query!("delete from account_signup_requests where id = $1", id.as_uuid())
            .execute(&self.pool)
            .await?;
        if deleted.rows_affected() == 1 {
            Ok(())
        } else {
            Err(SignupError::NotFound)
        }
    }

    /// Removes every waiting request, returning how many there were.
    pub async fn decline_all(&self) -> Result<u64, SignupError> {
        let deleted = sqlx::query!("delete from account_signup_requests")
            .execute(&self.pool)
            .await?;
        Ok(deleted.rows_affected())
    }
}
