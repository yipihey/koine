use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid AI request: {0}")]
    Invalid(String),
    #[error("oMLX is unavailable: {0}")]
    Omlx(String),
    #[error("{provider} is not configured: {message}")]
    NotConfigured { provider: String, message: String },
    #[error("{provider} rejected the credentials: {message}")]
    Unauthorized { provider: String, message: String },
    #[error("{provider} is rate limited{}", retry_suffix(.retry_after_secs))]
    RateLimited {
        provider: String,
        retry_after_secs: Option<u64>,
    },
    #[error("{provider} is unreachable: {message}")]
    Unreachable { provider: String, message: String },
    #[error("{provider} failed{}: {message}", status_suffix(.status))]
    Provider {
        provider: String,
        status: Option<u16>,
        message: String,
    },
    /// The provider is listed by the catalogue but executed by the host GUI
    /// (Apple on-device models); the Rust runtime cannot run it.
    #[error("{0} executes outside the Rust runtime")]
    ForeignExecutor(String),
    /// A managed local host must be launched by the platform layer (Launch
    /// Services) before the request can be retried.
    #[error("{bundle_id} must be launched by the host application")]
    HostLaunchRequired { bundle_id: String },
    #[error("shared store failed: {0}")]
    Store(String),
    #[error("content blob failed: {0}")]
    Blob(String),
    #[error("unsupported content: {0}")]
    UnsupportedContent(String),
    #[error("research context failed: {0}")]
    Web(String),
    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A legacy SQLite database (impress's LocalModels import) failed to
    /// read. Only with the `legacy-sqlite` feature.
    #[cfg(feature = "legacy-sqlite")]
    #[error("legacy database failed: {0}")]
    LegacyDatabase(#[from] rusqlite::Error),
}

impl Error {
    /// Whether a durable task should retry after this error. Transport and
    /// upstream-capacity failures are transient; credential, configuration
    /// and request-shape failures are not.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Omlx(_)
            | Self::Http(_)
            | Self::Io(_)
            | Self::Web(_)
            | Self::Unreachable { .. }
            | Self::RateLimited { .. } => true,
            // 408 (request timeout) and 429 (rate limit) are environmental
            // even when a path hands them over as a bare Provider status
            // instead of the dedicated RateLimited/Unreachable variants —
            // retrying is exactly what those statuses ask for.
            Self::Provider { status, .. } => {
                status.is_none_or(|status| status >= 500 || status == 408 || status == 429)
            }
            _ => false,
        }
    }
}

fn retry_suffix(retry_after_secs: &Option<u64>) -> String {
    retry_after_secs
        .map(|seconds| format!(" (retry after {seconds}s)"))
        .unwrap_or_default()
}

fn status_suffix(status: &Option<u16>) -> String {
    status
        .map(|status| format!(" with HTTP {status}"))
        .unwrap_or_default()
}

impl From<impress_item::store::StoreError> for Error {
    fn from(error: impress_item::store::StoreError) -> Self {
        Self::Store(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryability_follows_transport_versus_configuration() {
        assert!(Error::Unreachable {
            provider: "omlx".into(),
            message: "refused".into()
        }
        .is_retryable());
        assert!(Error::Provider {
            provider: "openai".into(),
            status: Some(503),
            message: "busy".into()
        }
        .is_retryable());
        assert!(!Error::Provider {
            provider: "openai".into(),
            status: Some(400),
            message: "bad".into()
        }
        .is_retryable());
        assert!(!Error::Unauthorized {
            provider: "anthropic".into(),
            message: "key".into()
        }
        .is_retryable());
        assert!(!Error::HostLaunchRequired {
            bundle_id: "app.omlx".into()
        }
        .is_retryable());
    }

    #[test]
    fn messages_include_retry_and_status_detail() {
        let limited = Error::RateLimited {
            provider: "openai".into(),
            retry_after_secs: Some(30),
        };
        assert_eq!(
            limited.to_string(),
            "openai is rate limited (retry after 30s)"
        );
        let failed = Error::Provider {
            provider: "google".into(),
            status: Some(502),
            message: "upstream".into(),
        };
        assert_eq!(failed.to_string(), "google failed with HTTP 502: upstream");
    }
}
