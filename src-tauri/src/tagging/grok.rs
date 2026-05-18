//! xAI Grok API client. The endpoint is OpenAI-compatible at
//! https://api.x.ai/v1/chat/completions — same request/response shape as
//! OpenAI's chat completions, including the `messages` array and the
//! `choices[0].message.content` extraction path.
//!
//! Retries: 3 attempts on 5xx with exponential backoff (1s, 2s, 4s). On 429
//! we honor `Retry-After` if present, otherwise fall back to the same
//! backoff schedule. 4xx errors are fatal (caller's fault — bad key, malformed
//! body, etc.) so we don't waste attempts.
//!
//! Timeouts: 30s connect, 120s read. Grok responses for ~50-record batches
//! can take 30-60s on long inputs, so the read timeout is generous.

use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const API_URL: &str = "https://api.x.ai/v1/chat/completions";
const USER_AGENT: &str = "vam-package-browser/0.1";

pub struct GrokClient {
    agent: ureq::Agent,
    api_key: String,
    model: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<&'a serde_json::Value>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: AssistantMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    content: String,
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

#[derive(Debug)]
pub struct ChatResult {
    pub content: String,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
}

impl GrokClient {
    pub fn new(api_key: String, model: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(120))
            .user_agent(USER_AGENT)
            .build();
        Self {
            agent,
            api_key,
            model,
        }
    }

    /// Send a system+user message pair, get back the assistant's content
    /// string. Retries 5xx and 429 with exponential backoff; 4xx fails fast.
    ///
    /// `response_format` enables xAI's structured-output mode. With a strict
    /// JSON schema, the returned `content` is guaranteed to parse and match
    /// the schema. Pass None for free-form text.
    pub fn complete(
        &self,
        system: &str,
        user: &str,
        temperature: Option<f32>,
        response_format: Option<&serde_json::Value>,
    ) -> Result<ChatResult> {
        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: system,
                },
                ChatMessage {
                    role: "user",
                    content: user,
                },
            ],
            temperature,
            response_format,
            stream: false,
        };
        let body_json = serde_json::to_string(&body).context("serialize request body")?;

        let auth_header = format!("Bearer {}", self.api_key);
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..3 {
            if attempt > 0 {
                // Backoff before retry: 1s, 2s, 4s.
                let wait_secs = 1u64 << (attempt - 1);
                thread::sleep(Duration::from_secs(wait_secs));
            }

            let resp = self
                .agent
                .post(API_URL)
                .set("Authorization", &auth_header)
                .set("Content-Type", "application/json")
                .send_string(&body_json);

            match resp {
                Ok(r) => {
                    if r.status() == 200 {
                        let body = r
                            .into_string()
                            .context("read chat completion response body")?;
                        let parsed: ChatResponse = serde_json::from_str(&body)
                            .with_context(|| {
                                format!(
                                    "decode chat completion JSON (body head: {})",
                                    body.chars().take(200).collect::<String>()
                                )
                            })?;
                        let choice = parsed
                            .choices
                            .into_iter()
                            .next()
                            .ok_or_else(|| anyhow!("response had no choices"))?;
                        return Ok(ChatResult {
                            content: choice.message.content,
                            usage: parsed.usage,
                            finish_reason: choice.finish_reason,
                        });
                    } else {
                        // 2xx-but-not-200 — unexpected. Treat as fatal.
                        let status = r.status();
                        let body = r
                            .into_string()
                            .unwrap_or_else(|_| "<unreadable>".to_string());
                        return Err(anyhow!("unexpected http {status}: {body}"));
                    }
                }
                Err(ureq::Error::Status(status, r)) => {
                    let retry_after = r.header("Retry-After").and_then(|s| s.parse::<u64>().ok());
                    let body = r
                        .into_string()
                        .unwrap_or_else(|_| "<unreadable>".to_string());
                    let err = anyhow!("http {status}: {body}");
                    // 4xx (except 429) is fatal — caller's fault.
                    if status == 429 || (500..600).contains(&status) {
                        if let Some(secs) = retry_after {
                            thread::sleep(Duration::from_secs(secs.min(60)));
                        }
                        last_err = Some(err);
                        continue;
                    } else {
                        return Err(err);
                    }
                }
                Err(ureq::Error::Transport(t)) => {
                    last_err = Some(anyhow!("transport error: {t}"));
                    continue;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("grok request failed after 3 attempts")))
    }
}
