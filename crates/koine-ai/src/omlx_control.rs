//! Starting the managed oMLX server on demand.
//!
//! oMLX.app owns a newline-delimited JSON control socket (the same protocol
//! its bundled `omlx start` command uses). Rust drives the socket and the
//! readiness wait; only launching the .app itself when the socket is absent
//! is left to the platform layer, which reports that with
//! [`Error::HostLaunchRequired`].

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::catalogue::{self, OMLX_BUNDLE_ID, OMLX_ID};
use crate::provider::{HealthState, ProviderHealth};
use crate::registry::AiRegistry;
use crate::{Error, Result};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const READINESS_POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

/// `~/Library/Application Support/oMLX/control.sock`, resolved from the
/// account's real home directory (sandboxed processes see a rewritten
/// `HOME`).
pub fn control_socket_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        home.join("Library")
            .join("Application Support")
            .join("oMLX")
            .join("control.sock")
    })
}

#[cfg(unix)]
pub async fn send_command(command: &str) -> Result<ControlResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let path = control_socket_path().ok_or_else(|| Error::NotConfigured {
        provider: OMLX_ID.into(),
        message: "no home directory to locate the oMLX control socket".into(),
    })?;
    let connect = tokio::net::UnixStream::connect(&path);
    let stream = tokio::time::timeout(CONTROL_TIMEOUT, connect)
        .await
        .map_err(|_| Error::HostLaunchRequired {
            bundle_id: OMLX_BUNDLE_ID.into(),
        })?
        .map_err(|_| Error::HostLaunchRequired {
            bundle_id: OMLX_BUNDLE_ID.into(),
        })?;
    let (reader, mut writer) = stream.into_split();
    let payload = format!("{}\n", serde_json::json!({ "command": command }));
    tokio::time::timeout(CONTROL_TIMEOUT, writer.write_all(payload.as_bytes()))
        .await
        .map_err(|_| control_error("timed out sending the command"))?
        .map_err(|error| control_error(format!("could not send the command: {error}")))?;
    let mut line = String::new();
    let mut reader = BufReader::new(reader);
    tokio::time::timeout(CONTROL_TIMEOUT, reader.read_line(&mut line))
        .await
        .map_err(|_| control_error("timed out waiting for the response"))?
        .map_err(|error| control_error(format!("could not read the response: {error}")))?;
    if line.trim().is_empty() {
        return Err(control_error(
            "oMLX closed the control socket without a response",
        ));
    }
    let response: ControlResponse = serde_json::from_str(line.trim())
        .map_err(|error| control_error(format!("invalid control response: {error}")))?;
    if !response.ok {
        return Err(control_error(
            response
                .message
                .clone()
                .unwrap_or_else(|| format!("oMLX rejected the {command} command")),
        ));
    }
    Ok(response)
}

#[cfg(not(unix))]
pub async fn send_command(_command: &str) -> Result<ControlResponse> {
    Err(Error::NotConfigured {
        provider: OMLX_ID.into(),
        message: "oMLX can only be controlled on a Unix host".into(),
    })
}

fn control_error(message: impl Into<String>) -> Error {
    Error::Provider {
        provider: OMLX_ID.into(),
        status: None,
        message: message.into(),
    }
}

/// Make sure the managed oMLX server answers, starting it when the
/// preferences allow and the endpoint is oMLX's own loopback address.
///
/// Errors: `NotConfigured` when automatic start is off, `Unreachable` when
/// the endpoint is not the managed one (the suite never claims another
/// runtime's port), `HostLaunchRequired` when the control socket is absent
/// and the platform layer must launch oMLX.app first.
pub async fn ensure_running(registry: &AiRegistry, timeout: Duration) -> Result<ProviderHealth> {
    let health = registry.health(OMLX_ID).await;
    if health.state.is_ready() {
        return Ok(health);
    }
    let endpoint = registry.endpoint_for(OMLX_ID).unwrap_or_default();
    if !catalogue::is_managed_omlx_endpoint(&endpoint) {
        return Err(Error::Unreachable {
            provider: OMLX_ID.into(),
            message: format!("{endpoint} is not the managed oMLX endpoint; not starting anything"),
        });
    }
    let auto_start = registry
        .preferences()
        .load()
        .map(|preferences| preferences.auto_start_omlx)
        .unwrap_or(true);
    if !auto_start {
        return Err(Error::NotConfigured {
            provider: OMLX_ID.into(),
            message: "oMLX is not running and automatic start is turned off".into(),
        });
    }
    send_command("start").await?;
    wait_until_ready(registry, timeout).await
}

/// Poll the health probe until the host reports ready or `timeout` passes.
pub async fn wait_until_ready(registry: &AiRegistry, timeout: Duration) -> Result<ProviderHealth> {
    let deadline = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < deadline {
        let health = registry.health(OMLX_ID).await;
        if health.state.is_ready() {
            return Ok(health);
        }
        last = Some(health);
        tokio::time::sleep(READINESS_POLL).await;
    }
    let detail = last
        .map(|health| health.detail)
        .unwrap_or_else(|| "no response from /v1/models".into());
    Err(Error::Unreachable {
        provider: OMLX_ID.into(),
        message: format!(
            "oMLX did not become ready within {} seconds: {detail}",
            timeout.as_secs()
        ),
    })
}

/// Whether the state counts as "the host is up" for callers that only have a
/// health record.
pub fn is_up(health: &ProviderHealth) -> bool {
    matches!(health.state, HealthState::Ready | HealthState::Empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::InMemoryCredentials;

    #[test]
    fn control_socket_lives_under_application_support() {
        let path = control_socket_path().unwrap();
        assert!(path.ends_with("Library/Application Support/oMLX/control.sock"));
    }

    #[tokio::test]
    async fn never_starts_anything_for_a_custom_endpoint_or_when_disabled() {
        let directory = tempfile::tempdir().unwrap();
        let registry = AiRegistry::for_app(directory.path(), InMemoryCredentials::new());
        registry
            .set_provider_endpoint(OMLX_ID, Some("http://127.0.0.1:1".into()))
            .unwrap();
        let error = ensure_running(&registry, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Unreachable { .. }), "{error}");

        registry.set_provider_endpoint(OMLX_ID, None).unwrap();
        registry.set_auto_start_omlx(false).unwrap();
        // Skip when a real oMLX answers on the default port: the probe would
        // report ready before the auto-start gate is consulted.
        if registry.health(OMLX_ID).await.state.is_ready() {
            return;
        }
        let error = ensure_running(&registry, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::NotConfigured { .. }), "{error}");
    }
}
