//! Mailpit's send API (`POST /api/v1/send`), used in development so every
//! email lands in a local inbox instead of reaching anyone.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Email, MailError, Mailer, SendReceipt, classify_transport_error};

/// Sends into a local Mailpit.
#[derive(Debug, Clone)]
pub struct MailpitMailer {
    client: reqwest::Client,
    base_url: String,
    from_address: String,
}

impl MailpitMailer {
    /// A Mailpit client for `base_url` (e.g. `http://localhost:8025`),
    /// sending from `from_address`.
    pub fn new(base_url: &str, from_address: &str) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            from_address: from_address.to_owned(),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Address<'a> {
    email: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct MailpitRequest<'a> {
    from: Address<'a>,
    to: [Address<'a>; 1],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reply_to: Vec<Address<'a>>,
    subject: &'a str,
    text: &'a str,
    headers: Headers<'a>,
}

#[derive(Serialize)]
struct Headers<'a> {
    #[serde(rename = "X-Kompleto-Idempotency-Key")]
    idempotency_key: &'a str,
}

#[derive(Deserialize)]
struct MailpitResponse {
    #[serde(rename = "ID")]
    id: String,
}

impl Mailer for MailpitMailer {
    fn blocks_duplicate_keys(&self) -> bool {
        false
    }

    async fn send(&self, email: &Email, idempotency_key: &str) -> Result<SendReceipt, MailError> {
        let body = MailpitRequest {
            from: Address {
                email: &self.from_address,
                name: Some(&email.from_name),
            },
            to: [Address {
                email: &email.to_address,
                name: None,
            }],
            reply_to: email
                .reply_to
                .iter()
                .map(|r| Address { email: r, name: None })
                .collect(),
            subject: &email.subject,
            text: &email.body_text,
            headers: Headers { idempotency_key },
        };
        let response = self
            .client
            .post(format!("{}/api/v1/send", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|err| classify_transport_error(&err))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(if status.is_server_error() {
                MailError::Unknown(format!("{status} {text}"))
            } else {
                MailError::Rejected(format!("{status} {text}"))
            });
        }
        let parsed: MailpitResponse = response
            .json()
            .await
            .map_err(|err| MailError::Unknown(format!("unreadable success response: {err}")))?;
        Ok(SendReceipt {
            provider_message_id: parsed.id,
        })
    }
}
