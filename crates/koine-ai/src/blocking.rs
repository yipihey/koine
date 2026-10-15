//! Synchronous entry points for callers that run inside `spawn_blocking`
//! (impel's enrichment, memory and throughline executors).
//!
//! The work runs on a private runtime driven from a scoped helper thread,
//! so this is safe to call from a plain thread *and* from inside another
//! runtime's blocking pool; the calling thread simply waits, and the future
//! may borrow the caller's registry.

use std::sync::OnceLock;

use crate::provider::Completion;
use crate::registry::{AiRegistry, ResolveTarget, ResolvedTarget};
use crate::types::{ChatRequest, ModelMessage, Role};
use crate::{Error, Result};

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("impress-ai-blocking")
            .enable_all()
            .build()
            .expect("build the impress-ai blocking runtime")
    })
}

fn block_on<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send,
    T: Send,
{
    std::thread::scope(|scope| {
        scope
            .spawn(|| runtime().block_on(future))
            .join()
            .map_err(|_| Error::Invalid("impress-ai blocking call panicked".into()))?
    })
}

pub fn complete_sync(
    registry: &AiRegistry,
    target: &ResolveTarget,
    request: ChatRequest,
) -> Result<(ResolvedTarget, Completion)> {
    block_on(registry.complete(target, request))
}

/// One system prompt + one user prompt → the model's text.
pub fn complete_text_sync(
    registry: &AiRegistry,
    target: &ResolveTarget,
    system: Option<&str>,
    prompt: &str,
    max_tokens: u32,
    temperature: Option<f32>,
) -> Result<(ResolvedTarget, String)> {
    let mut messages = Vec::new();
    if let Some(system) = system.filter(|system| !system.trim().is_empty()) {
        messages.push(ModelMessage::text(Role::System, system));
    }
    messages.push(ModelMessage::text(Role::User, prompt));
    let request = ChatRequest {
        messages,
        max_tokens,
        temperature,
        ..Default::default()
    };
    let (resolved, completion) = complete_sync(registry, target, request)?;
    Ok((resolved, completion.content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::InMemoryCredentials;

    #[test]
    fn blocking_completion_reports_configuration_errors_without_a_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let registry = AiRegistry::for_app(directory.path(), InMemoryCredentials::new());
        let error = complete_text_sync(
            &registry,
            &ResolveTarget::provider("openai"),
            None,
            "hi",
            32,
            Some(0.0),
        )
        .unwrap_err();
        assert!(matches!(error, Error::NotConfigured { .. }), "{error}");
    }

    #[tokio::test]
    async fn blocking_call_works_from_inside_another_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let registry = AiRegistry::for_app(directory.path(), InMemoryCredentials::new());
        let result = tokio::task::spawn_blocking(move || {
            complete_text_sync(
                &registry,
                &ResolveTarget::provider("google"),
                None,
                "hi",
                32,
                None,
            )
        })
        .await
        .unwrap();
        assert!(matches!(result, Err(Error::NotConfigured { .. })));
    }
}
