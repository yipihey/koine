//! The one verb descriptor (ADR-0034 D1, plan-verb-pipeline P1).
//!
//! Every `#[impress_method]` expands to exactly one `static`
//! [`VerbDescriptor`]; [`McpToolDescriptor`](crate::McpToolDescriptor) and
//! [`CliSubcommand`](crate::CliSubcommand) are projections of it (each keeps
//! the fields its readers use and a `verb` pointer back), so the inventory
//! `submit!`s and their readers compile unchanged while every interface reads
//! the same record.
//!
//! Derived over declared: the output schema comes from the return type (the
//! macro adds the `JsonSchema` bound), the group from the service, the name
//! from the identifiers. What is declared — `safety`, `since`, examples — is
//! declared once per service with per-method exceptions, and every declared
//! field has a test over the linked inventory
//! (`crates/impress-capabilities/tests/descriptor.rs`) that fails when it is
//! missing or disagrees with the committed table (`docs/verb-safety.md`).

use serde_json::{json, Value};

use crate::ServiceFuture;

/// What a verb does to the world, in the vocabulary of the safety table
/// (plan-auto-gui-and-self-docs.md table 4, appendix A4).
///
/// The class is the *worst* thing the verb can do: a verb that deletes rows
/// is `Destructive` even when it usually only reads; a verb whose default
/// implementation refuses and whose native backend reaches a running app, the
/// network, a device or a subprocess is `External`, whatever it does there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SafetyClass {
    /// Reads the store or computes; changes nothing an undo would need.
    ReadOnly,
    /// Writes rows or files in a way the suite can account for (an
    /// operation row, an undo ring, a revision).
    Mutating,
    /// Deletes or overwrites with no inverse the service offers.
    Destructive,
    /// Leaves the process: a running app over HTTP, the network, a device,
    /// a subprocess, an AI provider. What happens there is not the store's.
    External,
}

impl SafetyClass {
    /// Every class, in table order.
    pub const ALL: [SafetyClass; 4] = [
        SafetyClass::ReadOnly,
        SafetyClass::Mutating,
        SafetyClass::Destructive,
        SafetyClass::External,
    ];

    /// The spelling used in `impress_service_impl! { safety = … }`,
    /// `#[impress_method(safety = …)]` and `docs/verb-safety.md`.
    pub const fn as_str(self) -> &'static str {
        match self {
            SafetyClass::ReadOnly => "read_only",
            SafetyClass::Mutating => "mutating",
            SafetyClass::Destructive => "destructive",
            SafetyClass::External => "external",
        }
    }

    /// Parse the table spelling (`read_only`, also accepting `read-only`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "read_only" | "read-only" => Some(SafetyClass::ReadOnly),
            "mutating" => Some(SafetyClass::Mutating),
            "destructive" => Some(SafetyClass::Destructive),
            "external" => Some(SafetyClass::External),
            _ => None,
        }
    }

    /// A read-only verb is idempotent by construction; anything else must
    /// say so.
    pub const fn default_idempotent(self) -> bool {
        matches!(self, SafetyClass::ReadOnly)
    }
}

