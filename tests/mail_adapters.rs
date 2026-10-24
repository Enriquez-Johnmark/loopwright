//! Seam 3: the exact requests our email adapters send, against a fake
//! provider, and how they read its answers.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use nudgewright::mail::{Email, MailError, Mailer, MailpitMailer, ResendMailer};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn email() -> Email {
    Email {
        to_address: "nena@example.test".to_owned(),
        from_name: "Santos, Reyes & Co".to_owned(),
        reply_to: Some("reply@product.test".to_owned()),
        subject: "Kulang pa po".to_owned(),
        body_text: "Paki-upload po.".to_owned(),
    }
}

fn resend(server: &MockServer) -> ResendMailer {
    ResendMailer::new(
        &server.uri(),
        SecretString::from("re_test_key"),
        "reminders@product.test",
    )
    .unwrap()
}

async fn resend_status(status: u16, body: serde_json::Value) -> MailError {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/emails"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    resend(&server).send(&email(), "key-1").await.unwrap_err()
}

#[tokio::test]
async fn resend_sends_the_idempotency_key_and_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/emails"))
        .and(header("Idempotency-Key", "0199f1e2-key"))
        .and(header("Authorization", "Bearer re_test_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "re_123" })))
        .expect(1)
        .mount(&server)
        .await;

    let receipt = resend(&server).send(&email(), "0199f1e2-key").await;

    assert!(receipt.is_ok(), "send failed: {receipt:?}");
}

#[tokio::test]
async fn resend_sends_from_our_address_under_the_quoted_firm_name() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/emails"))
        .and(body_partial_json(json!({
            "from": "\"Santos, Reyes & Co\" <reminders@product.test>",
            "to": ["nena@example.test"],
            "reply_to": "reply@product.test",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "re_123" })))
        .expect(1)
        .mount(&server)
        .await;

    let receipt = resend(&server).send(&email(), "k").await;

    assert!(receipt.is_ok(), "request shape didn't match: {receipt:?}");
}

#[tokio::test]
async fn resend_server_error_means_the_outcome_is_unknown() {
    let err = resend_status(500, json!({ "name": "internal_server_error", "message": "boom" })).await;

    assert!(matches!(err, MailError::Unknown(_)), "got {err:?}");
}

#[tokio::test]
async fn resend_rate_limit_means_not_sent() {
    let err = resend_status(429, json!({ "name": "rate_limit_exceeded", "message": "slow down" })).await;

    assert!(matches!(err, MailError::NotSent(_)), "got {err:?}");
}

#[tokio::test]
async fn resend_validation_error_means_rejected() {
    let err = resend_status(422, json!({ "name": "validation_error", "message": "bad to" })).await;

    assert!(matches!(err, MailError::Rejected(_)), "got {err:?}");
}

#[tokio::test]
async fn resend_concurrent_request_with_the_same_key_means_unknown() {
    let err = resend_status(
        409,
        json!({ "name": "concurrent_idempotent_requests", "message": "in flight" }),
    )
    .await;

    assert!(matches!(err, MailError::Unknown(_)), "got {err:?}");
}

#[tokio::test]
async fn resend_key_reused_with_different_content_means_key_already_used() {
    let err = resend_status(
        409,
        json!({ "name": "invalid_idempotent_request", "message": "payload differs" }),
    )
    .await;

    assert!(matches!(err, MailError::KeyAlreadyUsed(_)), "got {err:?}");
}

#[tokio::test]
async fn an_unreachable_provider_means_not_sent() {
    // Port 9 (discard) on localhost has nothing listening.
    let mailer = ResendMailer::new("http://127.0.0.1:9", SecretString::from("k"), "r@product.test").unwrap();

    let err = mailer.send(&email(), "k").await.unwrap_err();

    assert!(matches!(err, MailError::NotSent(_)), "got {err:?}");
}

#[tokio::test]
async fn mailpit_receives_sender_recipient_and_reply_to() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/send"))
        .and(body_partial_json(json!({
            "From": { "Email": "reminders@product.test", "Name": "Santos, Reyes & Co" },
            "To": [{ "Email": "nena@example.test" }],
            "ReplyTo": [{ "Email": "reply@product.test" }],
            "Subject": "Kulang pa po",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ID": "mp_1" })))
        .expect(1)
        .mount(&server)
        .await;
    let mailer = MailpitMailer::new(&server.uri(), "reminders@product.test").unwrap();

    let receipt = mailer.send(&email(), "k").await;

    assert!(receipt.is_ok(), "request shape didn't match: {receipt:?}");
}
