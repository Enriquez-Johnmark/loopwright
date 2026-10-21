//! Inbound email (G1): a raw message reduced to the few facts the engine
//! uses. All of it is untrusted: addresses are lowercased, the text is cut
//! to a fixed length, and attachments are only bytes, never opened here.

use hmac::{Hmac, KeyInit, Mac};
use mail_parser::{Message, MessageParser};
use sha2::{Digest, Sha256};

/// The largest message accepted, in bytes (the inbound provider's limit).
pub const MAX_RAW_BYTES: usize = 25 * 1024 * 1024;
/// The most text kept from a message's body.
pub const MAX_TEXT_CHARS: usize = 20_000;
/// The longest Message-ID kept as a key.
const MAX_KEY_CHARS: usize = 300;

/// Why a message wasn't read.
#[derive(Debug, thiserror::Error)]
pub enum InboundError {
    /// Not a readable email, or no sender address.
    #[error("not a readable email")]
    Unreadable,
    /// Over the size limit.
    #[error("the email is too large")]
    TooLarge,
}

/// One inbound email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEmail {
    /// Tells a redelivery of the same email apart from a new one: its
    /// Message-ID, or else a hash of the whole message.
    pub key: String,
    /// The sender's address, lowercased.
    pub from: String,
    /// Every address it was sent to (To, Cc, Delivered-To, X-Original-To), lowercased.
    pub recipients: Vec<String>,
    /// The subject.
    pub subject: String,
    /// The plain text, at most [`MAX_TEXT_CHARS`].
    pub text: String,
    /// Whether it was sent automatically (an out-of-office reply, a list).
    pub automatic: bool,
    /// The attached files' bytes.
    pub attachments: Vec<Vec<u8>>,
}

/// Whether `signature` (hex) is the HMAC-SHA256 of `body` under `secret`,
/// compared in constant time: how the inbound provider proves a message
/// came from it.
pub fn signed(secret: &[u8], body: &[u8], signature: &str) -> bool {
    let Some(signature) = from_hex(signature.trim()) else {
        return false;
    };
    let Ok(mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(secret) else {
        return false;
    };
    !secret.is_empty() && mac.chain_update(body).verify_slice(&signature).is_ok()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// Reads `raw`, a whole message as received.
pub fn parse(raw: &[u8]) -> Result<InboundEmail, InboundError> {
    if raw.len() > MAX_RAW_BYTES {
        return Err(InboundError::TooLarge);
    }
    let message = MessageParser::default().parse(raw).ok_or(InboundError::Unreadable)?;
    let from = message
        .from()
        .and_then(|from| from.first())
        .and_then(|from| from.address())
        .map(|address| address.trim().to_ascii_lowercase())
        .filter(|address| address.contains('@'))
        .ok_or(InboundError::Unreadable)?;
    let mut recipients: Vec<String> = [message.to(), message.cc()]
        .into_iter()
        .flatten()
        .flat_map(|list| list.iter())
        .filter_map(|addr| addr.address())
        .map(str::to_ascii_lowercase)
        .collect();
    for name in ["Delivered-To", "X-Original-To"] {
        if let Some(value) = message.header_raw(name) {
            recipients.push(value.trim().trim_matches(['<', '>']).to_ascii_lowercase());
        }
    }
    let subject = message.subject().unwrap_or_default().trim().to_owned();
    let text = message
        .body_text(0)
        .map(|text| text.chars().take(MAX_TEXT_CHARS).collect())
        .unwrap_or_default();
    let key = match message.message_id().map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => format!("id:{}", id.chars().take(MAX_KEY_CHARS).collect::<String>()),
        None => format!(
            "sha256:{}",
            Sha256::digest(raw)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ),
    };
    Ok(InboundEmail {
        key,
        automatic: is_automatic(&message, &subject),
        attachments: message
            .attachments()
            .filter(|part| !part.is_message())
            .map(|part| part.contents().to_vec())
            .collect(),
        from,
        recipients,
        subject,
        text,
    })
}

/// Generous on purpose: a human reply taken for an automatic one only means
/// reminders continue, while the reverse would stop them wrongly.
fn is_automatic(message: &Message<'_>, subject: &str) -> bool {
    let header = |name: &str| message.header_raw(name).map(|value| value.trim().to_ascii_lowercase());
    let auto_submitted =
        header("Auto-Submitted").is_some_and(|value| value.split(';').next().unwrap_or_default().trim() != "no");
    let precedence =
        header("Precedence").is_some_and(|value| matches!(value.as_str(), "bulk" | "list" | "junk" | "auto_reply"));
    let marked = ["X-Autoreply", "X-Autorespond", "List-Id"]
        .into_iter()
        .any(|name| header(name).is_some());
    let subject = subject.to_ascii_lowercase();
    let out_of_office = ["automatic reply", "out of office", "auto:"]
        .into_iter()
        .any(|prefix| subject.starts_with(prefix));
    auto_submitted || precedence || marked || out_of_office
}