impl std::fmt::Display for SafetyClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A record kind a verb reads or writes, as the effect declaration names it
/// (ADR-0036 D1, plan-self-reflective-layer § Effects).
///
/// Most verbs name their kinds outright (`Ref`); the 76 whose effect is a
/// function of an argument — a triage verb writes the kind of whatever `id`
/// names — say so with `Target`/`Children`, and the store spy resolves the
/// argument against the store the same way the verb does. `Prefix` is for
/// the layout and surface services, whose kinds share `impress/ui/`. `Any`
/// is allowed only with a reason, which the exception table prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A canonical ref from `schema-refs.json` (`imbib/bibliography-entry`).
    /// The macro checks the spelling against the manifest at compile time;
    /// the descriptor test checks it again.
    Ref(&'static str),
    /// The kind of the record the named argument identifies (`id`, `ids`,
    /// `publication_ids`, …). The argument may be a string or an array.
    Target(&'static str),
    /// The kinds parented under the record the named argument identifies
    /// (a collection's members).
    Children(&'static str),
    /// Every kind under a prefix (`impress/ui/`).
    Prefix(&'static str),
    /// Any kind, with the reason the verb cannot say more.
    Any(&'static str),
}

impl Kind {
    /// The declaration spelling: `"ref"`, `target(arg)`, `children(arg)`,
    /// `prefix("…")`, `any("reason")`.
    pub fn describe(&self) -> String {
        match self {
            Kind::Ref(r) => format!("\"{r}\""),
            Kind::Target(a) => format!("target({a})"),
            Kind::Children(a) => format!("children({a})"),
            Kind::Prefix(p) => format!("prefix(\"{p}\")"),
            Kind::Any(why) => format!("any(\"{why}\")"),
        }
    }

    /// Whether a concrete schema ref observed on a store call is covered by
    /// this declared kind, once `Target`/`Children` have been resolved to
    /// `resolved` (the kinds the argument named; `None` when the caller
    /// could not resolve them, which covers nothing).
    pub fn covers(&self, observed: &str, resolved: Option<&[String]>) -> bool {
        match self {
            Kind::Ref(r) => *r == observed,
            Kind::Prefix(p) => observed.starts_with(p),
            Kind::Any(_) => true,
            Kind::Target(_) | Kind::Children(_) => {
                resolved.is_some_and(|kinds| kinds.iter().any(|k| k == observed))
            }
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Where a verb reaches outside the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reach {
    /// A running app over its HTTP automation port (`app("imbib")`).
    App(&'static str),
    Network,
    Fs,
    Subprocess,
    Device,
    /// An AI provider (local or hosted).
    Provider,
}

impl Reach {
    /// The declaration spelling (`fs`, `network`, `app("imbib")`, …), the
    /// same one `docs/verb-effects.md` prints.
    pub fn describe(&self) -> String {
        match self {
            Reach::App(id) => format!("app(\"{id}\")"),
            Reach::Network => "network".into(),
            Reach::Fs => "fs".into(),
            Reach::Subprocess => "subprocess".into(),
            Reach::Device => "device".into(),
            Reach::Provider => "provider".into(),
        }
    }

    /// Whether this reach leaves the machine's own process tree in a way the
    /// store cannot account for — everything but `fs`, which a mutating or
    /// destructive verb may declare (backups, exports) without being
    /// `external`.
    pub const fn is_external(&self) -> bool {
        !matches!(self, Reach::Fs)
    }
}

impl std::fmt::Display for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// What a verb touches: the record kinds it reads and writes and where it
/// reaches outside the process (ADR-0036 D1). Declared once per service in
/// `impress_service_impl! { effects = { reads: […], writes: […], reach: […] } }`
/// with per-method exceptions `#[impress_method(effects(reads = […], writes =
/// […], reach = […]))]` on the trait — a method's declaration replaces the
/// service's whole set, as `safety` does. Verified by the store spy over the
/// examples in Tier A (`crates/impress-capabilities/tests/effects.rs`) and
/// recorded, one row per verb, in `docs/verb-effects.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effects {
    /// Record kinds read (`query`, `count`, `get`, `neighbors`).
    pub reads: &'static [Kind],
    /// Record kinds written (`insert`, `update`, `delete`, `apply_operation`).
    pub writes: &'static [Kind],
    /// Outside the process.
    pub reach: &'static [Reach],
}

impl Effects {
    /// Touches nothing: pure computation.
    pub const NONE: Effects = Effects {
        reads: &[],
        writes: &[],
        reach: &[],
    };

    /// The three columns as `docs/verb-effects.md` prints them (`—` for an
    /// empty set; items comma-separated, in declaration order).
    pub fn columns(&self) -> [String; 3] {
        fn join<T: std::fmt::Display>(items: &[T]) -> String {
            if items.is_empty() {
                "—".to_string()
            } else {
                items
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
        [join(self.reads), join(self.writes), join(self.reach)]
    }

    /// Whether any declared kind is `Any`.
    pub fn declares_any(&self) -> bool {
        self.reads
            .iter()
            .chain(self.writes.iter())
            .any(|k| matches!(k, Kind::Any(_)))
    }
}

/// The safety facts a policy layer (ADR-0034 D3) and an MCP client read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Safety {
    pub class: SafetyClass,
    /// Calling the verb twice with the same arguments leaves the world as
    /// one call would. Defaults to `class.default_idempotent()`; a mutating
    /// verb that is (a `set-*`, an upsert) declares `idempotent = true`.
    pub idempotent: bool,
}

/// Which explicitly isolated runner may execute a declared example.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExampleTier {
    /// Headless example over an isolated scratch store.
    A,
    /// Requires an explicitly supplied isolated app or external host.
    B,
}

/// A named example call, declared beside the method with
/// `#[impress_example(name = "…", args = r#"{…}"#, expect = r#"{…}"#)]`.
/// `args` and `expect` are JSON text (validated at macro expansion) so the
/// static descriptor needs no allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Example {
    pub name: &'static str,
    /// The argument object, as JSON text.
    pub args: &'static str,
    /// The expected result shape, as JSON text, when the example states one.
    pub expect: Option<&'static str>,
    /// Execution environment. An omitted macro annotation defaults to A.
    pub tier: ExampleTier,
}

impl Example {
    /// The argument object, parsed.
    pub fn args_value(&self) -> Value {
        serde_json::from_str(self.args).unwrap_or(Value::Null)
    }
}

/// A verb's retirement notice (ADR-0034 D5, filled by P3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deprecation {
    /// The version the verb was deprecated in.
    pub since: &'static str,
    /// The verb that replaces it, when one does.
    pub alias_of: Option<&'static str>,
    pub note: &'static str,
}

/// Where a descriptor comes from (ADR-0034 D5, D7; P3 and P8 add variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Generated by `impress_service_impl!` and linked into this binary.
    Linked,
}

/// Per-method facts `#[impress_service]` captures from the trait for
/// `impress_service_impl!` to resolve at compile time: the doc comment, the
/// safety override and the examples. One table per trait, a `static` so the
/// generated descriptor can point into it.
#[derive(Debug)]
pub struct MethodMeta {
    /// The Rust method identifier.
    pub name: &'static str,
    /// The joined `///` comment (never empty; the macro refuses an
    /// undocumented method).
    pub doc: &'static str,
    /// `#[impress_method(safety = …)]`, when the method departs from the
    /// service's default.
    pub safety: Option<SafetyClass>,
    /// `#[impress_method(idempotent = …)]`, when declared.
    pub idempotent: Option<bool>,
    /// `#[impress_method(effects(…))]`, when the method departs from the
    /// service's default set.
    pub effects: Option<Effects>,
    pub examples: &'static [Example],
    /// `#[impress_method(deprecated(since = "…", note = "…"))]` (P3). A
    /// method that also declares `aliases` requires this — a rename implies
    /// deprecating the old name — and the macro refuses one without the
    /// other.
    pub deprecated: Option<Deprecation>,
    /// `#[impress_method(aliases = ["old-name", …])]` (P3): retired names
    /// that still resolve to this verb through
    /// [`crate::call::find`](crate::call), reported in `docs/verbs/` and
    /// excluded from `tools/list` and the CLI's own listing — the alias is
    /// dispatchable, never advertised.
    pub aliases: &'static [&'static str],
    /// `#[impress_method(budget_ms = …)]` (D-P2, G7c): the wall-clock ceiling
    /// a Tier A example may take before the runner fails it. `None` — the
    /// default for every verb until one declares a budget — means no ceiling
    /// is enforced.
    pub budget_ms: Option<u64>,
    /// Store complete call arguments for replay when the audit privacy rule permits it.
    pub replay_full: bool,
}

/// Find a method's captured facts by identifier. `const` because
/// `inventory::submit!` builds a `static`.
pub const fn method_meta(
    table: &'static [MethodMeta],
    method: &str,
) -> Option<&'static MethodMeta> {
    let mut i = 0;
    while i < table.len() {
        if crate::const_str_eq(table[i].name, method) {
            return Some(&table[i]);
        }
        i += 1;
    }
    None
}

/// The safety class for a method: its own declaration, else the service's
/// default.
pub const fn resolve_safety_class(
    table: &'static [MethodMeta],
    method: &str,
    service_default: SafetyClass,
) -> SafetyClass {
    match method_meta(table, method) {
        Some(MethodMeta {
            safety: Some(class),
            ..
        }) => *class,
        _ => service_default,
    }
}

/// Idempotency for a method: its own declaration, else the class's default.
pub const fn resolve_idempotent(
    table: &'static [MethodMeta],
    method: &str,
    class: SafetyClass,
) -> bool {
    match method_meta(table, method) {
        Some(MethodMeta {
            idempotent: Some(flag),
            ..
        }) => *flag,
        _ => class.default_idempotent(),
    }
}

/// The effect set for a method: its own declaration, else the service's
/// default (ADR-0036 D1). A method's declaration replaces the whole set.
pub const fn resolve_effects(
    table: &'static [MethodMeta],
    method: &str,
    service_default: Effects,
) -> Effects {
    match method_meta(table, method) {
        Some(MethodMeta {
            effects: Some(effects),
            ..
        }) => *effects,
        _ => service_default,
    }
}

/// The examples declared on a method (empty until G3 writes them).
pub const fn resolve_examples(table: &'static [MethodMeta], method: &str) -> &'static [Example] {
    match method_meta(table, method) {
        Some(meta) => meta.examples,
        None => &[],
    }
}

/// A method's Tier A time budget (D-P2, G7c), declared with
/// `#[impress_method(budget_ms = …)]`. `None` for every verb until one
/// declares a ceiling.
pub const fn resolve_budget_ms(table: &'static [MethodMeta], method: &str) -> Option<u64> {
    match method_meta(table, method) {
        Some(meta) => meta.budget_ms,
        None => None,
    }
}

/// A method explicitly declared `#[impress_method(replay = full)]`.
pub const fn resolve_replay_full(table: &'static [MethodMeta], method: &str) -> bool {
    match method_meta(table, method) {
        Some(meta) => meta.replay_full,
        None => false,
    }
}

/// A method's retirement notice (P3), declared with `#[impress_method(
/// deprecated(since = "…", note = "…"))]`. `None` for every verb until a
/// method declares one.
pub const fn resolve_deprecated(table: &'static [MethodMeta], method: &str) -> Option<Deprecation> {
    match method_meta(table, method) {
        Some(meta) => meta.deprecated,
        None => None,
    }
}

/// The retired names that resolve to this method (P3), declared with
/// `#[impress_method(aliases = ["old-name", …])]`. Empty for every verb
/// until a method declares one.
pub const fn resolve_aliases(
    table: &'static [MethodMeta],
    method: &str,
) -> &'static [&'static str] {
    match method_meta(table, method) {
        Some(meta) => meta.aliases,
        None => &[],
    }
}

/// One verb, as every interface sees it.
///
/// Field by field, where it comes from:
/// - `name`, `service`, `method`: the identifiers (derived);
/// - `description`: the trait method's doc (derived; refused when empty);
/// - `input_schema`: `schema_for!` of the generated args struct (derived);
/// - `output_schema`: `schema_for!` of the return type (derived — the bound
///   is why every result type derives `JsonSchema`);
/// - `safety`: the service's `safety = …` default with the method's
///   `#[impress_method(safety = …)]` exception (declared; checked against
///   `docs/verb-safety.md`);
/// - `effects`: the service's `effects = { … }` default with the method's
///   `#[impress_method(effects(…))]` exception (declared; checked against
///   `docs/verb-effects.md` and verified by the store spy);
/// - `since`: the service's `since = "…"` (declared; checked non-empty);
/// - `deprecated`, `aliases`: P3's lifecycle fields, empty until then;
/// - `examples`: `#[impress_example]` on the method (declared; G3 fills);
/// - `replay_full`: `#[impress_method(replay = full)]` (declared; audit still
///   applies privacy and size limits);
/// - `strict`: the service's `strict_args` (declared);
/// - `source`, `handler`: how the verb runs.
pub struct VerbDescriptor {
    /// The qualified tool name, `<service>_<method>` in kebab case — the MCP
    /// tool name, the CLI's fallback spelling and the name a surface uses.
    pub name: &'static str,
    /// The service in kebab case (`imbib-library-service`): the group.
    pub service: &'static str,
    /// The method in kebab case (`delete-publications-undoable`): the CLI's
    /// short spelling.
    pub method: &'static str,
    pub description: &'static str,
    pub input_schema: fn() -> Value,
    pub output_schema: fn() -> Value,
    pub safety: Safety,
    pub effects: Effects,
    pub since: &'static str,
    pub deprecated: Option<Deprecation>,
    pub aliases: &'static [&'static str],
    pub examples: &'static [Example],
    /// `impress_service_impl! { strict_args = true }`: an argument the schema
    /// does not name is refused `invalid-argument`.
    pub strict: bool,
    /// `#[impress_method(budget_ms = …)]` (D-P2, G7c): a Tier A example of
    /// this verb that takes longer than this fails the run. `None` when no
    /// budget is declared.
    pub budget_ms: Option<u64>,
    /// `#[impress_method(replay = full)]`; the audit layer still checks privacy and size.
    pub replay_full: bool,
    pub source: Source,
    pub handler: fn(Value) -> ServiceFuture,
}

impl VerbDescriptor {
    /// Every verb linked into this binary, in inventory order.
    pub fn iter() -> impl Iterator<Item = &'static VerbDescriptor> {
        crate::McpToolDescriptor::iter().map(|d| d.verb)
    }

    /// Look one up by its qualified name.
    pub fn find(name: &str) -> Option<&'static VerbDescriptor> {
        Self::iter().find(|v| v.name == name)
    }

    /// Whether an alias name resolves to this verb.
    pub fn has_alias(&self, name: &str) -> bool {
        self.aliases.contains(&name)
    }

    /// The additive `"deprecated"` envelope field for a call under this
    /// verb's own canonical name (P3, plan-verb-pipeline § Lifecycle):
    /// `since`/`use`/`note`, `use` naming the replacement when the
    /// deprecation records one (`alias_of`), else this verb's own name.
    /// `None` when the verb is not itself deprecated — in particular, when
    /// it declares `aliases`: those retired names are what is deprecated,
    /// not a direct call to the current, canonical name (see
    /// [`Self::alias_deprecation_notice`]).
    pub fn deprecation_notice(&self) -> Option<Value> {
        if !self.aliases.is_empty() {
            return None;
        }
        self.build_deprecation_notice()
    }

    /// The additive `"deprecated"` envelope field for a call that arrived
    /// through one of [`Self::aliases`] — always present once the method
    /// declared any aliases (the macro requires `deprecated(…)` alongside
    /// `aliases = […]`).
    pub fn alias_deprecation_notice(&self) -> Option<Value> {
        self.build_deprecation_notice()
    }

    fn build_deprecation_notice(&self) -> Option<Value> {
        let dep = self.deprecated?;
        Some(json!({
            "since": dep.since,
            "use": dep.alias_of.unwrap_or(self.name),
            "note": dep.note,
        }))
    }

    /// The MCP `annotations` object (MCP 2025-03-26 tool annotations), read
    /// off the safety class:
    /// - `readOnlyHint`: the class is `read_only`;
    /// - `destructiveHint`: the class is `destructive` — or `external`, since
    ///   what an external call does is not the store's to promise (the
    ///   protocol's own default is `true`, so this is the conservative
    ///   reading, not a new claim);
    /// - `idempotentHint`: `safety.idempotent`;
    /// - `openWorldHint`: the class is `external`.
    pub fn mcp_annotations(&self) -> Value {
        self.safety.mcp_annotations()
    }
}

