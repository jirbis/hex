//! Intel macOS Labs: opt-in local text model, never a cloud endpoint.
//!
//! Hex's speech inference remains CPU-first on the tested Radeon Mac.
//! Text cleanup can instead use an independently accelerated llama.cpp
//! Vulkan/MoltenVK server or a local Ollama server. No request ever leaves
//! this computer: Hex's loopback resolver refuses off-machine addresses.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use color_eyre::eyre::{Result, bail, eyre, WrapErr};
use serde::Deserialize;
use serde_json::json;

const MAX_RESPONSE: u64 = 256 * 1024;
const MAX_INPUT: usize = 24 * 1024;
const DEFAULT_MODEL: &str = "local";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Api {
    Ollama,
    OpenAi,
}

struct Config {
    endpoint: String,
    model: String,
    api: Api,
}

/// No background service is started by Hex. Opt in by setting an endpoint
/// in the app's launch environment, and enabling its Labs switch.
pub(crate) fn configured() -> bool {
    std::env::var("HEX_LOCAL_AI_ENDPOINT")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

impl Config {
    fn read() -> Result<Self> {
        let endpoint = std::env::var("HEX_LOCAL_AI_ENDPOINT")
            .map_err(|_| eyre!("set HEX_LOCAL_AI_ENDPOINT for a local llama.cpp or Ollama server"))?;
        let endpoint = endpoint.trim();
        let url = url::Url::parse(endpoint).wrap_err("invalid HEX_LOCAL_AI_ENDPOINT")?;
        if url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
        {
            bail!("HEX_LOCAL_AI_ENDPOINT must be an HTTP loopback URL with no credentials or query");
        }
        let api = match url.path() {
            "/api/chat" => Api::Ollama,
            "/v1/chat/completions" => Api::OpenAi,
            _ => bail!("HEX_LOCAL_AI_ENDPOINT must end in /api/chat or /v1/chat/completions"),
        };
        let model = std::env::var("HEX_LOCAL_AI_MODEL")
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.into());
        if model.len() > 128 || model.chars().any(char::is_control) {
            bail!("HEX_LOCAL_AI_MODEL is not a valid model name");
        }
        if api == Api::Ollama && model == DEFAULT_MODEL {
            bail!("set HEX_LOCAL_AI_MODEL to an installed Ollama model, e.g. qwen2.5:3b");
        }
        Ok(Self {
            endpoint: endpoint.into(),
            model,
            api,
        })
    }
}

#[derive(Deserialize)]
struct Message {
    content: String,
}

#[derive(Deserialize)]
struct OllamaReply {
    message: Message,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct OpenAiReply {
    choices: Vec<Choice>,
}

/// One bounded, local-only model pass. Called from the dictation processing
/// worker (not the UI thread). The caller retains the original transcript
/// on timeout, cancellation, invalid JSON or an unacceptable rewrite.
pub(crate) fn generate_text(
    instructions: &str,
    transcript: &str,
    temperature: f64,
    deadline: Duration,
    cancelled: &AtomicBool,
) -> Result<String> {
    if cancelled.load(Ordering::Acquire) {
        bail!("local model request cancelled");
    }
    if transcript.len() > MAX_INPUT {
        bail!("transcript exceeds the local model input limit");
    }
    let config = Config::read()?;
    let messages = [
        json!({"role": "system", "content": instructions}),
        json!({"role": "user", "content": transcript}),
    ];
    let payload = match config.api {
        Api::Ollama => json!({
            "model": config.model,
            "messages": messages,
            "stream": false,
            "think": false,
            "options": {"temperature": temperature, "num_predict": 512},
        }),
        Api::OpenAi => json!({
            "model": config.model,
            "messages": messages,
            "stream": false,
            "temperature": temperature,
            "max_tokens": 512,
        }),
    };
    let body = serde_json::to_vec(&payload)?;
    let client = crate::net::loopback_agent(|builder| {
        builder
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_millis(500).min(deadline)))
            .timeout_recv_response(Some(deadline))
            .timeout_recv_body(Some(deadline))
            .timeout_send_body(Some(deadline))
    });
    let mut response = client
        .post(&config.endpoint)
        .header("Content-Type", "application/json")
        .send(body.as_slice())
        .map_err(|error| eyre!("local AI server: {}", crate::net::reason(&error)))?;
    let mut answer = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE + 1)
        .reader()
        .read_to_end(&mut answer)
        .wrap_err("could not read the local AI response")?;
    if answer.len() as u64 > MAX_RESPONSE {
        bail!("local AI response is too long");
    }
    if cancelled.load(Ordering::Acquire) {
        bail!("local model request cancelled");
    }
    let text = match config.api {
        Api::Ollama => serde_json::from_slice::<OllamaReply>(&answer)
            .wrap_err("invalid Ollama response")?
            .message
            .content,
        Api::OpenAi => serde_json::from_slice::<OpenAiReply>(&answer)
            .wrap_err("invalid llama.cpp response")?
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| eyre!("local AI server returned no choices"))?
            .message
            .content,
    };
    if text.trim().is_empty() {
        bail!("local AI server returned empty text");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_chat_endpoints_are_allowed() {
        // Tests the same URL constraints without changing process environment.
        for endpoint in [
            "https://127.0.0.1:8080/v1/chat/completions",
            "http://192.168.1.50:8080/v1/chat/completions",
            "http://127.0.0.1:8080/unsafe",
            "http://user:pass@localhost:11434/api/chat",
        ] {
            let url = url::Url::parse(endpoint).unwrap();
            assert!(
                url.scheme() != "http"
                    || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || !matches!(url.path(), "/api/chat" | "/v1/chat/completions"),
                "{endpoint} should be refused"
            );
        }
    }
}
