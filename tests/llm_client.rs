//! Seam 1 (M3): the AI client against a fake Anthropic server. No test
//! ever calls the real API.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use std::time::Duration;

use loopwright::llm::{AnthropicClient, LanguageModel, LlmError, LlmRequest};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer, model: &str) -> AnthropicClient {
    AnthropicClient::new(
        &server.uri(),
        SecretString::from("sk-ant-test"),
        model,
        Duration::from_millis(500),
    )
    .unwrap()
}

fn request() -> LlmRequest {
    LlmRequest {
        system: "You write reminder wording.".to_owned(),
        user: "Client: Nena's Store".to_owned(),
        schema: json!({"type": "object", "properties": {"subject": {"type": "string"}}, "required": ["subject"], "additionalProperties": false}),
        max_tokens: 1000,
    }
}

fn reply() -> serde_json::Value {
    json!({
        "id": "msg_01",
        "type": "message",
        "role": "assistant",
        "model": "claude-haiku-5-5",
        "content": [{"type": "text", "text": "{\"subject\":\"Hi\"}"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 420, "output_tokens": 95}
    })
}

#[tokio::test]
async fn haiku_requests_turn_thinking_off_and_ask_for_json_matching_the_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_partial_json(json!({
            "model": "claude-haiku-5-5",
            "max_tokens": 1000,
            "system": "You write reminder wording.",
            "messages": [{"role": "user", "content": "Client: Nena's Store"}],
            "thinking": {"type": "disabled"},
            "output_config": {"format": {"type": "json_schema", "schema": request().schema}},
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("request-id", "req_123")
                .set_body_json(reply()),
        )
        .expect(1)
        .mount(&server)
        .await;

    let answer = client(&server, "claude-haiku-5-5").complete(&request()).await.unwrap();

    assert_eq!(answer.text, r#"{"subject":"Hi"}"#);
    assert_eq!((answer.input_tokens, answer.output_tokens), (420, 95));
    assert_eq!(answer.request_id.as_deref(), Some("req_123"));
    assert_eq!(answer.model, "claude-haiku-5-5");
}

#[tokio::test]
async fn sonnet_turns_thinking_off_the_way_it_accepts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(
            json!({"model": "claude-sonnet-5-5", "thinking": {"type": "between_tools"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply()))
        .expect(1)
        .mount(&server)
        .await;

    client(&server, "claude-sonnet-5-5").complete(&request()).await.unwrap();
}

#[tokio::test]
async fn failures_come_back_as_errors_not_text() {
    let server = MockServer::start().await;
    let overloaded =
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}, "request_id": "req_9"});
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).set_body_json(overloaded))
        .mount(&server)
        .await;
    let err = client(&server, "claude-haiku-5-5")
        .complete(&request())
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Api { status: 529, .. }), "{err:?}");

    let slow = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(reply())
                .set_delay(Duration::from_secs(2)),
        )
        .mount(&slow)
        .await;
    let err = client(&slow, "claude-haiku-5-5")
        .complete(&request())
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Unavailable(_)), "{err:?}");

    let refused = MockServer::start().await;
    let mut refusal = reply();
    refusal["stop_reason"] = json!("refusal");
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(refusal))
        .mount(&refused)
        .await;
    let err = client(&refused, "claude-haiku-5-5")
        .complete(&request())
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Incomplete { ref stop_reason, .. } if stop_reason == "refusal"),
        "{err:?}"
    );
}
