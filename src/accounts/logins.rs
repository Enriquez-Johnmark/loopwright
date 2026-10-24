//! Login by one-time email link, and sessions.
//!
//! - A link works once, for [`LINK_LIFETIME`]. The token table holds only
//!   its SHA-256 hash; the email carrying it stays in the outbox, and a
//!   draft of it that isn't approved is cancelled at once.
//! - The email goes through the outbox like every other message, approved
//!   automatically as [`LOGIN_APPROVER`], the one kind of message that may
//!   be. It always goes to the member's own address.
//! - An address with no open account gets no email; callers show the same
//!   screen either way.
//! - At most [`MAX_LINKS_PER_HOUR`] links per address, and
//!   [`MAX_ACCOUNT_LINKS_PER_HOUR`] per account, an hour.
//! - A session lasts [`SESSION_LIFETIME`], or until logout.

use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::Duration;

use super::{MemberId, Role};
use crate::audit::{self, AuditEvent};
use crate::clock::Clock;
use crate::ids::TenantId;
use crate::mail::Email;
use crate::outbox::{Outbox, OutboxError, PreSendCheck};

/// How long a login link works.
pub const LINK_LIFETIME: Duration = Duration::minutes(15);
/// How long a session lasts.
pub const SESSION_LIFETIME: Duration = Duration::days(30);
/// Login emails per address per hour.
pub const MAX_LINKS_PER_HOUR: i64 = 5;
/// Login emails per account per hour, across its members.
pub const MAX_ACCOUNT_LINKS_PER_HOUR: i64 = 30;
/// The approver recorded for login emails.
pub const LOGIN_APPROVER: &str = "system:login";

/// What a product needs to word a login email.
#[derive(Debug, Clone, Copy)]
pub struct LoginLink<'a> {
    /// The account the member logs in to.
    pub account_name: &'a str,
    /// The member's address; the email goes here whatever the product sets.
    pub to_address: &'a str,
    /// The secret to put in the product's login URL.
    pub token: &'a str,
}

/// Whether a login email was sent. Callers show the same screen either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginOutcome {
    /// A link is on its way.
    Sent,
    /// No open account for the address, or too many links this hour.
    NotSent,
}

/// Who a live session belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Their account.
    pub tenant: TenantId,
    /// The person.
    pub member: MemberId,
    /// Their address, which names them in the audit log.
    pub email: String,
    /// What they may do.
    pub role: Role,
}

/// Login errors: only infrastructure failures; a bad link is `Ok(None)`.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The database failed.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// The outbox refused the login email.
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    /// The system's random number source failed.
    #[error("no randomness available: {0}")]
    Random(String),
}

/// Logins and sessions.
#[derive(Clone)]
pub struct Logins {
    pool: PgPool,
    clock: Arc<dyn Clock>,
    outbox: Outbox,
}

