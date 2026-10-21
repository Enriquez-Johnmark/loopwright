//! Resend's HTTP API (`POST /emails`), with its `Idempotency-Key` header
//! so a retry within 24 hours can't send twice.

use std::time::Duration;

use reqwest::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use super::{Email, MailError, Mailer, SendReceipt, classify_transport_error};

/// Sends through Resend.
#[derive(Debug, Clone)]
pub struct ResendMailer {
    client: reqwest::Client,
    base_url: String,
    api_key: SecretString,
    from_address: String,
}

impl ResendMailer {
    /// A Resend client for `base_url` (normally `https://api.resend.com`),
    /// sending from `from_address`.
    pub fn new(base_url: &str, api_key: SecretString, from_address: &str) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            from_address: from_address.to_owned(),
        })
    }
}

#[derive(Serialize)]
struct ResendRequest<'a> {
    from: String,
    to: [&'a str; 1],
    subject: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<&'a str>,
}

#[derive(Deserialize)]
struct ResendResponse {
    id: String,
}

#[derive(Deserialize)]
struct ResendErrorBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    message: String,
}

impl Mailer for ResendMailer {
    fn blocks_duplicate_keys(&self) -> bool {
        true
    }

    async fn send(&self, email: &Email, idempotency_key: &str) -> Result<SendReceipt, MailError> {
        let body = ResendRequest {
            from: mailbox(&email.from_name, &self.from_address),
            to: [&email.to_address],
            subject: &email.subject,
            text: &email.body_text,
            reply_to: email.reply_to.as_deref(),
        };
        let response = self
            .client
            .post(format!("{}/emails", self.base_url))
            .bearer_auth(self.api_key.expose_secret())
            .header("Idempotency-Key", idempotency_key)
            .json(&body)
            .send()
            .await
            .map_err(|err| classify_transport_error(&err))?;

        let status = response.status();
        if status.is_success() {
            let parsed: ResendResponse = response
                .json()
                .await
                .map_err(|err| MailError::Unknown(format!("unreadable success response: {err}")))?;
            return Ok(SendReceipt {
                provider_message_id: parsed.id,
            });
        }
        let error: ResendErrorBody = response.json().await.unwrap_or(ResendErrorBody {
            name: String::new(),
            message: String::new(),
        });
        let detail = format!("{status} {} {}", error.name, error.message);
        Err(match status {
            // Another request with this key is still in flight: same email, try again shortly.
            StatusCode::CONFLICT if error.name == "concurrent_idempotent_requests" => MailError::Unknown(detail),
            StatusCode::CONFLICT if error.name == "invalid_idempotent_request" => MailError::KeyAlreadyUsed(detail),
            StatusCode::TOO_MANY_REQUESTS => MailError::NotSent(detail),
            s if s.is_server_error() => MailError::Unknown(detail),
            _ => MailError::Rejected(detail),
        })
    }
}

/// `"Name" <address>`, quoting the display name so commas or quotes in a
/// Firm's name can't change the address.
fn mailbox(name: &str, address: &str) -> String {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\" <{address}>")
}

#[cfg(test)]
mod tests {
    use super::mailbox;

    #[test]
    fn mailbox_quotes_a_name_containing_a_comma() {
        assert_eq!(
            mailbox("Santos, Reyes & Co", "r@k.test"),
            "\"Santos, Reyes & Co\" <r@k.test>"
        );
    }

    #[test]
    fn mailbox_escapes_quotes_in_the_name() {
        assert_eq!(
            mailbox("The \"Best\" Books", "r@k.test"),
            "\"The \\\"Best\\\" Books\" <r@k.test>"
        );
    }
}
