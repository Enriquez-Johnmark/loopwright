//! Shared test helpers.

#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::sync::{Arc, Mutex};

use loopwright::clock::FixedClock;
use loopwright::mail::{Email, MailError, Mailer, SendReceipt};
use time::macros::datetime;

/// A clock fixed at 2026-10-21 09:00 UTC; tests move it forward by hand.
pub fn clock() -> Arc<FixedClock> {
    Arc::new(FixedClock::new(datetime!(2026-10-21 09:00 UTC)))
}

pub fn sample_email() -> Email {
    Email {
        to_address: "nena@example.test".to_owned(),
        from_name: "Santos Bookkeeping".to_owned(),
        reply_to: Some("reply@product.test".to_owned()),
        subject: "Kulang pa po ang September receipts".to_owned(),
        body_text: "Hi Nena, paki-upload po ang September receipts bago Oct 20.".to_owned(),
    }
}

/// A mailer that records every send instead of delivering it.
#[derive(Clone, Default)]
pub struct RecordingMailer {
    idempotent: bool,
    sent: Arc<Mutex<Vec<(Email, String)>>>,
    failures: Arc<Mutex<Vec<MailError>>>,
}

impl RecordingMailer {
    /// A mailer whose provider blocks duplicate sends with the same key, like Resend.
    pub fn idempotent() -> Self {
        Self {
            idempotent: true,
            ..Self::default()
        }
    }

    /// A mailer whose provider can't block duplicates, like Mailpit or SES.
    pub fn non_idempotent() -> Self {
        Self::default()
    }

    /// Fails the next call with `err`; calls after the queued failures succeed.
    pub fn then_fail(self, err: MailError) -> Self {
        self.failures.lock().unwrap().push(err);
        self
    }

    pub fn calls(&self) -> Vec<(Email, String)> {
        self.sent.lock().unwrap().clone()
    }
}

impl Mailer for RecordingMailer {
    fn blocks_duplicate_keys(&self) -> bool {
        self.idempotent
    }

    async fn send(&self, email: &Email, idempotency_key: &str) -> Result<SendReceipt, MailError> {
        self.sent
            .lock()
            .unwrap()
            .push((email.clone(), idempotency_key.to_owned()));
        let mut failures = self.failures.lock().unwrap();
        if !failures.is_empty() {
            return Err(failures.remove(0));
        }
        Ok(SendReceipt {
            provider_message_id: format!("fake-{idempotency_key}"),
        })
    }
}
