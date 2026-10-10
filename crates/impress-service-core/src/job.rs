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

#[cfg(test)]
mod tests {
    use super::*;

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