impl std::fmt::Debug for VerbDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerbDescriptor")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("safety", &self.safety)
            .field("effects", &self.effects)
            .field("since", &self.since)
            .field("strict", &self.strict)
            .finish()
    }
}

impl Safety {
    pub fn mcp_annotations(&self) -> Value {
        let class = self.class;
        json!({
            "readOnlyHint": class == SafetyClass::ReadOnly,
            "destructiveHint": matches!(class, SafetyClass::Destructive | SafetyClass::External),
            "idempotentHint": self.idempotent,
            "openWorldHint": class == SafetyClass::External,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TABLE: [MethodMeta; 2] = [
        MethodMeta {
            name: "delete",
            doc: "Delete it.",
            safety: Some(SafetyClass::Destructive),
            idempotent: None,
            effects: Some(Effects {
                reads: &[],
                writes: &[Kind::Target("id")],
                reach: &[],
            }),
            examples: &[Example {
                name: "one",
                args: r#"{"id": "x"}"#,
                expect: None,
                tier: ExampleTier::A,
            }],
            deprecated: None,
            aliases: &[],
            budget_ms: Some(5),
            replay_full: false,
        },
        MethodMeta {
            name: "set_flag",
            doc: "Set it.",
            safety: None,
            idempotent: Some(true),
            effects: None,
            examples: &[],
            deprecated: Some(Deprecation {
                since: "0.2.0",
                alias_of: None,
                note: "superseded by delete",
            }),
            aliases: &["old_set_flag"],
            budget_ms: None,
            replay_full: false,
        },
    ];

    const SERVICE_EFFECTS: Effects = Effects {
        reads: &[Kind::Ref("imbib/bibliography-entry")],
        writes: &[],
        reach: &[Reach::Fs],
    };

    #[test]
    fn a_method_effects_declaration_replaces_the_service_default() {
        let own = resolve_effects(&TABLE, "delete", SERVICE_EFFECTS);
        assert_eq!(own.writes, &[Kind::Target("id")]);
        assert!(own.reads.is_empty(), "replaces, never merges");
        assert_eq!(
            resolve_effects(&TABLE, "set_flag", SERVICE_EFFECTS),
            SERVICE_EFFECTS
        );
        assert_eq!(
            resolve_effects(&TABLE, "missing", SERVICE_EFFECTS),
            SERVICE_EFFECTS
        );
    }

    #[test]
    fn a_budget_declaration_is_resolved_and_defaults_to_none() {
        assert_eq!(resolve_budget_ms(&TABLE, "delete"), Some(5));
        assert_eq!(resolve_budget_ms(&TABLE, "set_flag"), None);
        assert_eq!(resolve_budget_ms(&TABLE, "missing"), None);
    }

    #[test]
    fn kinds_cover_observed_refs_once_targets_resolve() {
        assert!(Kind::Ref("manuscript").covers("manuscript", None));
        assert!(!Kind::Ref("manuscript").covers("manuscript-file@1.0.0", None));
        assert!(Kind::Prefix("impress/ui/").covers("impress/ui/layout@1.0.0", None));
        assert!(Kind::Any("dispatches whatever the surface calls").covers("x", None));
        assert!(
            !Kind::Target("id").covers("figure", None),
            "unresolved covers nothing"
        );
        assert!(Kind::Target("id").covers("figure", Some(&["figure".to_string()])));
        assert!(!Kind::Children("id").covers("figure", Some(&["manuscript".to_string()])));
    }

    #[test]
    fn effects_print_as_the_table_does() {
        let e = Effects {
            reads: &[Kind::Ref("manuscript"), Kind::Target("ids")],
            writes: &[],
            reach: &[Reach::App("imbib"), Reach::Network],
        };
        assert_eq!(
            e.columns(),
            [
                "\"manuscript\", target(ids)".to_string(),
                "—".to_string(),
                "app(\"imbib\"), network".to_string()
            ]
        );
        assert!(!e.declares_any());
        assert!(Reach::Network.is_external() && !Reach::Fs.is_external());
    }

    #[test]
    fn a_method_override_beats_the_service_default() {
        assert_eq!(
            resolve_safety_class(&TABLE, "delete", SafetyClass::Mutating),
            SafetyClass::Destructive
        );
        assert_eq!(
            resolve_safety_class(&TABLE, "set_flag", SafetyClass::Mutating),
            SafetyClass::Mutating
        );
        assert_eq!(
            resolve_safety_class(&TABLE, "missing", SafetyClass::ReadOnly),
            SafetyClass::ReadOnly
        );
    }

    #[test]
    fn idempotency_defaults_from_the_class_unless_declared() {
        assert!(!resolve_idempotent(
            &TABLE,
            "delete",
            SafetyClass::Destructive
        ));
        assert!(resolve_idempotent(
            &TABLE,
            "set_flag",
            SafetyClass::Mutating
        ));
        assert!(resolve_idempotent(&TABLE, "missing", SafetyClass::ReadOnly));
        assert!(!resolve_idempotent(
            &TABLE,
            "missing",
            SafetyClass::Mutating
        ));
    }

    #[test]
    fn examples_resolve_per_method() {
        assert_eq!(resolve_examples(&TABLE, "delete").len(), 1);
        assert_eq!(
            resolve_examples(&TABLE, "delete")[0].args_value()["id"],
            "x"
        );
        assert!(resolve_examples(&TABLE, "set_flag").is_empty());
        assert!(resolve_examples(&TABLE, "missing").is_empty());
    }

    #[test]
    fn deprecation_and_aliases_resolve_per_method() {
        assert_eq!(resolve_deprecated(&TABLE, "delete"), None);
        assert_eq!(resolve_aliases(&TABLE, "delete"), &[] as &[&str]);
        assert_eq!(
            resolve_deprecated(&TABLE, "set_flag"),
            Some(Deprecation {
                since: "0.2.0",
                alias_of: None,
                note: "superseded by delete",
            })
        );
        assert_eq!(resolve_aliases(&TABLE, "set_flag"), &["old_set_flag"]);
        assert_eq!(resolve_deprecated(&TABLE, "missing"), None);
        assert_eq!(resolve_aliases(&TABLE, "missing"), &[] as &[&str]);
    }

    #[test]
    fn deprecation_notice_names_the_replacement_or_falls_back_to_self() {
        let mut verb = VerbDescriptor {
            name: "t-service_x",
            service: "t-service",
            method: "x",
            description: "d",
            input_schema: schema,
            output_schema: schema,
            safety: Safety {
                class: SafetyClass::ReadOnly,
                idempotent: true,
            },
            effects: Effects::NONE,
            since: "0.1.0",
            deprecated: None,
            aliases: &[],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            source: Source::Linked,
            handler,
        };
        assert_eq!(verb.deprecation_notice(), None);
        verb.deprecated = Some(Deprecation {
            since: "0.9.0",
            alias_of: None,
            note: "retired, no replacement",
        });
        assert_eq!(
            verb.deprecation_notice(),
            Some(
                json!({"since": "0.9.0", "use": "t-service_x", "note": "retired, no replacement"})
            )
        );
        verb.deprecated = Some(Deprecation {
            since: "0.9.0",
            alias_of: Some("t-service_y"),
            note: "renamed",
        });
        assert_eq!(
            verb.deprecation_notice(),
            Some(json!({"since": "0.9.0", "use": "t-service_y", "note": "renamed"}))
        );
        assert!(!verb.has_alias("old-x"));
        verb.aliases = &["old-x"];
        assert!(verb.has_alias("old-x"));
    }

    #[test]
    fn aliased_verbs_are_deprecated_only_by_their_old_name() {
        let mut verb = VerbDescriptor {
            name: "t-service_x",
            service: "t-service",
            method: "x",
            description: "d",
            input_schema: schema,
            output_schema: schema,
            safety: Safety {
                class: SafetyClass::ReadOnly,
                idempotent: true,
            },
            effects: Effects::NONE,
            since: "0.1.0",
            deprecated: Some(Deprecation {
                since: "0.5.0",
                alias_of: None,
                note: "renamed",
            }),
            aliases: &["old-x"],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            source: Source::Linked,
            handler,
        };
        assert_eq!(
            verb.deprecation_notice(),
            None,
            "a direct call to the live name is not deprecated"
        );
        assert_eq!(
            verb.alias_deprecation_notice(),
            Some(json!({"since": "0.5.0", "use": "t-service_x", "note": "renamed"}))
        );
        verb.aliases = &[];
        assert_eq!(
            verb.deprecation_notice(),
            Some(json!({"since": "0.5.0", "use": "t-service_x", "note": "renamed"})),
            "with no aliases, the verb itself is what's deprecated"
        );
    }

    #[test]
    fn class_spelling_round_trips() {
        for class in SafetyClass::ALL {
            assert_eq!(SafetyClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(SafetyClass::parse("read-only"), Some(SafetyClass::ReadOnly));
        assert_eq!(SafetyClass::parse("nope"), None);
    }

    fn schema() -> Value {
        json!({})
    }
    fn handler(_: Value) -> ServiceFuture {
        Box::pin(async { Ok(Value::Null) })
    }

    #[test]
    fn annotations_follow_the_class() {
        let mut verb = VerbDescriptor {
            name: "t-service_x",
            service: "t-service",
            method: "x",
            description: "d",
            input_schema: schema,
            output_schema: schema,
            safety: Safety {
                class: SafetyClass::ReadOnly,
                idempotent: true,
            },
            effects: Effects::NONE,
            since: "0.1.0",
            deprecated: None,
            aliases: &[],
            examples: &[],
            strict: false,
            budget_ms: None,
            replay_full: false,
            source: Source::Linked,
            handler,
        };
        assert_eq!(
            verb.mcp_annotations(),
            json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false})
        );
        verb.safety = Safety {
            class: SafetyClass::Destructive,
            idempotent: false,
        };
        assert_eq!(
            verb.mcp_annotations(),
            json!({"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false})
        );
        verb.safety.class = SafetyClass::External;
        assert_eq!(
            verb.mcp_annotations(),
            json!({"readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": true})
        );
        verb.safety.class = SafetyClass::Mutating;
        assert_eq!(
            verb.mcp_annotations(),
            json!({"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false})
        );
    }
}
