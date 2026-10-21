//! Our own small client for the model provider, so the provider can be
//! swapped. The model gets no tools: it only returns text, as JSON matching
//! a schema the caller gives. Callers own what goes into a request; model
//! output is untrusted and must be parsed and checked by the caller.

use std::future::Future;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};

/// One request: a system prompt, one user message, and the JSON schema the
/// answer must match.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// Instructions.
    pub system: String,
    /// The task's data.
    pub user: String,
    /// JSON Schema for the answer.
    pub schema: Value,
    /// Most tokens the answer may use.
    pub max_tokens: u32,
}

/// Tokens a call used, for the cost log (J1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Prompt tokens.
    pub input_tokens: u32,
    /// Answer tokens.
    pub output_tokens: u32,
}

/// A complete answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmReply {
    /// The answer's text: JSON matching the schema, not yet checked.
    pub text: String,
    /// The model that answered.
    pub model: String,
    /// The provider's ID for the request.
    pub request_id: Option<String>,
    /// Tokens used.
    pub input_tokens: u32,
    /// Tokens produced.
    pub output_tokens: u32,
}

/// Why there is no usable answer. Callers fall back rather than retry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// No answer: the connection failed or timed out.
    #[error("model unavailable: {0}")]
    Unavailable(String),
    /// The provider answered with an error status.
    #[error("model API error {status}: {message}")]
    Api {
        /// HTTP status.
        status: u16,
        /// The provider's message.
        message: String,
        /// The provider's request ID.
        request_id: Option<String>,
    },
    /// The model stopped early (a refusal, or out of tokens). It still cost.
    #[error("model answer incomplete: {stop_reason}")]
    Incomplete {
        /// Why it stopped.
        stop_reason: String,
        /// Tokens it used.
        usage: Usage,
        /// The provider's request ID.
        request_id: Option<String>,
    },
    /// The provider's response couldn't be read.
    #[error("unreadable model response: {0}")]
    Malformed(String),
}

/// A model that answers [`LlmRequest`]s.
pub trait LanguageModel: Send + Sync {
    /// The model's ID, for the cost log.
    fn model(&self) -> &str;

    /// Asks once; no retries.
    fn complete(&self, request: &LlmRequest) -> impl Future<Output = Result<LlmReply, LlmError>> + Send;
}

/// The Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicClient {
    http: reqwest::Client,
    url: String,
    api_key: SecretString,
    model: String,
}

impl AnthropicClient {
    /// A client for `model` at `base_url` (normally `https://api.anthropic.com`)
    /// that gives up after `timeout`.
    pub fn new(base_url: &str, api_key: SecretString, model: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(timeout).build()?,
            url: format!("{}/v1/messages", base_url.trim_end_matches('/')),
            api_key,
            model: model.to_owned(),
        })
    }

    /// The request body. Thinking is turned off in the way each model
    /// accepts: these are short wording tasks, and thinking bills as output.
    fn body(&self, request: &LlmRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": request.max_tokens,
            "system": request.system,
            "messages": [{"role": "user", "content": request.user}],
            "output_config": {"format": {"type": "json_schema", "schema": request.schema}},
        });
        let thinking_off = match self.model.as_str() {
            model if model.starts_with("claude-sonnet-5-5") => Some("between_tools"),
            model if model.starts_with("claude-haiku") => Some("disabled"),
            _ => None,
        };
        if let Some(kind) = thinking_off {
            body["thinking"] = json!({"type": kind});
        }
        body
    }
}

#[derive(Deserialize)]
struct MessageResponse {
    model: String,
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    usage: ResponseUsage,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct ResponseUsage {
    input_tokens: u32,
    output_tokens: u32,
}

impl LanguageModel for AnthropicClient {
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: &LlmRequest) -> Result<LlmReply, LlmError> {
        let response = self
            .http
            .post(&self.url)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", "2023-06-01")
            .json(&self.body(request))
            .send()
            .await
            .map_err(|err| LlmError::Unavailable(err.to_string()))?;
        let status = response.status();
        let request_id = response
            .headers()
            .get("request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = response
            .bytes()
            .await
            .map_err(|err| LlmError::Unavailable(err.to_string()))?;
        if !status.is_success() {
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| status.to_string());
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
                request_id,
            });
        }
        let message: MessageResponse =
            serde_json::from_slice(&bytes).map_err(|err| LlmError::Malformed(err.to_string()))?;
        let usage = Usage {
            input_tokens: message.usage.input_tokens,
            output_tokens: message.usage.output_tokens,
        };
        let stop_reason = message.stop_reason.unwrap_or_default();
        if stop_reason != "end_turn" {
            return Err(LlmError::Incomplete {
                stop_reason,
                usage,
                request_id,
            });
        }
        let text = message
            .content
            .into_iter()
            .filter(|block| block.kind == "text")
            .map(|block| block.text)
            .collect();
        Ok(LlmReply {
            text,
            model: message.model,
            request_id,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        })
    }
}
