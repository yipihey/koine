//! The types that cross the wire. A result that can refuse carries `ok`,
//! and when refused only `ok`, `code` and `message` (docs/capability.md
//! § Refusing); the answer fields are a flattened `Option` beside it — the
//! shape `source-service` uses.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codes::Launch;

/// `ok`, and when refused `code` and `message`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outcome {
    /// False when the call was refused; then `code` and `message` say why.
    pub ok: bool,
    /// A stable kebab-case reason (`invalid-argument`, `not-found`,
    /// `conflict`, `unsupported`, `store-unavailable`), when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The reason in a sentence, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A refusal on its way to an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub code: &'static str,
    pub message: String,
}

impl Refused {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Refused {
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid-argument", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not-found", message)
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

macro_rules! refusable {
    ($(#[$doc:meta])* $name:ident, $answer:ty) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
        pub struct $name {
            #[serde(flatten)]
            pub outcome: Outcome,
            #[serde(flatten)]
            pub answer: Option<$answer>,
        }

        impl $name {
            pub fn answer(answer: $answer) -> Self {
                $name {
                    outcome: Outcome { ok: true, code: None, message: None },
                    answer: Some(answer),
                }
            }

            pub fn refused(code: &str, message: impl Into<String>) -> Self {
                $name {
                    outcome: Outcome { ok: false, code: Some(code.into()), message: Some(message.into()) },
                    answer: None,
                }
            }
        }

        impl From<Refused> for $name {
            fn from(r: Refused) -> Self {
                $name::refused(r.code, r.message)
            }
        }
    };
}

/// One registered code, as `codes` lists it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CodeSummary {
    pub name: String,
    pub version: String,
    /// The declared parameter names, sorted.
    pub parameters: Vec<String>,
    /// The groups the parameters fall in (`x-run.group`), first-seen order.
    pub groups: Vec<String>,
    /// The declared metric names (`x-run.metrics`).
    pub metrics: Vec<String>,
    /// The problem the code records when a start names none.
    pub default_problem: Value,
    pub launch: Option<Launch>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CodesAnswer {
    pub codes: Vec<CodeSummary>,
}

refusable!(
    /// Result of `codes` and `register-code`.
    CodesResult,
    CodesAnswer
);

/// A run as the store row says it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunRow {
    /// The run's id (`run.json` `id`, the row id).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The run directory, absolute.
    pub dir: String,
    /// `run.json` `code`: name, version, commit, dirty …
    pub code: Value,
    /// `run.json` `problem`.
    pub problem: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// `created`, `running`, `suspended`, `done`, `failed`, `cancelled`.
    pub state: String,
    /// R-D4.4: the run says `running` but the process this host launched
    /// is gone. Derived, never written into `run.json`.
    #[serde(default)]
    pub orphaned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// `{code, label, message?, invalidates_reuse?}` once terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<Value>,
    /// How far `events.jsonl` has been read into the ring.
    #[serde(default)]
    pub last_event_seq: u64,
    /// The last `progress` event (`step`, `time`, `dt`, `fraction`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Value>,
    /// The last value of each metric seen (`metric.values`, merged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<Value>,
    /// `set` requests on `restart` parameters, waiting for a resume:
    /// `[{param, value, control}]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs_restart: Vec<Value>,
    /// The sweep this run belongs to (R-D10), when koine started it in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep: Option<String>,
    /// The desk request a 3xx exit filed (R-D13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnose_request: Option<String>,
    /// The pid of the process this host launched, while it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IngestAnswer {
    pub run: RunRow,
    /// Events appended to the ring by this call.
    pub events_added: u64,
    /// `check_dir`'s findings, when an imported run was ingested anyway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,
}

refusable!(
    /// Result of `ingest`.
    IngestResult,
    IngestAnswer
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunsAnswer {
    pub runs: Vec<RunRow>,
}

refusable!(
    /// Result of `list`.
    RunsResult,
    RunsAnswer
);

/// One steering request and what became of it (R-D8).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ControlOutcome {
    /// The request id (the file's stem).
    pub id: String,
    /// `set`, `checkpoint`, `stop`, `pause`, `resume`, `step`, `until`.
    pub request: String,
    /// `pending` (still in `control/`), `applied` (`control/done/`) or
    /// `refused` (`control/refused/`).
    pub status: String,
    /// The code's reason, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The request as written.
    pub body: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusAnswer {
    pub run: RunRow,
    /// Every request in the inbox and its outcome, oldest first.
    #[serde(default)]
    pub controls: Vec<ControlOutcome>,
    /// `outputs.json`'s index.
    #[serde(default)]
    pub outputs: Vec<Value>,
    /// `notes.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<Value>,
}

refusable!(
    /// Result of `status`.
    StatusResult,
    StatusAnswer
);

/// One event from the ring.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventRow {
    pub seq: u64,
    /// The event's `kind` (R-D6.1).
    pub kind: String,
    /// When the ring row was written.
    pub at: String,
    /// The line as `events.jsonl` has it.
    pub event: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventsAnswer {
    pub events: Vec<EventRow>,
    /// Pass back as `after_seq` for the next page.
    pub cursor: u64,
    /// The ring was pruned past `after_seq`: events between it and the first
    /// row are gone from the ring (`events.jsonl` still has them).
    pub gap: bool,
}

refusable!(
    /// Result of `events`.
    EventsResult,
    EventsAnswer
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StartAnswer {
    pub run: RunRow,
    /// True when a `done` run with this key answered instead of a launch
    /// (R-D8 reuse).
    #[serde(default)]
    pub reused: bool,
    /// The key the start predicted and looked up.
    pub key: String,
}

refusable!(
    /// Result of `start` and `resume`.
    StartResult,
    StartAnswer
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ControlAnswer {
    pub run: RunRow,
    /// The request id; `status` reports whether it was applied or refused.
    pub control: String,
    pub request: Value,
}

refusable!(
    /// Result of `set`, `stop`, `checkpoint`.
    ControlResult,
    ControlAnswer
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SweepAnswer {
    /// The sweep's id (`sweep.json` `id`).
    pub sweep: String,
    /// The sweep directory, holding `sweep.json` and one run directory each.
    pub dir: String,
    /// The parent job: a `task@1.0.0` handle whose ring records each run
    /// as it starts and that finishes when the last one has.
    pub job: String,
    /// The `koine.verb` tasks filed, one `run-service_start` each.
    pub tasks: Vec<String>,
    /// How many runs the sweep expands to.
    pub planned: u32,
}

refusable!(
    /// Result of `sweep`.
    SweepResult,
    SweepAnswer
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormAnswer {
    /// The generated surface spec (`impress-surface`), grouped by `x-run.group`.
    pub spec: Value,
}

refusable!(
    /// Result of `form`.
    FormResult,
    FormAnswer
);
