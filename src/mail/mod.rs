//! Sending email through a provider. The outbox is the only caller
//! (AGENTS.md: only `engine::outbox` sends messages).

mod mailpit;
mod resend;

use std::future::Future;

pub use mailpit::MailpitMailer;
pub use resend::ResendMailer;

/// One email's content. The outbox stores it, an approval binds to it, and
/// a provider sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    /// The address the email goes to.
    pub to_address: String,
    /// Display name shown as the sender, e.g. the Firm's name. The address
    /// itself is the mailer's configured address on our own domain.
    pub from_name: String,
    /// Where replies go.
    pub reply_to: Option<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body_text: String,
}

/// The provider's confirmation that it accepted an email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    /// The provider's ID for the email.
    pub provider_message_id: String,
}

/// Why a send didn't succeed. The variant says whether the email might
/// still have gone out, which decides whether a retry is safe (E1).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    /// The provider refused the email; it was not sent and retrying won't help.
    #[error("provider rejected the email: {0}")]
    Rejected(String),
    /// The email was certainly not sent (e.g. rate-limited, or the connection
    /// never opened); retrying is safe with any provider.
    #[error("email not sent, retry later: {0}")]
    NotSent(String),
    /// The outcome is unknown (timeout, server error): the email may have
    /// gone out. Only a provider that blocks duplicate keys can retry safely.
    #[error("send outcome unknown: {0}")]
    Unknown(String),
    /// The provider already holds an email under this key with different
    /// content: an earlier attempt reached it, so the email may have gone out.
    #[error("provider already has an email under this key: {0}")]
    KeyAlreadyUsed(String),
}

/// An email provider.
pub trait Mailer: Send + Sync {
    /// Whether the provider ignores a second send with the same idempotency
    /// key (Resend does, for 24 hours; Mailpit and SES don't).
    fn blocks_duplicate_keys(&self) -> bool;

    /// Sends one email. `idempotency_key` is the same on every attempt for
    /// the same outbox message.
    fn send(&self, email: &Email, idempotency_key: &str)
    -> impl Future<Output = Result<SendReceipt, MailError>> + Send;
}

/// The mailer chosen at start-up.
#[derive(Debug, Clone)]
pub enum ConfiguredMailer {
    /// Local development: Mailpit catches everything.
    Mailpit(MailpitMailer),
    /// Production: Resend.
    Resend(ResendMailer),
}

impl Mailer for ConfiguredMailer {
    fn blocks_duplicate_keys(&self) -> bool {
        match self {
            Self::Mailpit(m) => m.blocks_duplicate_keys(),
            Self::Resend(m) => m.blocks_duplicate_keys(),
        }
    }

    async fn send(&self, email: &Email, idempotency_key: &str) -> Result<SendReceipt, MailError> {
        match self {
            Self::Mailpit(m) => m.send(email, idempotency_key).await,
            Self::Resend(m) => m.send(email, idempotency_key).await,
        }
    }
}

/// Maps a transport error to whether the email could have gone out.
fn classify_transport_error(err: &reqwest::Error) -> MailError {
    if err.is_connect() || err.is_builder() {
        MailError::NotSent(err.to_string())
    } else {
        MailError::Unknown(err.to_string())
    }
}
