//! Wire-protocol clients. Each implements [`crate::InferenceProvider`]; the
//! catalogue decides which one a provider id gets.

pub mod anthropic;
pub mod openai_compat;

use reqwest::header::RETRY_AFTER;

use crate::Error;

pub use anthropic::AnthropicClient;
pub use openai_compat::OpenAiCompatibleClient;

/// Sampling knobs arrive as `f32`; widening them naively serialises `0.3` as
/// `0.30000001192092896`, which is ugly in requests and in run provenance.
pub(crate) fn sampling_value(value: f32) -> serde_json::Value {
    serde_json::json!(((f64::from(value)) * 1_000_000.0).round() / 1_000_000.0)
}

/// Shared HTTP client configuration: a short connect timeout so an absent
/// local host fails fast, a long overall timeout so a slow local model can
/// finish.
pub(crate) fn http_client() -> crate::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15 * 60))
        .build()?)
}

/// Classify a failed `send()`: connection-level failures are `Unreachable`,
/// everything else is a provider failure without a status.
pub(crate) fn transport_error(provider: &str, error: reqwest::Error) -> Error {
    if error.is_connect() || error.is_timeout() || error.is_request() {
        Error::Unreachable {
            provider: provider.to_string(),
            message: error.to_string(),
        }
    } else {
        Error::Provider {
            provider: provider.to_string(),
            status: None,
            message: error.to_string(),
        }
    }
}

/// Map a non-success HTTP response to the shared error vocabulary, consuming
/// the body for the detail message.
pub(crate) async fn status_error(provider: &str, response: reqwest::Response) -> Error {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let body = response.text().await.unwrap_or_default();
    let message = extract_message(&body).unwrap_or_else(|| {
        if body.trim().is_empty() {
            status
                .canonical_reason()
                .unwrap_or("request failed")
                .to_string()
        } else {
            body.chars().take(500).collect()
        }
    });
    match status.as_u16() {
        401 | 403 => Error::Unauthorized {
            provider: provider.to_string(),
            message,
        },
        429 => Error::RateLimited {
            provider: provider.to_string(),
            retry_after_secs: retry_after,
        },
        code => Error::Provider {
            provider: provider.to_string(),
            status: Some(code),
            message,
        },
    }
}

/// Pull the human-readable message out of the usual `{"error": {"message"}}`
/// / `{"error": "…"}` / `{"detail": "…"}` envelopes.
fn extract_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let candidates = [
        value.pointer("/error/message"),
        value.get("error"),
        value.get("detail"),
        value.get("message"),
    ];
    let message = candidates
        .into_iter()
        .flatten()
        .find_map(|candidate| match candidate {
            serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
            serde_json::Value::Object(_) => Some(candidate.to_string()),
            _ => None,
        });
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_messages_from_common_error_envelopes() {
        assert_eq!(
            extract_message(r#"{"error":{"message":"bad key","type":"auth"}}"#).as_deref(),
            Some("bad key")
        );
        assert_eq!(
            extract_message(r#"{"error":"plain"}"#).as_deref(),
            Some("plain")
        );
        assert_eq!(
            extract_message(r#"{"detail":"Not Found"}"#).as_deref(),
            Some("Not Found")
        );
        assert_eq!(extract_message("not json"), None);
    }
}