impl Logins {
    /// Logins in `pool`.
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self {
            outbox: Outbox::new(pool.clone(), clock.clone()),
            pool,
            clock,
        }
    }

    /// Emails a login link to `email` if it's the address of a member of an
    /// open account. `compose` words the email from the link; `checks` are
    /// the product's pre-send rules, run as for any message.
    pub async fn send_link(
        &self,
        email: &str,
        compose: impl FnOnce(&LoginLink<'_>) -> Email,
        checks: &dyn PreSendCheck,
    ) -> Result<LoginOutcome, LoginError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let Some(member) = sqlx::query!(
            "select m.id, m.tenant_id, m.email, a.name as account_name
             from account_members m join accounts a on a.tenant_id = m.tenant_id and a.closed_at is null
             where lower(m.email) = lower($1)",
            email.trim(),
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(LoginOutcome::NotSent);
        };
        let tenant = TenantId::from(member.tenant_id);
        // Locking the account serialises its requests, so parallel ones
        // can't all pass the limits before any of them is counted. Checked
        // open again under the lock, in case it closed since the lookup.
        let open = sqlx::query!(
            "select tenant_id from accounts where tenant_id = $1 and closed_at is null for update",
            tenant.as_uuid()
        )
        .fetch_optional(&mut *tx)
        .await?;
        if open.is_none() {
            return Ok(LoginOutcome::NotSent);
        }
        // Counted from the outbox, which keeps the emails of removed members
        // too: removing and adding an address again can't reset its limit.
        let recent = sqlx::query!(
            r#"select count(*) filter (where lower(to_address) = lower($2)) as "to_address!",
                      count(*) as "account!"
               from outbox_messages where tenant_id = $1 and approved_by = $4 and created_at > $3"#,
            tenant.as_uuid(),
            member.email,
            now - Duration::hours(1),
            LOGIN_APPROVER,
        )
        .fetch_one(&mut *tx)
        .await?;
        if recent.to_address >= MAX_LINKS_PER_HOUR || recent.account >= MAX_ACCOUNT_LINKS_PER_HOUR {
            tracing::warn!(%tenant, "login emails limited this hour");
            return Ok(LoginOutcome::NotSent);
        }
        let token = random_token()?;
        sqlx::query!(
            "insert into account_login_tokens (token_hash, tenant_id, member_id, created_at, expires_at)
             values ($1, $2, $3, $4, $5)",
            &hash(&token)[..],
            tenant.as_uuid(),
            member.id,
            now,
            now + LINK_LIFETIME,
        )
        .execute(&mut *tx)
        .await?;
        let link = LoginLink {
            account_name: &member.account_name,
            to_address: &member.email,
            token: &token,
        };
        let email = Email {
            to_address: member.email.clone(),
            ..compose(&link)
        };
        // The draft holds a working link, so it expires with the link.
        let id = self
            .outbox
            .create_draft_in(&mut tx, tenant, &email, Some(now + LINK_LIFETIME))
            .await?;
        tx.commit().await?;
        let approved = match self.outbox.get(tenant, id).await {
            Ok(draft) => {
                self.outbox
                    .approve(tenant, id, LOGIN_APPROVER, &draft.content_hash(), checks)
                    .await
            }
            Err(err) => Err(err),
        };
        if let Err(err) = approved {
            // Never leave a draft holding a working link where a person could
            // read, edit or approve it.
            if let Err(cancel) = self.outbox.cancel(tenant, id, LOGIN_APPROVER).await {
                tracing::error!(%tenant, %cancel, "cancelling an unapproved login email failed");
            }
            return Err(err.into());
        }
        Ok(LoginOutcome::Sent)
    }

    /// Uses up a login link and starts a session, returning the session's
    /// secret for the cookie. `None` if the link is unknown, used or expired,
    /// or its account is closed.
    pub async fn log_in(&self, token: &str) -> Result<Option<String>, LoginError> {
        let now = self.clock.now();
        let mut tx = self.pool.begin().await?;
        let Some(used) = sqlx::query!(
            "update account_login_tokens t set used_at = $2
             from accounts a, account_members m
             where t.token_hash = $1 and t.used_at is null and t.expires_at > $2
               and a.tenant_id = t.tenant_id and a.closed_at is null
               and m.id = t.member_id and m.tenant_id = t.tenant_id
             returning t.tenant_id, t.member_id, m.email",
            &hash(token)[..],
            now,
        )
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let session = random_token()?;
        sqlx::query!(
            "insert into account_sessions (session_hash, tenant_id, member_id, created_at, expires_at)
             values ($1, $2, $3, $4, $5)",
            &hash(&session)[..],
            used.tenant_id,
            used.member_id,
            now,
            now + SESSION_LIFETIME,
        )
        .execute(&mut *tx)
        .await?;
        let event = AuditEvent {
            tenant: used.tenant_id.into(),
            at: now,
            actor: &used.email,
            action: "account.logged_in",
            subject_type: "account_member",
            subject_id: used.member_id,
            details: json!({}),
        };
        audit::record(&mut *tx, &event).await?;
        tx.commit().await?;
        Ok(Some(session))
    }

    /// Who a live session belongs to; `None` if it is unknown, expired, or
    /// its account is closed.
    pub async fn session(&self, session: &str) -> Result<Option<Session>, LoginError> {
        let row = sqlx::query!(
            "select s.tenant_id, s.member_id, m.email, m.role
             from account_sessions s
             join accounts a on a.tenant_id = s.tenant_id and a.closed_at is null
             join account_members m on m.id = s.member_id and m.tenant_id = s.tenant_id
             where s.session_hash = $1 and s.expires_at > $2",
            &hash(session)[..],
            self.clock.now(),
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| Session {
            tenant: row.tenant_id.into(),
            member: row.member_id.into(),
            email: row.email,
            role: Role::parse(&row.role),
        }))
    }

    /// Ends a session.
    pub async fn log_out(&self, session: &str) -> Result<(), LoginError> {
        sqlx::query!(
            "delete from account_sessions where session_hash = $1",
            &hash(session)[..]
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// 32 random bytes as 64 lowercase hex digits.
fn random_token() -> Result<String, LoginError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| LoginError::Random(err.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn hash(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}
