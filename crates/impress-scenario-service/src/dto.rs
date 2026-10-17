//! Wire types the scenario verbs take and return (S1). Every result is
//! snake_case with `wire_version` (`impress_service_core::wire`); every
//! refusal is `ok: false` with a `code`/`message`.

use impress_service_core::wire::wire_version;
use impress_service_core::Refusal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The refusal half every result DTO below carries: `ok: false` with a
/// `code`/`message` (`impress_service_core::refusal`), never a bare parse
/// failure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Refusal2 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl From<Refusal> for Refusal2 {
    fn from(r: Refusal) -> Self {
        Self {
            code: Some(r.code),
            message: Some(r.message),
        }
    }
}

/// A scenario spec as an argument, read by the verb rather than the
/// argument parser — a structural mistake comes back as a located problem
/// (`scenario-service_validate`) rather than a bare parse failure.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct SpecArg(pub Value);

impl SpecArg {
    pub fn parse(&self) -> Result<impress_scenario::Scenario, String> {
        serde_json::from_value(self.0.clone()).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScenarioValidateResult {
    pub wire_version: u32,
    pub ok: bool,
    pub problems: Vec<ProblemDto>,
    #[serde(flatten)]
    pub refusal: Refusal2,
}

impl ScenarioValidateResult {
    pub fn of(problems: Vec<impress_scenario::Problem>) -> Self {
        Self {
            wire_version: wire_version(),
            ok: problems.is_empty(),
            problems: problems.into_iter().map(ProblemDto::from).collect(),
            refusal: Refusal2::default(),
        }
    }

    pub fn refused(r: Refusal) -> Self {
        Self {
            wire_version: wire_version(),
            ok: false,
            problems: Vec::new(),
            refusal: r.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProblemDto {
    pub step: Option<usize>,
    pub message: String,
}

impl From<impress_scenario::Problem> for ProblemDto {
    fn from(p: impress_scenario::Problem) -> Self {
        Self {
            step: p.step,
            message: p.message,
        }
    }
}

/// One stored scenario, spec included.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScenarioResult {
    pub wire_version: u32,
    pub ok: bool,
    pub id: String,
    pub scenario_id: String,
    pub tags: Vec<String>,
    pub revision: u64,
    pub spec: Value,
    #[serde(flatten)]
    pub refusal: Refusal2,
}

impl ScenarioResult {
    pub fn from_row(row: &crate::store::ScenarioRow) -> Self {
        Self {
            wire_version: wire_version(),
            ok: true,
            id: row.id.to_string(),
            scenario_id: row.spec.id.clone(),
            tags: row.tags.clone(),
            revision: 1,
            spec: serde_json::to_value(&row.spec).unwrap_or(Value::Null),
            refusal: Refusal2::default(),
        }
    }

    pub fn refused(r: Refusal) -> Self {
        Self {
            wire_version: wire_version(),
            ok: false,
            id: String::new(),
            scenario_id: String::new(),
            tags: Vec::new(),
            revision: 0,
            spec: Value::Null,
            refusal: r.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScenarioListResult {
    pub wire_version: u32,
    pub ok: bool,
    pub scenarios: Vec<ScenarioSummaryDto>,
    #[serde(flatten)]
    pub refusal: Refusal2,
}

/// A recorded scenario and an explicit accounting of omitted calls.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScenarioRecordResult {
    #[serde(flatten)]
    pub scenario: ScenarioResult,
    pub selected: usize,
    pub skipped: Vec<RecordedCallOmission>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecordedCallOmission {
    pub call_id: String,
    pub verb: String,
    pub reason: String,
}

impl From<crate::record::SkippedCall> for RecordedCallOmission {
    fn from(call: crate::record::SkippedCall) -> Self {
        Self {
            call_id: call.call_id,
            verb: call.verb,
            reason: call.reason,
        }
    }
}

impl ScenarioRecordResult {
    pub fn refused(refusal: Refusal) -> Self {
        Self {
            scenario: ScenarioResult::refused(refusal),
            selected: 0,
            skipped: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScenarioSummaryDto {
    pub id: String,
    pub scenario_id: String,
    pub description: String,
    pub tier: String,
    pub tags: Vec<String>,
}

/// One `scenario-service_run` outcome — the shared report shape (SC-1).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScenarioRunResult {
    pub wire_version: u32,
    pub ok: bool,
    pub results: Vec<impress_service_core::report::CapabilityResult>,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub duration_ms: u64,
    #[serde(flatten)]
    pub refusal: Refusal2,
}

impl From<impress_service_core::report::SelfTestReport> for ScenarioRunResult {
    fn from(report: impress_service_core::report::SelfTestReport) -> Self {
        Self {
            wire_version: wire_version(),
            ok: report.ok,
            results: report.results,
            total: report.total,
            passed: report.passed,
            failed: report.failed,
            skipped: report.skipped,
            duration_ms: report.duration_ms,
            refusal: Refusal2::default(),
        }
    }
}

impl ScenarioRunResult {
    pub fn refused(r: Refusal) -> Self {
        Self {
            wire_version: wire_version(),
            ok: false,
            results: Vec::new(),
            total: 0,
            passed: 0,
            failed: 0,
            skipped: 0,
            duration_ms: 0,
            refusal: r.into(),
        }
    }
}
