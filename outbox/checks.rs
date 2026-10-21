//! Pre-send checks (D7). Every message is checked when it's approved and
//! again just before it's sent; failing either, it doesn't go out.
//!
//! The engine's own rules apply to every message. A product adds the rules
//! only it can judge (is this the Client's stored address? is this link
//! the Client's own?) by implementing [`PreSendCheck`], so the engine never
//! depends on the product (ADR 0001).

use std::future::Future;
use std::pin::Pin;

use crate::engine::ids::{MessageId, TenantId};
use crate::engine::mail::Email;

/// The longest body the outbox sends, in characters.
pub const MAX_BODY_CHARS: usize = 2_000;

/// Why a message may not go out, in words for the person who approved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckFailed(pub String);

/// A boxed future, so checks can be shared as `dyn PreSendCheck`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A product's own pre-send rules.
pub trait PreSendCheck: Send + Sync {
    /// `Ok` if `email`, message `id` of `tenant`, may go out.
    fn check<'a>(&'a self, tenant: TenantId, id: MessageId, email: &'a Email)
    -> BoxFuture<'a, Result<(), CheckFailed>>;
}

/// No product rules: only the engine's own apply. For tests and the demo.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoExtraChecks;

impl PreSendCheck for NoExtraChecks {
    fn check<'a>(&'a self, _: TenantId, _: MessageId, _: &'a Email) -> BoxFuture<'a, Result<(), CheckFailed>> {
        Box::pin(async { Ok(()) })
    }
}

/// The links in `text`: words containing `://` or starting with `www.`.
pub fn links(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .filter(|word| word.contains("://") || word.to_ascii_lowercase().starts_with("www."))
        .collect()
}

/// The engine's rules, then the product's.
pub(super) async fn run(
    product: &dyn PreSendCheck,
    tenant: TenantId,
    id: MessageId,
    email: &Email,
) -> Result<(), CheckFailed> {
    if email.body_text.trim().is_empty() {
        return Err(CheckFailed("the message is empty".to_owned()));
    }
    if email.body_text.chars().count() > MAX_BODY_CHARS {
        return Err(CheckFailed(format!(
            "the message is longer than {MAX_BODY_CHARS} characters"
        )));
    }
    if links(&email.body_text).len() > 1 {
        return Err(CheckFailed("the message has more than one link".to_owned()));
    }
    product.check(tenant, id, email).await
}
