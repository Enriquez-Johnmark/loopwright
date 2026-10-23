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

/// Why a message didn't pass, in words for the person who approved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckFailed {
    /// The message breaks a rule; it must not go out as it is.
    Refused(String),
    /// The check itself couldn't run (say, a database timeout); try later.
    Unavailable(String),
}

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

/// Top-level domains a bare `name.tld/path` link would use. A list rather
/// than any word with a dot, so typos like "po.Salamat" aren't links.
const LINK_TLDS: [&str; 24] = [
    "com", "net", "org", "ph", "io", "co", "me", "app", "info", "biz", "xyz", "link", "site", "online", "store",
    "shop", "ly", "gl", "gg", "to", "dev", "page", "top", "cc",
];

/// The links in `text`: words, stripped of surrounding punctuation, that
/// have a scheme (`https://`), start with `www.`, or are a bare domain
/// such as `gcash-pay.ph/upload`. Email addresses aren't links.
pub fn links(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .map(|word| {
            word.trim_start_matches(['(', '<', '[', '"', '\''])
                .trim_end_matches([')', '>', ']', '"', '\'', '.', ',', ';', ':', '!', '?'])
        })
        .filter(|word| looks_like_link(word))
        .collect()
}

fn looks_like_link(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("www.") {
        return true;
    }
    if lower.contains('@') {
        return false;
    }
    let host = lower.split('/').next().unwrap_or_default();
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels
            .iter()
            .all(|label| !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|tld| LINK_TLDS.contains(tld))
}

/// The engine's rules, then the product's.
pub(super) async fn run(
    product: &dyn PreSendCheck,
    tenant: TenantId,
    id: MessageId,
    email: &Email,
) -> Result<(), CheckFailed> {
    if email.body_text.trim().is_empty() {
        return Err(CheckFailed::Refused("the message is empty".to_owned()));
    }
    if email.body_text.chars().count() > MAX_BODY_CHARS {
        return Err(CheckFailed::Refused(format!(
            "the message is longer than {MAX_BODY_CHARS} characters"
        )));
    }
    if links(&email.body_text).len() > 1 {
        return Err(CheckFailed::Refused("the message has more than one link".to_owned()));
    }
    product.check(tenant, id, email).await
}

#[cfg(test)]
mod tests {
    use super::links;

    #[test]
    fn d7_links_are_found_inside_punctuation_and_without_a_scheme() {
        for text in [
            "Upload here: https://getkompleto.test/u/abc",
            "Pay here (www.evil.ph/pay) po",
            "<http://x.example/a>",
            "I-upload sa gcash-pay.ph/upload.",
            "Visit bit.ly/abc123 now",
            "See Drive.Google.com",
        ] {
            assert_eq!(links(text).len(), 1, "{text}");
        }
    }

    #[test]
    fn d7_ordinary_words_dates_and_addresses_are_not_links() {
        let text = "Hi Nena, e.g. the Q3 2026 1601-EQ docs for Sari-Sari Store Inc. by 21.10.2026 po.Salamat! \
                    Reply to nena@example.test. Santos Bookkeeping";
        assert_eq!(links(text), Vec::<&str>::new());
    }
}
