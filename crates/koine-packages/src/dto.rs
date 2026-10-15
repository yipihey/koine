//! What the package verbs answer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One installed package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PackageRow {
    pub name: String,
    pub version: String,
    pub digest: String,
    /// Where it was installed from (`builtin:<name>` for one in the binary).
    pub source: String,
    pub installed_at: String,
    /// How many surfaces it shipped.
    pub surfaces: u64,
    pub description: String,
}

/// `list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PackagesResult {
    pub ok: bool,
    pub packages: Vec<PackageRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One surface in a review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewSurface {
    pub name: String,
    pub file: String,
    pub valid: bool,
    /// Validation errors, `path: message`.
    pub problems: Vec<String>,
    /// The verbs it calls.
    pub verbs: Vec<String>,
    /// What installing would do to it.
    pub would: String,
}

/// One verb a package calls or requires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewVerb {
    pub name: String,
    /// Whether this host links it.
    pub linked: bool,
    pub safety: Option<String>,
    pub secrets: Vec<String>,
}

/// `inspect`: the review a person reads before `install`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ReviewResult {
    pub ok: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub source: String,
    /// Pass this to `install`: what you reviewed is what is installed.
    #[serde(default)]
    pub digest: String,
    /// Every surface validates.
    #[serde(default)]
    pub valid: bool,
    #[serde(default)]
    pub surfaces: Vec<ReviewSurface>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// The workflows it ships, each installed `proposed` (P-D9).
    #[serde(default)]
    pub workflows: Vec<ReviewWorkflow>,
    /// The AI task categories it declares.
    #[serde(default)]
    pub ai_categories: Vec<CategoryRow>,
    #[serde(default)]
    pub verbs: Vec<ReviewVerb>,
    /// Verbs it needs that this host does not link.
    #[serde(default)]
    pub missing_verbs: Vec<String>,
    /// Secrets its verbs need that are not set.
    #[serde(default)]
    pub missing_secrets: Vec<String>,
    /// Where the package is now: the directory inspected, or the bundle
    /// `fetch` kept. Pass it to `install` with `digest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One surface an install wrote or kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InstallRow {
    pub name: String,
    pub surface_id: String,
    /// `created`, `updated`, `unchanged`, `diverged` or `missing`.
    pub outcome: String,
}

/// One workflow a package ships, as the review shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewWorkflow {
    pub name: String,
    pub file: String,
    pub valid: bool,
    #[serde(default)]
    pub problems: Vec<String>,
    /// The verbs its steps call.
    #[serde(default)]
    pub verbs: Vec<String>,
    /// What starts it.
    #[serde(default)]
    pub trigger: serde_json::Value,
    /// `propose`, `keep (unchanged)`, `keep yours`, `propose the new one`,
    /// `leave deleted` or `refuse`.
    pub would: String,
}

/// An AI task category a package declares.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CategoryRow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub apps: Vec<String>,
}

/// What installing did to one workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InstallWorkflowRow {
    pub name: String,
    pub workflow_id: String,
    /// `proposed`, `unchanged`, `kept yours` or `left deleted`.
    pub outcome: String,
}

/// `install`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct InstallResult {
    pub ok: bool,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub surfaces: Vec<InstallRow>,
    /// Its workflows, each `proposed` until a person enables it (P-D9).
    #[serde(default)]
    pub workflows: Vec<InstallWorkflowRow>,
    /// The AI task categories it declared.
    #[serde(default)]
    pub ai_categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `remove`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RemoveResult {
    pub ok: bool,
    /// Surfaces deleted (they were as shipped).
    #[serde(default)]
    pub deleted: Vec<String>,
    /// Surfaces kept (the person changed them); they no longer have a baseline.
    #[serde(default)]
    pub kept: Vec<String>,
    /// Workflows deleted (they were as shipped).
    #[serde(default)]
    pub workflows_deleted: Vec<String>,
    /// Workflows kept (the person edited them).
    #[serde(default)]
    pub workflows_kept: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One shipped surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShippedRow {
    pub surface_id: String,
    pub package: String,
    pub name: String,
    pub version: String,
    /// `pristine`, `modified`, `diverged` or `missing`.
    pub status: String,
    /// The newer shipped version waiting, when `diverged`.
    pub pending_version: Option<String>,
}

/// `shipped-status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ShippedStatusResult {
    pub ok: bool,
    pub rows: Vec<ShippedRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One change from the shipped spec to the person's copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeRow {
    /// A JSON pointer into the spec.
    pub pointer: String,
    /// `added`, `removed` or `changed`.
    pub op: String,
    /// The shipped value, cut at 200 characters.
    pub shipped: Option<String>,
    /// The person's value, cut at 200 characters.
    pub yours: Option<String>,
}

/// `shipped-diff`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DiffResult {
    pub ok: bool,
    #[serde(default)]
    pub surface_id: String,
    #[serde(default)]
    pub changes: Vec<ChangeRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `shipped-reset`, `-take`, `-keep`: the surface after.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ShippedResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<ShippedRow>,
    /// Set by the pipeline under `reversibly`: the call that undoes this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A registry the host knows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RegistryRow {
    pub url: String,
    pub name: String,
    pub added_at: String,
}

/// `registries`, `registry-add`, `registry-remove`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RegistriesResult {
    pub ok: bool,
    #[serde(default)]
    pub registries: Vec<RegistryRow>,
    /// How many packages the added registry lists (`registry-add`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One package version a registry lists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FoundRow {
    pub name: String,
    pub version: String,
    pub description: String,
    pub registry: String,
    pub digest: String,
    /// The version installed now, if any.
    pub installed: Option<String>,
    /// `install`, `installed`, `update` (newer than installed) or `older`.
    pub state: String,
}

/// `search`, `updates`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    pub ok: bool,
    #[serde(default)]
    pub rows: Vec<FoundRow>,
    /// Registries that could not be read, with why.
    #[serde(default)]
    pub unreadable: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
