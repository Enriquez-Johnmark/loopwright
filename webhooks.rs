//! Delivery events from the email provider (G3): Resend's webhooks, signed
//! the Svix way. A request counts only if its signature is right and its
//! timestamp recent, so an old delivery can't be replayed.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use time::OffsetDateTime;

/// How far a delivery's timestamp may be from now.
pub const TOLERANCE_SECONDS: i64 = 5 * 60;

/// The signature headers, as sent.
#[derive(Debug, Clone, Copy)]
pub struct Signature<'a> {
    /// `svix-id`.
    pub id: &'a str,
    /// `svix-timestamp`, in Unix seconds.
    pub timestamp: &'a str,
    /// `svix-signature`: space-separated `v1,<base64>` entries.
    pub signatures: &'a str,
}

/// Whether `body` was signed with `secret` (`whsec_<base64>`) at a time
/// within [`TOLERANCE_SECONDS`] of `now`.
pub fn verified(secret: &str, signature: Signature<'_>, body: &[u8], now: OffsetDateTime) -> bool {
    let Ok(key) = STANDARD.decode(secret.trim().trim_start_matches("whsec_")) else {
        return false;
    };
    let Ok(sent_at) = signature.timestamp.trim().parse::<i64>() else {
        return false;
    };
    let recent = now
        .unix_timestamp()
        .checked_sub(sent_at)
        .is_some_and(|age| age.abs() <= TOLERANCE_SECONDS);
    if key.is_empty() || !recent {
        return false;
    }
    signature.signatures.split_whitespace().any(|entry| {
        let Some(given) = entry.strip_prefix("v1,").and_then(|b64| STANDARD.decode(b64).ok()) else {
            return false;
        };
        let Ok(mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(&key) else {
            return false;
        };
        mac.chain_update(signature.id.as_bytes())
            .chain_update(b".")
            .chain_update(signature.timestamp.trim().as_bytes())
            .chain_update(b".")
            .chain_update(body)
            .verify_slice(&given)
            .is_ok()
    })
}

/// What the provider reports about one email it sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryEvent {
    /// It bounced; `permanent` unless the provider says it may yet arrive.
    Bounced {
        /// The provider's ID for the email (what its send call returned).
        provider_message_id: String,
        /// A hard bounce: the address doesn't take mail.
        permanent: bool,
        /// The provider's explanation.
        reason: String,
    },
    /// The recipient marked it as spam.
    Complained {
        /// The provider's ID for the email.
        provider_message_id: String,
    },
    /// Anything else (delivered, opened…): nothing to do.
    Other,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: String,
    data: PayloadData,
}

#[derive(Deserialize)]
struct PayloadData {
    /// Only events about an email carry it.
    email_id: Option<String>,
    bounce: Option<Bounce>,
}

#[derive(Deserialize)]
struct Bounce {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

/// Reads a verified webhook body; `None` if it isn't a delivery event.
pub fn parse(body: &[u8]) -> Option<DeliveryEvent> {
    let payload: Payload = serde_json::from_slice(body).ok()?;
    let id = payload.data.email_id;
    Some(match payload.kind.as_str() {
        "email.bounced" => {
            let id = id?;
            let bounce = payload.data.bounce;
            DeliveryEvent::Bounced {
                provider_message_id: id,
                // Resend sends `email.bounced` for permanent rejections; a
                // bounce it calls Transient or Undetermined only shows.
                permanent: bounce
                    .as_ref()
                    .and_then(|bounce| bounce.kind.as_deref())
                    .is_none_or(|kind| kind.eq_ignore_ascii_case("permanent")),
                reason: bounce.and_then(|bounce| bounce.message).unwrap_or_default(),
            }
        }
        "email.complained" => DeliveryEvent::Complained {
            provider_message_id: id?,
        },
        _ => DeliveryEvent::Other,
    })
}
