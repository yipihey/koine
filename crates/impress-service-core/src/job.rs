//! The job handle every long-running verb answers with (ADR-0034 D6).
//!
//! One shape, in the crate every `*-service` already depends on, so a
//! converted verb in any crate returns the same thing and a caller — MCP,
//! the CLI, a surface source, the profiler — reads every job the same way:
//!
//! ```json
//! { "ok": true, "job": { "id": "…", "kind": "imprint-project-service_project-build",
//!   "state": "running" }, "message": "…", "wire_version": 1 }
//! ```
//!
//! `job.id` is a `task@1.0.0` row (the kernel's handle, lifecycle and
//! review); the verbs that read it are `impel-service_job-status`,
//! `job-events`, `job-wait`, `job-cancel` and `job-result`. The store half
//! is `impress_core::job`; the runner is `impress_store_service::job`. This
//! module is deliberately store-free: it is the wire.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::refusal::Refusal;
use crate::wire::{wire_version, WIRE_VERSION};

/// The handle: which row, which verb, and the state it was in when the
/// handle was minted (`running`, since the verb starts the job before it
/// answers — a job that could not start is a refusal, not a handle).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobHandle {
    /// The `task@1.0.0` row id. Pass it to the `impel-service_job-*` verbs.
    pub id: String,
    /// The qualified verb name that started the job.
    pub kind: String,
    /// `running` at start; `job_status` reads the live value.
    pub state: String,
}

/// What a long-running verb answers: the handle, at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct JobStarted {
    pub ok: bool,
    /// The refusal code when `ok` is false (`store-unavailable`,
    /// `invalid-argument`, `not-found`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    /// Present exactly when `ok` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobHandle>,
    #[serde(default = "wire_version")]
    pub wire_version: u32,
}

impl JobStarted {
    /// A job that started. `message` should say what to do next
    /// (`job_wait {id}`), since an agent reads it before the schema.
    pub fn started(id: impl Into<String>, kind: impl Into<String>) -> Self {
        let kind = kind.into();
        let id = id.into();
        Self {
            ok: true,
            code: None,
            message: format!(
                "{kind} started as job {id}; poll impel-service_job-wait {{id, after_seq}} for \
                 progress, impel-service_job-result {{id}} for the result, \
                 impel-service_job-cancel {{id}} to stop it"
            ),
            job: Some(JobHandle {
                id,
                kind,
                state: "running".into(),
            }),
            wire_version: WIRE_VERSION,
        }
    }

    /// The verb could not even start the job.
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            code: Some(refusal.code),
            message: refusal.message,
            job: None,
            wire_version: WIRE_VERSION,
        }
    }

    /// The handle's id, when there is one — what a CLI `--wait` reads off
    /// any verb's result without knowing the verb.
    pub fn id_of(value: &serde_json::Value) -> Option<&str> {
        value.get("job")?.get("id")?.as_str()
    }
}

/// Where a running job's [`progress`] calls go. The runner builds one per
/// job (or per routine run) and runs the job inside [`PROGRESS`]`.scope`.
#[derive(Clone)]
pub struct ProgressSink(pub Arc<ProgressFn>);

/// What a [`ProgressSink`] calls: the fraction and the message.
pub type ProgressFn = dyn Fn(Option<f32>, &str) + Send + Sync;

impl ProgressSink {
    /// A sink that calls `f` for every report.
    pub fn new(f: impl Fn(Option<f32>, &str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
}

tokio::task_local! {
    /// The sink of the job this task is running, set by the runner around
    /// the verb (L-D3). Unset everywhere else.
    pub static PROGRESS: ProgressSink;
}

/// Report a long-running verb's progress: `fraction` in `0.0..=1.0` (clamped;
/// `None` when the verb cannot say how far along it is) and a short
/// `message`. The runner shows both on the job (`GET /api/job/<id>`,
/// `progress` and `progress_message`), at most twice a second.
///
/// A no-op outside a job: a verb calls it unconditionally, and a call that is
/// not running as a job (a form's Run, the CLI, a test) does nothing. The
/// scope is the task: work a verb hands to `tokio::spawn` or a blocking
/// thread does not carry it, so report from the verb's own task.
pub fn progress(fraction: Option<f32>, message: &str) {
    let fraction = fraction.map(|f| if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) });
    let _ = PROGRESS.try_with(|sink| (sink.0)(fraction, message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn progress_reaches_the_sink_inside_a_scope_and_nowhere_else() {
        use std::sync::Mutex;
        // Outside a job: a no-op, not a panic.
        progress(Some(0.1), "ignored");
        type Seen = Vec<(Option<f32>, String)>;
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let sink = {
            let seen = seen.clone();
            ProgressSink::new(move |f, m| seen.lock().unwrap().push((f, m.to_string())))
        };
        PROGRESS
            .scope(sink, async {
                progress(Some(0.5), "half");
                progress(Some(7.0), "clamped");
                progress(None, "unknown");
            })
            .await;
        progress(Some(0.9), "after");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (Some(0.5), "half".to_string()),
                (Some(1.0), "clamped".to_string()),
                (None, "unknown".to_string()),
            ]
        );
    }

    #[test]
    fn the_handle_is_the_documented_shape() {
        let started = JobStarted::started("abc", "x-service_slow");
        let json = serde_json::to_value(&started).unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["job"]["id"], "abc");
        assert_eq!(json["job"]["kind"], "x-service_slow");
        assert_eq!(json["job"]["state"], "running");
        assert_eq!(json["wire_version"], 1);
        assert!(json.get("code").is_none(), "no code on success");
        assert_eq!(JobStarted::id_of(&json), Some("abc"));
        let refused = serde_json::to_value(JobStarted::refused(Refusal::not_found("no"))).unwrap();
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["code"], "not-found");
        assert!(refused.get("job").is_none());
        assert_eq!(JobStarted::id_of(&refused), None);
    }
}
