//! Intel macOS Labs: opt-in local text model, never a cloud endpoint.
//!
//! Hex's speech inference remains CPU-first on the tested Radeon Mac.
//! Text cleanup can instead use an independently accelerated llama.cpp
//! Vulkan/MoltenVK server or a local Ollama server. No request ever leaves
//! this computer: Hex's loopback resolver refuses off-machine addresses.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use color_eyre::eyre::{Result, WrapErr, bail, eyre};
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

/// Validate launch settings without contacting the server. Errors are safe
/// to display in Labs and do not contain transcripts or API credentials.
pub(crate) fn configuration_problem() -> Option<String> {
    configuration_status().err()
}

/// The model and loopback server Labs can name once the launch settings are
/// valid (`qwen2.5:3b · 127.0.0.1:11434`), or why they are not. Like
/// [`configuration_problem`], this never contacts the server, so "valid"
/// does not mean the server is running.
pub(crate) fn configuration_status() -> std::result::Result<String, String> {
    Config::read()
        .map(|config| describe(&config.model, &config.endpoint))
        .map_err(|error| error.to_string())
}

fn describe(model: &str, endpoint: &str) -> String {
    let server = url::Url::parse(endpoint)
        .ok()
        .and_then(|url| {
            let host = url.host_str()?.to_owned();
            Some(match url.port_or_known_default() {
                Some(port) => format!("{host}:{port}"),
                None => host,
            })
        })
        .unwrap_or_else(|| endpoint.to_owned());
    format!("{model} · {server}")
}

/// Share one URL parser between runtime configuration and security tests.
/// Parsing the typed host also supports IPv6 [::1] correctly.
fn parse_endpoint(endpoint: &str) -> Result<(String, Api)> {
    let url = url::Url::parse(endpoint).wrap_err("invalid HEX_LOCAL_AI_ENDPOINT")?;
    let loopback = match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("HEX_LOCAL_AI_ENDPOINT must be an HTTP loopback URL with no credentials or query");
    }
    let api = match url.path() {
        "/api/chat" => Api::Ollama,
        "/v1/chat/completions" => Api::OpenAi,
        _ => bail!("HEX_LOCAL_AI_ENDPOINT must end in /api/chat or /v1/chat/completions"),
    };
    Ok((url.to_string(), api))
}

impl Config {
    fn read() -> Result<Self> {
        let endpoint = std::env::var("HEX_LOCAL_AI_ENDPOINT").map_err(|_| {
            eyre!("set HEX_LOCAL_AI_ENDPOINT for a local llama.cpp or Ollama server")
        })?;
        let (endpoint, api) = parse_endpoint(endpoint.trim())?;
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
            endpoint,
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
            .timeout_global(Some(deadline))
            .timeout_connect(Some(Duration::from_millis(500).min(deadline)))
    });
    if cancelled.load(Ordering::Acquire) {
        bail!("local model request cancelled");
    }
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
        Api::Ollama => {
            serde_json::from_slice::<OllamaReply>(&answer)
                .wrap_err("invalid Ollama response")?
                .message
                .content
        }
        Api::OpenAi => {
            serde_json::from_slice::<OpenAiReply>(&answer)
                .wrap_err("invalid llama.cpp response")?
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("local AI server returned no choices"))?
                .message
                .content
        }
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
    fn accepts_valid_local_hosts_and_both_apis() {
        for endpoint in [
            "http://localhost:11434/api/chat",
            "http://127.0.0.1:8080/v1/chat/completions",
            "http://127.0.0.2:8080/v1/chat/completions",
            "http://[::1]:11434/api/chat",
        ] {
            let (_, api) = parse_endpoint(endpoint).expect(endpoint);
            if endpoint.ends_with("/api/chat") {
                assert_eq!(api, Api::Ollama);
            } else {
                assert_eq!(api, Api::OpenAi);
            }
        }
    }

    #[test]
    fn describes_the_model_and_server_without_the_path() {
        assert_eq!(
            describe("qwen2.5:3b", "http://127.0.0.1:11434/api/chat"),
            "qwen2.5:3b · 127.0.0.1:11434"
        );
        assert_eq!(
            describe("local", "http://[::1]:8080/v1/chat/completions"),
            "local · [::1]:8080"
        );
        assert_eq!(
            describe("local", "http://localhost/v1/chat/completions"),
            "local · localhost:80"
        );
    }

    #[test]
    fn rejects_remote_hosts_credentials_and_bad_protocols() {
        for endpoint in [
            "https://127.0.0.1:8080/v1/chat/completions",
            "http://192.168.1.50:8080/v1/chat/completions",
            "http://[::2]:8080/api/chat",
            "http://127.0.0.1:8080/unsafe",
            "http://user:pass@localhost:11434/api/chat",
            "http://localhost:11434/api/chat?token=secret",
            "http://localhost:11434/api/chat#fragment",
            "garbage",
        ] {
            assert!(
                parse_endpoint(endpoint).is_err(),
                "{endpoint} should be refused"
            );
        }
    }
}
