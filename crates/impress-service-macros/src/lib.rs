//! Procedural macros for the impress service codegen pipeline.
//!
//! # Usage
//!
//! ```ignore
//! use impress_service_core as svc;
//! use impress_service_macros::{impress_service, impress_service_impl};
//!
//! #[impress_service]
//! pub trait EchoService: Send + Sync + 'static {
//!     /// Echo a message back to the caller.
//!     #[impress_method]
//!     async fn echo(&self, message: String) -> String;
//! }
//!
//! pub struct DemoEcho;
//!
//! #[async_trait::async_trait]
//! impl EchoService for DemoEcho {
//!     async fn echo(&self, message: String) -> String {
//!         format!("echo: {message}")
//!     }
//! }
//!
//! impress_service_impl! {
//!     service = EchoService,
//!     impl = DemoEcho,
//!     instance = || DemoEcho,
//!     safety = read_only,
//!     since = "0.1.0",
//!     methods = [
//!         echo(message: String) -> String,
//!     ],
//! }
//! ```
//!
//! Phase 0 supports a small set of "simple" argument types:
//! `String`, `i64`, `u64`, `bool`, `Option<String>`, `Vec<String>`.
//! Return types follow the same set, plus `()`.
//!
//! The macros emit, per method:
//! * A standalone async invoker `__impress_<service>_<method>_invoke`.
//! * An input-schema fn (the args struct) and an output-schema fn (the
//!   return type, which must therefore implement `JsonSchema`).
//! * One `static` `VerbDescriptor` (ADR-0034 D1) carrying name, service,
//!   description, both schemas, safety, `since`, examples and the handler.
//! * An `inventory::submit!` registering its `McpToolDescriptor` projection.
//! * An `inventory::submit!` registering its `CliSubcommand` projection.
//!
//! That is the whole output. No UniFFI or Python shim is generated here: the
//! Swift bindings are hand-written `#[uniffi::export]` items in the FFI crates,
//! and Python is decided in `docs/plan-verb-pipeline-and-transport.md`.
//!
//! Every `#[impress_method]` must carry a `///` doc comment, and every
//! `methods = [...]` entry must resolve an inline or trait doc: it is the
//! description agents read. A missing description is a compile error naming
//! the method (plan-auto-gui-and-self-docs.md G-2 — 54 verbs once shipped
//! `Invoke Service.method` because the macro accepted an empty doc).
//!
//! Safety is declared once per service — `impress_service_impl! { safety =
//! read_only, since = "0.1.0", … }` — with per-method exceptions on the trait,
//! `#[impress_method(safety = destructive, idempotent = false)]`. The
//! vocabulary is `read_only | mutating | destructive | external`
//! (`impress_service_core::SafetyClass`), and `docs/verb-safety.md` is the
//! table every declaration is checked against. Examples go beside the method
//! as `#[impress_example(name = "…", args = r#"{…}"#, expect = r#"{…}"#)]`.
//! `tier = "b"` opts into an isolated host run; omitted tier means A.
//!
//! Effects (ADR-0036 D1) are declared the same way — once per service,
//! `impress_service_impl! { effects = { reads: ["imbib/bibliography-entry"],
//! writes: [target(id)], reach: [fs] }, … }`, with per-method exceptions
//! `#[impress_method(effects(reads = […], writes = […], reach = […]))]` that
//! replace the whole set. A kind is a canonical ref from `schema-refs.json`
//! (checked here at expansion time; a misspelt ref is a compile error),
//! `target(arg)` / `children(arg)` for the kind an argument names,
//! `prefix("impress/ui/")`, or `any("reason")`. Reach is `fs | network |
//! subprocess | device | provider | app("imbib")`. `docs/verb-effects.md` is
//! the table every declaration is checked against, and the store spy verifies
//! it over the examples. An argument the call log must never store by value
//! is marked `#[impress_private]` in `methods = […]`, which sets
//! `"x-private": true` on its input-schema property.
//!
//! Anything more elaborate (custom DTOs, error mapping nuances) is deferred to
//! Phase 1+.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Ident, ItemTrait, TraitItem, Type};

mod schema_refs {
    include!(concat!(env!("OUT_DIR"), "/schema_refs.rs"));
}

/// `#[impress_service]` attribute on a trait.
///
/// Confirms the trait has at least one `#[impress_method]`, strips the
/// per-method marker attribute (Rust does not let unknown attributes on trait
/// items flow downstream untouched), and emits the trait plus a hidden
/// `__IMPRESS_SERVICE_DOCS_<Trait>` table of each method's doc comment.
///
/// That table is why a description written on the trait — where a Rust
/// developer naturally puts it — reaches the model. Before it existed, only
/// `///` comments inside `impress_service_impl! { methods = [...] }` were read,
/// and 119 of 133 tools silently shipped `"Invoke Service.method"`.
///
/// The heavy lifting (per-method invokers, inventory submissions) is
/// performed by [`impress_service_impl!`] against a concrete impl block,
/// because that is where we know the concrete `Self` type to dispatch into.
///
/// A method marked `#[impress_method]` with no `///` doc comment is a compile
/// error naming the method: the doc is the description agents read, and an
/// empty one used to ship as `Invoke Service.method`.
#[proc_macro_attribute]
pub fn impress_service(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let trait_item = parse_macro_input!(input as ItemTrait);
    match expand_service(trait_item) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// The body of [`impress_service`], split out so the doc-comment rule can be
/// unit-tested on a parsed trait without a compile-fail harness.
fn expand_service(mut trait_item: ItemTrait) -> syn::Result<TokenStream2> {
    let mut found_any_method = false;
    // One `MethodMeta` per #[impress_method] — the doc, the safety override
    // and the examples — so `impress_service_impl!` can read what was written
    // on the trait. Without the doc half of this, the docs a developer writes
    // on the trait were silently dropped and the model got
    // "Invoke Service.method".
    let mut metas: Vec<TokenStream2> = Vec::new();
    let trait_name = trait_item.ident.to_string();

    for item in &mut trait_item.items {
        if let TraitItem::Fn(method) = item {
            let markers: Vec<syn::Attribute> = method
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("impress_method"))
                .cloned()
                .collect();
            let examples: Vec<syn::Attribute> = method
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("impress_example"))
                .cloned()
                .collect();
            method.attrs.retain(|attr| {
                !attr.path().is_ident("impress_method") && !attr.path().is_ident("impress_example")
            });
            if markers.is_empty() {
                if let Some(example) = examples.first() {
                    return Err(syn::Error::new_spanned(
                        example,
                        format!(
                            "#[impress_example] on `{trait_name}::{}`, which is not an \
                             #[impress_method]",
                            method.sig.ident
                        ),
                    ));
                }
                continue;
            }
            {
                found_any_method = true;
                let doc = collect_doc(&method.attrs);
                if doc.is_empty() {
                    let method_name = method.sig.ident.to_string();
                    return Err(syn::Error::new_spanned(
                        &method.sig.ident,
                        format!(
                            "#[impress_method] `{trait_name}::{method_name}` has no doc comment; \
                             write a `///` line saying what the verb does — it is the description \
                             agents read, and without it the tool would ship as \
                             `Invoke {trait_name}.{method_name}`.",
                        ),
                    ));
                }
                let method_name = method.sig.ident.to_string();
                let overrides = parse_method_overrides(&markers)?;
                let example_tokens = examples
                    .iter()
                    .map(parse_example)
                    .collect::<syn::Result<Vec<_>>>()?;
                let safety = match overrides.safety {
                    Some(class) => {
                        let variant = format_ident!("{class}");
                        quote! { ::core::option::Option::Some(::impress_service_core::SafetyClass::#variant) }
                    }
                    None => quote! { ::core::option::Option::None },
                };
                let idempotent = match overrides.idempotent {
                    Some(flag) => quote! { ::core::option::Option::Some(#flag) },
                    None => quote! { ::core::option::Option::None },
                };
                let effects = match &overrides.effects {
                    Some(decl) => {
                        let literal = decl.literal();
                        quote! { ::core::option::Option::Some(#literal) }
                    }
                    None => quote! { ::core::option::Option::None },
                };
                let deprecated = match &overrides.deprecated {
                    Some(decl) => {
                        let literal = decl.literal();
                        quote! { ::core::option::Option::Some(#literal) }
                    }
                    None => quote! { ::core::option::Option::None },
                };
                let aliases = &overrides.aliases;
                let budget_ms = match &overrides.budget_ms {
                    Some(lit) => quote! { ::core::option::Option::Some(#lit) },
                    None => quote! { ::core::option::Option::None },
                };
                let replay_full = overrides.replay_full;
                metas.push(quote! {
                    ::impress_service_core::MethodMeta {
                        name: #method_name,
                        doc: #doc,
                        safety: #safety,
                        idempotent: #idempotent,
                        effects: #effects,
                        examples: &[#(#example_tokens),*],
                        deprecated: #deprecated,
                        aliases: &[#(#aliases),*],
                        budget_ms: #budget_ms,
                        replay_full: #replay_full,
                    }
                });
            }
        }
    }

    if !found_any_method {
        return Err(syn::Error::new_spanned(
            &trait_item.ident,
            format!(
                "#[impress_service] trait `{trait_name}` has no #[impress_method] methods; \
                 add #[impress_method] to at least one method.",
            ),
        ));
    }

    // The trait has `async fn` methods, so it needs `#[async_trait::async_trait]`
    // applied (until native async-in-traits are universally available with
    // dyn-compatible vtables on Rust stable). We add it here so users don't
    // have to write the attribute themselves.
    // Emitted beside the trait so `impress_service_impl!` can resolve each
    // method's description, safety and examples. A free static rather than
    // an associated const: associated consts make a trait non-dyn-compatible,
    // and every service is used as `Arc<dyn Trait>`.
    let meta_table = format_ident!("__IMPRESS_SERVICE_METHODS_{}", trait_item.ident);
    let meta_count = metas.len();

    Ok(quote! {
        // `too_many_arguments`: a service method's parameter list *is* the
        // tool's input schema. Bundling arguments into a struct to please the
        // lint would change the schema agents see, so a 10-parameter method is
        // correct here in a way it would not be in ordinary code.
        #[allow(clippy::too_many_arguments)]
        #[::impress_service_core::async_trait::async_trait]
        #trait_item

        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        pub static #meta_table: [::impress_service_core::MethodMeta; #meta_count] =
            [#(#metas),*];
    })
}

/// What `#[impress_method(safety = …, idempotent = …, effects(…),
/// deprecated(…), aliases = […], replay = full]` declares.
#[derive(Default)]
struct MethodOverrides {
    /// The `SafetyClass` variant name (`ReadOnly`, …), validated.
    safety: Option<String>,
    idempotent: Option<bool>,
    effects: Option<EffectsDecl>,
    /// `deprecated(since = "…", note = "…")` (P3, plan-verb-pipeline §
    /// Lifecycle).
    deprecated: Option<DeprecatedDecl>,
    /// `aliases = ["old-name", …]` (P3): retired names for this verb.
    /// Requires `deprecated(…)` on the same method — a rename implies
    /// deprecating the old name.
    aliases: Vec<syn::LitStr>,
    /// `budget_ms = …` (D-P2, G7c): a Tier A example ceiling in whole
    /// milliseconds.
    budget_ms: Option<syn::LitInt>,
    /// `replay = full`: opt into complete arguments when the audit privacy rule permits.
    replay_full: bool,
}

/// `deprecated(since = "…", note = "…")` as written on `#[impress_method]`.
struct DeprecatedDecl {
    since: syn::LitStr,
    note: syn::LitStr,
}

impl DeprecatedDecl {
    /// `::impress_service_core::Deprecation { since: …, alias_of: …, note: … }`,
    /// with `alias_of` always `None`: the pipeline reads the replacement
    /// name off the *call site* (the alias resolved, or the verb's own
    /// canonical name) rather than a second copy of it here.
    fn literal(&self) -> TokenStream2 {
        let since = &self.since;
        let note = &self.note;
        quote! {
            ::impress_service_core::Deprecation {
                since: #since,
                alias_of: ::core::option::Option::None,
                note: #note,
            }
        }
    }
}

/// An effect set as written — `reads`, `writes` and `reach` lists, each
/// already turned into `impress_service_core::{Kind, Reach}` tokens. The
/// three keys are each optional and default to empty; the set as a whole is
/// what a method declaration replaces.
#[derive(Default)]
struct EffectsDecl {
    reads: Vec<TokenStream2>,
    writes: Vec<TokenStream2>,
    reach: Vec<TokenStream2>,
}

impl EffectsDecl {
    fn literal(&self) -> TokenStream2 {
        let reads = &self.reads;
        let writes = &self.writes;
        let reach = &self.reach;
        quote! {
            ::impress_service_core::Effects {
                reads: &[#(#reads),*],
                writes: &[#(#writes),*],
                reach: &[#(#reach),*],
            }
        }
    }

    /// One `key = [ … ]` (attribute form) or `key: [ … ]` (impl-macro form)
    /// entry, the list already opened. Returns `false` for an unknown key.
    fn take(&mut self, key: &Ident, list: syn::parse::ParseStream) -> syn::Result<bool> {
        match key.to_string().as_str() {
            "reads" => self.reads = parse_kind_list(list)?,
            "writes" => self.writes = parse_kind_list(list)?,
            "reach" => self.reach = parse_reach_list(list)?,
            _ => return Ok(false),
        }
        Ok(true)
    }
}

const KIND_VOCABULARY: &str =
    "a canonical ref string, `target(arg)`, `children(arg)`, `prefix(\"…\")` or `any(\"reason\")`";
const REACH_VOCABULARY: &str =
    "`fs`, `network`, `subprocess`, `device`, `provider` or `app(\"<id>\")`";

/// `[ "imbib/library", target(id), children(collection_id), prefix("impress/ui/"), any("…") ]`.
/// A ref string must be a key of `schema-refs.json`'s `canonical` table when
/// the manifest was beside the workspace at build time.
fn parse_kind_list(input: syn::parse::ParseStream) -> syn::Result<Vec<TokenStream2>> {
    let content;
    syn::bracketed!(content in input);
    let mut out = Vec::new();
    while !content.is_empty() {
        if content.peek(syn::LitStr) {
            let lit: syn::LitStr = content.parse()?;
            let value = lit.value();
            if !schema_refs::CANONICAL_SCHEMA_REFS.is_empty()
                && !schema_refs::CANONICAL_SCHEMA_REFS.contains(&value.as_str())
            {
                return Err(syn::Error::new(
                    lit.span(),
                    format!(
                        "`{value}` is not a canonical schema ref in schema-refs.json; copy the \
                         spelling from the manifest (the store matches `schema_ref` by exact \
                         equality, so a misspelt ref reads zero rows forever)"
                    ),
                ));
            }
            out.push(quote! { ::impress_service_core::Kind::Ref(#lit) });
        } else {
            let head: Ident = content.parse()?;
            let inner;
            syn::parenthesized!(inner in content);
            match head.to_string().as_str() {
                "target" | "children" => {
                    let arg: Ident = inner.parse()?;
                    let arg = arg.to_string();
                    let variant = if head == "target" {
                        quote! { Target }
                    } else {
                        quote! { Children }
                    };
                    out.push(quote! { ::impress_service_core::Kind::#variant(#arg) });
                }
                "prefix" => {
                    let lit: syn::LitStr = inner.parse()?;
                    out.push(quote! { ::impress_service_core::Kind::Prefix(#lit) });
                }
                "any" => {
                    let lit: syn::LitStr = inner.parse()?;
                    if lit.value().trim().is_empty() {
                        return Err(syn::Error::new(lit.span(), "`any(…)` needs a reason"));
                    }
                    out.push(quote! { ::impress_service_core::Kind::Any(#lit) });
                }
                other => {
                    return Err(syn::Error::new(
                        head.span(),
                        format!("unknown kind `{other}`; one of {KIND_VOCABULARY}"),
                    ));
                }
            }
            if !inner.is_empty() {
                return Err(inner.error("one argument"));
            }
        }
        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        }
    }
    Ok(out)
}

/// `[ fs, network, app("imbib") ]`.
fn parse_reach_list(input: syn::parse::ParseStream) -> syn::Result<Vec<TokenStream2>> {
    let content;
    syn::bracketed!(content in input);
    let mut out = Vec::new();
    while !content.is_empty() {
        let head: Ident = content.parse()?;
        let token = match head.to_string().as_str() {
            "fs" => quote! { ::impress_service_core::Reach::Fs },
            "network" => quote! { ::impress_service_core::Reach::Network },
            "subprocess" => quote! { ::impress_service_core::Reach::Subprocess },
            "device" => quote! { ::impress_service_core::Reach::Device },
            "provider" => quote! { ::impress_service_core::Reach::Provider },
            "app" => {
                let inner;
                syn::parenthesized!(inner in content);
                let lit: syn::LitStr = inner.parse()?;
                quote! { ::impress_service_core::Reach::App(#lit) }
            }
            other => {
                return Err(syn::Error::new(
                    head.span(),
                    format!("unknown reach `{other}`; one of {REACH_VOCABULARY}"),
                ));
            }
        };
        out.push(token);
        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        }
    }
    Ok(out)
}

/// `effects(reads = […], writes = […], reach = […])` inside `#[impress_method(…)]`.
/// `effects()` — every key absent — is a method that touches nothing under
/// a service whose default touches something, so the parentheses may be
/// empty (which `parse_nested_meta` would refuse).
fn parse_effects_attr(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<EffectsDecl> {
    let mut decl = EffectsDecl::default();
    let content;
    syn::parenthesized!(content in meta.input);
    while !content.is_empty() {
        let key: Ident = content.parse()?;
        content.parse::<syn::Token![=]>()?;
        if !decl.take(&key, &content)? {
            return Err(syn::Error::new(
                key.span(),
                "unknown effects key; `reads`, `writes` or `reach`",
            ));
        }
        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        }
    }
    Ok(decl)
}

/// `{ reads: […], writes: […], reach: […] }` after `effects =` in
/// `impress_service_impl!`.
fn parse_effects_block(input: syn::parse::ParseStream) -> syn::Result<EffectsDecl> {
    let content;
    syn::braced!(content in input);
    let mut decl = EffectsDecl::default();
    while !content.is_empty() {
        let key: Ident = content.parse()?;
        content.parse::<syn::Token![:]>()?;
        if !decl.take(&key, &content)? {
            return Err(syn::Error::new(
                key.span(),
                "unknown effects key; `reads`, `writes` or `reach`",
            ));
        }
        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        }
    }
    Ok(decl)
}

/// `read_only` → `ReadOnly`, or `None` for anything outside the vocabulary.
fn safety_variant(spelling: &str) -> Option<&'static str> {
    match spelling {
        "read_only" => Some("ReadOnly"),
        "mutating" => Some("Mutating"),
        "destructive" => Some("Destructive"),
        "external" => Some("External"),
        _ => None,
    }
}

const SAFETY_VOCABULARY: &str = "`read_only`, `mutating`, `destructive` or `external`";

fn parse_method_overrides(markers: &[syn::Attribute]) -> syn::Result<MethodOverrides> {
    let mut out = MethodOverrides::default();
    for attr in markers {
        if matches!(attr.meta, syn::Meta::Path(_)) {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("safety") {
                let ident: Ident = meta.value()?.parse()?;
                let spelling = ident.to_string();
                let variant = safety_variant(&spelling).ok_or_else(|| {
                    syn::Error::new(
                        ident.span(),
                        format!("unknown safety class `{spelling}`; one of {SAFETY_VOCABULARY}"),
                    )
                })?;
                out.safety = Some(variant.to_string());
                Ok(())
            } else if meta.path.is_ident("idempotent") {
                let flag: syn::LitBool = meta.value()?.parse()?;
                out.idempotent = Some(flag.value);
                Ok(())
            } else if meta.path.is_ident("effects") {
                out.effects = Some(parse_effects_attr(&meta)?);
                Ok(())
            } else if meta.path.is_ident("deprecated") {
                out.deprecated = Some(parse_deprecated_attr(&meta)?);
                Ok(())
            } else if meta.path.is_ident("aliases") {
                out.aliases = parse_aliases_attr(&meta)?;
                Ok(())
            } else if meta.path.is_ident("budget_ms") {
                let lit: syn::LitInt = meta.value()?.parse()?;
                out.budget_ms = Some(lit);
                Ok(())
            } else if meta.path.is_ident("replay") {
                let mode: Ident = meta.value()?.parse()?;
                if mode != "full" {
                    return Err(syn::Error::new(mode.span(), "replay must be `full`"));
                }
                out.replay_full = true;
                Ok(())
            } else {
                Err(meta.error(
                    "unknown #[impress_method] key; `safety = …`, `idempotent = …`, \
                     `effects(…)`, `deprecated(…)`, `aliases = […]`, `budget_ms = …` or `replay = full`",
                ))
            }
        })?;
    }
    if !out.aliases.is_empty() && out.deprecated.is_none() {
        return Err(syn::Error::new(
            out.aliases[0].span(),
            "`aliases = […]` needs `deprecated(since = \"…\", note = \"…\")` on the same \
             method — a rename implies deprecating the old name",
        ));
    }
    Ok(out)
}

/// `deprecated(since = "…", note = "…")` inside `#[impress_method(…)]`.
fn parse_deprecated_attr(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<DeprecatedDecl> {
    let content;
    syn::parenthesized!(content in meta.input);
    let mut since: Option<syn::LitStr> = None;
    let mut note: Option<syn::LitStr> = None;
    while !content.is_empty() {
        let key: Ident = content.parse()?;
        content.parse::<syn::Token![=]>()?;
        if key == "since" {
            since = Some(content.parse()?);
        } else if key == "note" {
            note = Some(content.parse()?);
        } else {
            return Err(syn::Error::new(
                key.span(),
                "unknown deprecated key; `since` or `note`",
            ));
        }
        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        }
    }
    Ok(DeprecatedDecl {
        since: since.ok_or_else(|| {
            syn::Error::new_spanned(&meta.path, "deprecated needs `since = \"…\"`")
        })?,
        note: note.ok_or_else(|| {
            syn::Error::new_spanned(&meta.path, "deprecated needs `note = \"…\"`")
        })?,
    })
}

/// `aliases = ["old-name", …]` inside `#[impress_method(…)]`.
fn parse_aliases_attr(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<Vec<syn::LitStr>> {
    let value = meta.value()?;
    let content;
    syn::bracketed!(content in value);
    let mut out = Vec::new();
    while !content.is_empty() {
        out.push(content.parse()?);
        if content.peek(syn::Token![,]) {
            content.parse::<syn::Token![,]>()?;
        }
    }
    Ok(out)
}

/// `#[impress_example(name = "…", args = r#"{…}"#, expect = r#"{…}"#, tier = "b")]` as
/// an `impress_service_core::Example` literal. `args` and `expect` must be
/// JSON text, and `args` an object: a typo here is a compile error, not a
/// Tier A failure later.
fn parse_example(attr: &syn::Attribute) -> syn::Result<TokenStream2> {
    let mut name: Option<syn::LitStr> = None;
    let mut args: Option<syn::LitStr> = None;
    let mut expect: Option<syn::LitStr> = None;
    let mut tier: Option<syn::LitStr> = None;
    attr.parse_nested_meta(|meta| {
        let value: syn::LitStr = meta.value()?.parse()?;
        if meta.path.is_ident("name") {
            name = Some(value);
        } else if meta.path.is_ident("args") {
            match serde_json::from_str::<serde_json::Value>(&value.value()) {
                Ok(serde_json::Value::Object(_)) => {}
                Ok(_) => {
                    return Err(syn::Error::new(
                        value.span(),
                        "`args` must be a JSON object",
                    ));
                }
                Err(e) => {
                    return Err(syn::Error::new(
                        value.span(),
                        format!("`args` is not JSON: {e}"),
                    ));
                }
            }
            args = Some(value);
        } else if meta.path.is_ident("expect") {
            if let Err(e) = serde_json::from_str::<serde_json::Value>(&value.value()) {
                return Err(syn::Error::new(
                    value.span(),
                    format!("`expect` is not JSON: {e}"),
                ));
            }
            expect = Some(value);
        } else if meta.path.is_ident("tier") {
            if tier.is_some() {
                return Err(syn::Error::new(
                    value.span(),
                    "duplicate `tier` in #[impress_example]",
                ));
            }
            if !matches!(value.value().as_str(), "a" | "b") {
                return Err(syn::Error::new(value.span(), "`tier` must be `a` or `b`"));
            }
            tier = Some(value);
        } else {
            return Err(
                meta.error("unknown #[impress_example] key; `name`, `args`, `expect` or `tier`")
            );
        }
        Ok(())
    })?;
    let name = name
        .ok_or_else(|| syn::Error::new_spanned(attr, "#[impress_example] needs `name = \"…\"`"))?;
    let args = args.ok_or_else(|| {
        syn::Error::new_spanned(attr, "#[impress_example] needs `args = r#\"{…}\"#`")
    })?;
    let expect = match expect {
        Some(e) => quote! { ::core::option::Option::Some(#e) },
        None => quote! { ::core::option::Option::None },
    };
    let tier = if tier.as_ref().is_some_and(|value| value.value() == "b") {
        quote! { ::impress_service_core::ExampleTier::B }
    } else {
        quote! { ::impress_service_core::ExampleTier::A }
    };
    Ok(quote! {
        ::impress_service_core::Example { name: #name, args: #args, expect: #expect, tier: #tier }
    })
}

/// `#[impress_method]` marker on a trait method, optionally
/// `#[impress_method(safety = destructive, idempotent = false)]` to depart
/// from the service's declared default.
///
/// This is a marker consumed by [`macro@impress_service`]. Defining it as its
/// own attribute macro lets users write the attribute on trait methods
/// without the compiler complaining about an unknown attribute. If invoked
/// directly on a free-standing item (which should not happen in normal use)
/// it returns the item unchanged.
#[proc_macro_attribute]
pub fn impress_method(_attr: TokenStream, input: TokenStream) -> TokenStream {
    input
}

/// `#[impress_example(name = "…", args = r#"{…}"#, expect = r#"{…}"#)]` on an
/// `#[impress_method]`: a named example call, stored on the verb's descriptor
/// (`VerbDescriptor::examples`). Repeatable. A marker consumed by
/// [`macro@impress_service`], like `impress_method`.
#[proc_macro_attribute]
pub fn impress_example(_attr: TokenStream, input: TokenStream) -> TokenStream {
    input
}

// ---------------------------------------------------------------------------
// impress_service_impl! — the workhorse function-like macro
// ---------------------------------------------------------------------------

struct ImplMacroInput {
    service: Ident,
    instance: syn::Expr,
    methods: Vec<MethodDecl>,
    /// By default, an argument object carrying a field the method's
    /// input schema does not name — or one that does not parse — is answered
    /// with a refusal envelope (`{"ok": false, "code": "invalid-argument",
    /// …}`) rather than parsed leniently or failed as a transport error. See
    /// `impress_service_core::strict`. `strict_args = false` is the explicit
    /// escape for a service whose callers still send extra keys.
    strict_args: bool,
    /// `safety = read_only`: the service's default class; a method departs
    /// from it with `#[impress_method(safety = …)]` on the trait.
    safety: String,
    /// `since = "0.1.0"`: the version every verb of the service appeared in.
    since: syn::LitStr,
    /// `effects = { reads: […], writes: […], reach: […] }`: the service's
    /// default effect set; a method departs from it with
    /// `#[impress_method(effects(…))]` on the trait (ADR-0036 D1).
    effects: EffectsDecl,
}

struct MethodDecl {
    name: Ident,
    doc: String,
    /// `(docs, name, type)`: `docs` are the argument's `///` lines, which
    /// become the args-struct field's doc attributes and so its JSON-schema
    /// `description` (MCP `inputSchema`, CLI `--help`).
    args: Vec<(Vec<syn::Attribute>, Ident, Type)>,
    /// Arguments marked `#[impress_private]`: their input-schema property
    /// carries `"x-private": true`, and the call log stores them as a length
    /// and a hash, never by value (ADR-0036 D2's privacy rule).
    private_args: Vec<String>,
    ret: Option<Type>,
}

impl syn::parse::Parse for ImplMacroInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        // Format (comma-separated fields, trailing comma allowed):
        //   service = TraitName,
        //   impl = TypeName,        // accepted but currently unused by the
        //                           // generated code (kept for symmetry +
        //                           // future Self-type-aware codegen)
        //   instance = || TypeName, // expression that yields a value
        //                           // implementing the trait
        //   methods = [
        //     name(arg: Type, ...) -> RetType,
        //     name(arg: Type, ...),            // implicit `-> ()`
        //     name(
        //         /// Becomes this argument's schema `description`.
        //         arg: Type,
        //     ) -> RetType,
        //   ],

        let mut service: Option<Ident> = None;
        let mut instance: Option<syn::Expr> = None;
        let mut methods: Option<Vec<MethodDecl>> = None;
        let mut strict_args = true;
        let mut safety: Option<String> = None;
        let mut since: Option<syn::LitStr> = None;
        let mut effects: Option<EffectsDecl> = None;

        while !input.is_empty() {
            // Accept both regular identifiers and keywords-as-identifiers
            // (notably `impl`, which is a reserved word). We peek-by-text to
            // keep the surface-level DSL readable.
            let key_text: String = if input.peek(syn::Token![impl]) {
                input.parse::<syn::Token![impl]>()?;
                "impl".to_string()
            } else {
                let id: Ident = input.parse()?;
                id.to_string()
            };
            input.parse::<syn::Token![=]>()?;
            match key_text.as_str() {
                "service" => service = Some(input.parse()?),
                "impl" => {
                    let _ty: Type = input.parse()?;
                }
                "instance" => instance = Some(input.parse()?),
                "strict_args" => {
                    let flag: syn::LitBool = input.parse()?;
                    strict_args = flag.value;
                }
                "safety" => {
                    let ident: Ident = input.parse()?;
                    let spelling = ident.to_string();
                    let variant = safety_variant(&spelling).ok_or_else(|| {
                        syn::Error::new(
                            ident.span(),
                            format!(
                                "unknown safety class `{spelling}`; one of {SAFETY_VOCABULARY}"
                            ),
                        )
                    })?;
                    safety = Some(variant.to_string());
                }
                "since" => {
                    let version: syn::LitStr = input.parse()?;
                    if version.value().trim().is_empty() {
                        return Err(syn::Error::new(
                            version.span(),
                            "`since` must name a version",
                        ));
                    }
                    since = Some(version);
                }
                "effects" => effects = Some(parse_effects_block(input)?),
                "methods" => {
                    let content;
                    syn::bracketed!(content in input);
                    let mut list = Vec::new();
                    while !content.is_empty() {
                        list.push(parse_method_decl(&content)?);
                        if content.peek(syn::Token![,]) {
                            content.parse::<syn::Token![,]>()?;
                        }
                    }
                    methods = Some(list);
                }
                other => {
                    return Err(syn::Error::new(
                        input.span(),
                        format!("unknown impress_service_impl! key `{other}`"),
                    ));
                }
            }
            if input.peek(syn::Token![,]) {
                input.parse::<syn::Token![,]>()?;
            }
        }

        Ok(Self {
            service: service
                .ok_or_else(|| syn::Error::new(input.span(), "missing `service = ...`"))?,
            instance: instance
                .ok_or_else(|| syn::Error::new(input.span(), "missing `instance = ...`"))?,
            methods: methods
                .ok_or_else(|| syn::Error::new(input.span(), "missing `methods = [...]`"))?,
            strict_args,
            safety: safety.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    format!(
                        "missing `safety = …` (the service's default class: {SAFETY_VOCABULARY})"
                    ),
                )
            })?,
            since: since
                .ok_or_else(|| syn::Error::new(input.span(), "missing `since = \"<version>\"`"))?,
            effects: effects.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "missing `effects = { reads: […], writes: […], reach: […] }` (the service's \
                     default effect set, ADR-0036 D1; `{}` for a service that touches nothing)",
                )
            })?,
        })
    }
}

fn parse_method_decl(input: syn::parse::ParseStream) -> syn::Result<MethodDecl> {
    // Optional `/// doc comment` lines as `#[doc = "..."]` attrs.
    let attrs = input.call(syn::Attribute::parse_outer)?;
    let doc = collect_doc(&attrs);

    let name: Ident = input.parse()?;
    let arg_content;
    syn::parenthesized!(arg_content in input);

    let mut args = Vec::new();
    let mut private_args = Vec::new();
    while !arg_content.is_empty() {
        // `/// doc` lines on an argument describe it in the generated schema;
        // `#[impress_private]` marks it `x-private`. Anything else is a
        // mistake here.
        let mut arg_attrs = arg_content.call(syn::Attribute::parse_outer)?;
        if let Some(bad) = arg_attrs
            .iter()
            .find(|a| !a.path().is_ident("doc") && !a.path().is_ident("impress_private"))
        {
            return Err(syn::Error::new_spanned(
                bad,
                "only `///` doc comments and `#[impress_private]` are allowed on an \
                 impress_service_impl! argument",
            ));
        }
        let private = arg_attrs
            .iter()
            .any(|a| a.path().is_ident("impress_private"));
        arg_attrs.retain(|a| a.path().is_ident("doc"));
        let arg_name: Ident = arg_content.parse()?;
        arg_content.parse::<syn::Token![:]>()?;
        let arg_ty: Type = arg_content.parse()?;
        if private {
            private_args.push(arg_name.to_string());
        }
        args.push((arg_attrs, arg_name, arg_ty));
        if arg_content.peek(syn::Token![,]) {
            arg_content.parse::<syn::Token![,]>()?;
        }
    }

    let ret = if input.peek(syn::Token![->]) {
        input.parse::<syn::Token![->]>()?;
        let ty: Type = input.parse()?;
        Some(ty)
    } else {
        None
    };

    Ok(MethodDecl {
        name,
        doc,
        args,
        private_args,
        ret,
    })
}

/// Join `///` lines into a description.
///
/// Rust doc comments are hard-wrapped at the source margin, so joining every
/// line with `\n` puts a break in the middle of each sentence. Consecutive
/// lines are one paragraph and join with a space; a blank `///` line is a
/// deliberate paragraph break and becomes `\n\n`.
fn collect_doc(attrs: &[syn::Attribute]) -> String {
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();

    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let syn::Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        let syn::Expr::Lit(lit) = &nv.value else {
            continue;
        };
        let syn::Lit::Str(s) = &lit.lit else {
            continue;
        };

        let line = s.value();
        let line = line.trim();
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs.join("\n\n")
}

fn kebab(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c == '_' {
            out.push('-');
        } else if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Expand `impress_service_impl!`.
#[proc_macro]
pub fn impress_service_impl(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as ImplMacroInput);
    match expand_impl(parsed) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_impl(input: ImplMacroInput) -> syn::Result<TokenStream2> {
    let service = &input.service;
    let instance = &input.instance;

    let mut emitted = Vec::new();
    for method in &input.methods {
        emitted.push(expand_method(service, instance, method, &input)?);
    }

    Ok(quote! {
        #(#emitted)*
    })
}

fn expand_method(
    service: &Ident,
    instance: &syn::Expr,
    method: &MethodDecl,
    input: &ImplMacroInput,
) -> syn::Result<TokenStream2> {
    let strict_args = input.strict_args;
    let name = &method.name;
    let kebab_name = kebab(&name.to_string());
    let service_kebab = kebab(&service.to_string());

    // Descriptions resolve in two places, at compile time: a `///` here in
    // `methods = [...]`, then the trait method's own doc comment (captured by
    // `#[impress_service]` into the table below). Before the table existed
    // only the first was read, so services that documented their trait —
    // nearly all of them — shipped "Invoke Service.method" to the model.
    let inline_doc = method.doc.clone();
    let fallback = format!("Invoke {service}.{name}");
    let meta_table = format_ident!("__IMPRESS_SERVICE_METHODS_{}", service);
    let method_name_str = name.to_string();
    let doc = quote! {
        ::impress_service_core::resolve_description(
            #inline_doc,
            &#meta_table,
            #method_name_str,
            #fallback,
        )
    };
    // A methods-list entry may name a trait method without #[impress_method],
    // so the attribute macro alone cannot guarantee its description. Check
    // the resolved value in a const: this emits a compile error naming the
    // service and method instead of publishing the generic fallback.
    let doc_required = quote! {
        const _: () = {
            if ::impress_service_core::resolve_description(
                #inline_doc, &#meta_table, #method_name_str, ""
            ).is_empty() {
                panic!(concat!("impress_service_impl! `", stringify!(#service), "::",
                    stringify!(#name), "` has no doc comment"));
            }
        };
    };
    let default_safety = format_ident!("{}", input.safety);
    let since = &input.since;
    let default_effects = input.effects.literal();
    let private_args = &method.private_args;

    let args_struct = format_ident!("__Impress_{}_{}_Args", service, name);
    let invoker_fn = format_ident!("__impress_{}_{}_invoke", service, name);
    let schema_fn = format_ident!("__impress_{}_{}_schema", service, name);
    let output_schema_fn = format_ident!("__impress_output_schema_{}_{}", service, name);
    let verb_static = format_ident!("__IMPRESS_VERB_{}_{}", service, name);
    let ret_ty: Type = match &method.ret {
        Some(ty) => ty.clone(),
        None => syn::parse_quote!(()),
    };

    // Build the args struct fields and the deserialization → call expression.
    let mut struct_fields = Vec::new();
    let mut arg_idents = Vec::new();
    for (arg_attrs, arg_name, arg_ty) in &method.args {
        validate_supported_ty(arg_ty)?;
        struct_fields.push(quote! {
            #(#arg_attrs)*
            pub #arg_name: #arg_ty,
        });
        arg_idents.push(arg_name.clone());
    }

    // Return-type serialization: () → null, anything else → serde_json::to_value.
    let serialize_ret = match &method.ret {
        Some(ty) => {
            validate_supported_ty(ty)?;
            quote! {
                let value = ::impress_service_core::serde_json::to_value(__out)
                    .map_err(|e| -> ::impress_service_core::BoxError { Box::new(e) })?;
                Ok(value)
            }
        }
        None => quote! {
            Ok(::impress_service_core::serde_json::Value::Null)
        },
    };

    // Build the trait method call using method-call syntax. This dispatches
    // through Rust's auto-deref + auto-borrow rules, so the `instance` closure
    // can return any of:
    //   * an owned value:           `|| DefaultFoo::default()`
    //   * a `&'static` reference:   `|| GLOBAL.get().unwrap()`
    //   * an `Arc<T>` / smart ptr:  `|| GLOBAL.clone()`
    // all uniformly. UFCS (`<_ as Trait>::method(&inst, ...)`) only worked
    // for the owned case because `&&T` does not coerce to `&T` in UFCS.
    //
    // Requires the trait to be in scope at the macro call site — it always
    // is in practice (services co-locate trait + impl + this macro call).
    let _ = service; // suppress unused-variable lint for future use
    let call_expr = quote! {
        __instance.#name(#( __args.#arg_idents ),*).await
    };

    // The invoker parses nothing but its own args struct (plan-verb-pipeline
    // P2): the strict schema check is the pipeline's strict-args layer,
    // which runs before this handler for every verb of a `strict_args`
    // service. What stays here for a strict service is the envelope a type
    // error answers — `{ok: false, code: "invalid-argument", …}` naming the
    // tool, as before — so a refused argument is a result, not a transport
    // error, on every path.
    let parse_args = if strict_args {
        quote! {
            let __args: #args_struct = match ::impress_service_core::serde_json::from_value(__json) {
                Ok(args) => args,
                Err(e) => {
                    return Ok(::impress_service_core::strict::parse_refusal(
                        concat!(#service_kebab, "_", #kebab_name),
                        e,
                    ));
                }
            };
        }
    } else {
        quote! {
            let __args: #args_struct =
                ::impress_service_core::serde_json::from_value(__json)
                    .map_err(|e| -> ::impress_service_core::BoxError { Box::new(e) })?;
        }
    };

    Ok(quote! {
        #doc_required
        // -- Args struct -----------------------------------------------------
        #[doc(hidden)]
        #[derive(
            ::impress_service_core::schemars::JsonSchema,
            ::serde::Deserialize,
        )]
        #[allow(non_camel_case_types)]
        pub struct #args_struct {
            #(#struct_fields)*
        }

        // -- Schema functions ------------------------------------------------
        #[doc(hidden)]
        #[allow(non_snake_case)]
        pub fn #schema_fn() -> ::impress_service_core::serde_json::Value {
            let schema = ::impress_service_core::schemars::schema_for!(#args_struct);
            let schema = ::impress_service_core::serde_json::to_value(schema)
                .unwrap_or(::impress_service_core::serde_json::Value::Null);
            ::impress_service_core::mark_private(schema, &[#(#private_args),*])
        }

        // The output schema is derived from the return type, which is why
        // every result type implements `JsonSchema` (ADR-0034 D1).
        #[doc(hidden)]
        #[allow(non_snake_case)]
        pub fn #output_schema_fn() -> ::impress_service_core::serde_json::Value {
            let schema = ::impress_service_core::schemars::schema_for!(#ret_ty);
            ::impress_service_core::serde_json::to_value(schema)
                .unwrap_or(::impress_service_core::serde_json::Value::Null)
        }

        // -- Async invoker ---------------------------------------------------
        // `redundant_closure`: `instance = || foo()` is this macro's contract —
        // the closure defers construction to call time. Clippy attributes the
        // lint to the call site, so every service was being told to "fix" a
        // shape the macro requires. Silence it once, here.
        #[doc(hidden)]
        #[allow(non_snake_case, clippy::redundant_closure)]
        pub fn #invoker_fn(
            __json: ::impress_service_core::serde_json::Value,
        ) -> ::impress_service_core::ServiceFuture {
            Box::pin(async move {
                #parse_args
                let __instance = (#instance)();
                let __out = #call_expr;
                #serialize_ret
            })
        }

        // -- The verb descriptor (ADR-0034 D1) ------------------------------
        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        pub static #verb_static: ::impress_service_core::VerbDescriptor =
            ::impress_service_core::VerbDescriptor {
                name: concat!(#service_kebab, "_", #kebab_name),
                service: #service_kebab,
                method: #kebab_name,
                description: #doc,
                input_schema: #schema_fn,
                output_schema: #output_schema_fn,
                safety: ::impress_service_core::Safety {
                    class: ::impress_service_core::resolve_safety_class(
                        &#meta_table,
                        #method_name_str,
                        ::impress_service_core::SafetyClass::#default_safety,
                    ),
                    idempotent: ::impress_service_core::resolve_idempotent(
                        &#meta_table,
                        #method_name_str,
                        ::impress_service_core::resolve_safety_class(
                            &#meta_table,
                            #method_name_str,
                            ::impress_service_core::SafetyClass::#default_safety,
                        ),
                    ),
                },
                effects: ::impress_service_core::resolve_effects(
                    &#meta_table,
                    #method_name_str,
                    #default_effects,
                ),
                since: #since,
                deprecated: ::impress_service_core::resolve_deprecated(&#meta_table, #method_name_str),
                aliases: ::impress_service_core::resolve_aliases(&#meta_table, #method_name_str),
                examples: ::impress_service_core::resolve_examples(&#meta_table, #method_name_str),
                strict: #strict_args,
                budget_ms: ::impress_service_core::resolve_budget_ms(&#meta_table, #method_name_str),
                replay_full: ::impress_service_core::resolve_replay_full(&#meta_table, #method_name_str),
                source: ::impress_service_core::Source::Linked,
                handler: #invoker_fn,
            };

        // -- MCP and CLI projections into the inventory ---------------------
        ::impress_service_core::inventory::submit! {
            ::impress_service_core::McpToolDescriptor::of(&#verb_static)
        }
        ::impress_service_core::inventory::submit! {
            ::impress_service_core::CliSubcommand::of(&#verb_static)
        }
    })
}

/// Phase 0 supports a small allow-list of argument and return types.
///
/// We accept anything syntactically — the compiler still has to type-check
/// the generated code — but we surface a helpful error for things outside
/// the documented allow-list to keep the contract clear.
fn validate_supported_ty(ty: &Type) -> syn::Result<()> {
    let text = quote!(#ty).to_string().replace(' ', "");
    const ALLOWED: &[&str] = &[
        "String",
        "i64",
        "u64",
        "i32",
        "u32",
        "bool",
        "f64",
        "f32",
        "()",
        "Option<String>",
        "Vec<String>",
        "serde_json::Value",
        "Value",
    ];
    if ALLOWED.iter().any(|a| text == *a) {
        Ok(())
    } else {
        // Permit anything; warn via a doc-style note instead of an error so
        // that adventurous users can use custom DTOs that implement
        // `serde::Deserialize` + `schemars::JsonSchema`.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! The doc-comment rule (G-2) as a compile-fail test without trybuild:
    //! `expand_service` is the whole attribute minus the `proc_macro` wrapper,
    //! so a `syn::Error` here is exactly the diagnostic rustc prints.
    use super::*;

    fn expand(src: &str) -> syn::Result<TokenStream2> {
        expand_service(syn::parse_str::<ItemTrait>(src).expect("parses"))
    }

    #[test]
    fn strict_arguments_default_on_and_explicit_false_keeps_lenient_handler() {
        let base = "service = EchoService, instance = || Echo, safety = read_only, \
                    since = \"0.1.0\", effects = {}, methods = [echo(message: String) -> String]";
        let default: ImplMacroInput = syn::parse_str(base).expect("default parses");
        assert!(default.strict_args);
        let default_tokens = expand_impl(default).expect("default expands").to_string();
        assert!(default_tokens.contains("strict : true"), "{default_tokens}");
        assert!(
            default_tokens.contains("strict :: parse_refusal"),
            "{default_tokens}"
        );

        let lenient: ImplMacroInput =
            syn::parse_str(&base.replace("methods =", "strict_args = false, methods ="))
                .expect("explicit false parses");
        assert!(!lenient.strict_args);
        let lenient_tokens = expand_impl(lenient).expect("escape expands").to_string();
        assert!(
            lenient_tokens.contains("strict : false"),
            "{lenient_tokens}"
        );
        assert!(
            !lenient_tokens.contains("strict :: parse_refusal"),
            "{lenient_tokens}"
        );
    }

    #[test]
    fn method_without_doc_is_a_compile_error_naming_it() {
        let err = expand(
            r#"
            pub trait EchoService: Send + Sync + 'static {
                /// Echo a message.
                #[impress_method]
                async fn echo(&self, message: String) -> String;
                #[impress_method]
                async fn shout(&self, message: String) -> String;
            }
            "#,
        )
        .expect_err("an undocumented #[impress_method] must not expand");
        let msg = err.to_string();
        assert!(
            msg.contains("EchoService::shout"),
            "names the method: {msg}"
        );
        assert!(msg.contains("no doc comment"), "says why: {msg}");
    }

    #[test]
    fn blank_doc_lines_count_as_no_doc() {
        let err = expand(
            r#"
            pub trait EchoService: Send + Sync + 'static {
                ///
                #[impress_method]
                async fn echo(&self, message: String) -> String;
            }
            "#,
        )
        .expect_err("a doc comment with no text is no doc comment");
        assert!(err.to_string().contains("EchoService::echo"));
    }

    #[test]
    fn documented_methods_expand_with_their_docs() {
        let ts = expand(
            r#"
            pub trait EchoService: Send + Sync + 'static {
                /// Echo a message
                /// back.
                #[impress_method]
                async fn echo(&self, message: String) -> String;
                /// Not a verb: no marker, so no doc is required.
                async fn helper(&self);
            }
            "#,
        )
        .expect("documented trait expands");
        let text = ts.to_string();
        assert!(text.contains("__IMPRESS_SERVICE_METHODS_EchoService"));
        assert!(text.contains("\"Echo a message back.\""), "{text}");
        assert!(
            !text.contains("impress_method"),
            "the marker is stripped: {text}"
        );
    }

    #[test]
    fn trait_without_methods_is_a_compile_error() {
        let err = expand("pub trait Empty: Send + Sync + 'static { async fn f(&self); }")
            .expect_err("no #[impress_method] at all");
        assert!(err.to_string().contains("`Empty` has no #[impress_method]"));
    }

    #[test]
    fn replay_full_is_captured_and_other_modes_are_refused() {
        let ts = expand(
            "pub trait ReplayService: Send + Sync + 'static {\n\
             /// Replay this.\n\
             #[impress_method(replay = full)]\n\
             async fn replay(&self, id: String) -> String;\n}",
        )
        .expect("replay opt-in expands")
        .to_string();
        assert!(ts.contains("replay_full : true"), "{ts}");
        let err = expand(
            "pub trait ReplayService: Send + Sync + 'static {\n\
             /// Replay this.\n\
             #[impress_method(replay = summary)]\n\
             async fn replay(&self, id: String) -> String;\n}",
        )
        .expect_err("unknown replay mode must fail");
        assert!(err.to_string().contains("replay must be `full`"));
    }

    /// The per-method safety override and an example land in the method
    /// table as the `MethodMeta` fields `impress_service_impl!` resolves.
    #[test]
    fn safety_override_and_examples_are_captured() {
        let ts = expand(
            r##"
            pub trait EchoService: Send + Sync + 'static {
                /// Delete it.
                #[impress_method(safety = destructive, idempotent = true)]
                #[impress_example(name = "one", args = r#"{"id": "x"}"#, expect = r#"{"ok": true}"#)]
                async fn delete(&self, id: String) -> bool;
                /// Read it.
                #[impress_method]
                async fn get(&self, id: String) -> String;
            }
            "##,
        )
        .expect("expands")
        .to_string();
        assert!(ts.contains("SafetyClass :: Destructive"), "{ts}");
        assert!(
            ts.contains("idempotent : :: core :: option :: Option :: Some (true)"),
            "{ts}"
        );
        assert!(ts.contains("name : \"one\""), "{ts}");
        assert!(
            ts.contains("ExampleTier :: A"),
            "omitted tier defaults to A: {ts}"
        );
        assert!(
            !ts.contains("impress_example"),
            "the marker is stripped: {ts}"
        );
        // The undeclared method carries `None`s, not the other's values.
        assert!(
            ts.contains("safety : :: core :: option :: Option :: None"),
            "{ts}"
        );
    }

    #[test]
    fn explicit_tier_b_is_captured_and_invalid_tiers_are_compile_errors() {
        let service = |tier: &str| {
            format!(
                "pub trait EchoService: Send + Sync + 'static {{\n\
                 /// Echo it.\n\
                 #[impress_method]\n\
                 #[impress_example(name = \"one\", args = \"{{}}\", tier = {tier})]\n\
                 async fn echo(&self) -> String;\n}}"
            )
        };
        let tokens = expand(&service("\"b\""))
            .expect("tier B expands")
            .to_string();
        assert!(tokens.contains("ExampleTier :: B"), "{tokens}");
        let error = expand(&service("\"network\"")).expect_err("unknown tier must fail");
        assert!(error.to_string().contains("`tier` must be `a` or `b`"));
        let error = expand(&service("b")).expect_err("tier must be a string literal");
        assert!(error.to_string().contains("expected string literal"));
        let duplicate = service("\"b\"").replace("tier = \"b\"", "tier = \"b\", tier = \"a\"");
        let error = expand(&duplicate).expect_err("duplicate tier must fail");
        assert!(error.to_string().contains("duplicate `tier`"));
    }

    #[test]
    fn an_unknown_safety_class_and_a_non_json_example_are_compile_errors() {
        let err = expand(
            r#"
            pub trait EchoService: Send + Sync + 'static {
                /// Doc.
                #[impress_method(safety = harmless)]
                async fn echo(&self) -> String;
            }
            "#,
        )
        .expect_err("outside the vocabulary");
        assert!(
            err.to_string().contains("unknown safety class `harmless`"),
            "{err}"
        );

        let err = expand(
            r#"
            pub trait EchoService: Send + Sync + 'static {
                /// Doc.
                #[impress_method]
                #[impress_example(name = "bad", args = "[1, 2]")]
                async fn echo(&self) -> String;
            }
            "#,
        )
        .expect_err("args must be an object");
        assert!(
            err.to_string().contains("`args` must be a JSON object"),
            "{err}"
        );

        let err = expand(
            r#"
            pub trait EchoService: Send + Sync + 'static {
                /// Doc.
                #[impress_example(name = "stray", args = "{}")]
                async fn helper(&self) -> String;
                /// Doc.
                #[impress_method]
                async fn echo(&self) -> String;
            }
            "#,
        )
        .expect_err("an example on a non-verb");
        assert!(
            err.to_string().contains("not an #[impress_method]"),
            "{err}"
        );
    }

    #[test]
    fn the_impl_macro_requires_safety_and_since() {
        let parse = |src: &str| syn::parse_str::<ImplMacroInput>(src);
        let err = parse(
            "service = EchoService, impl = DemoEcho, instance = || DemoEcho, methods = [echo(m: String) -> String]",
        )
        .err()
        .expect("no safety");
        assert!(err.to_string().contains("missing `safety = …`"), "{err}");
        let err = parse(
            "service = EchoService, impl = DemoEcho, instance = || DemoEcho, safety = read_only, methods = []",
        )
        .err()
        .expect("no since");
        assert!(err.to_string().contains("missing `since"), "{err}");
        let err = parse(
            "service = EchoService, impl = DemoEcho, instance = || DemoEcho, safety = mutating, since = \"0.1.0\", methods = []",
        )
        .err()
        .expect("no effects");
        assert!(err.to_string().contains("missing `effects"), "{err}");
        let ok = parse(
            "service = EchoService, impl = DemoEcho, instance = || DemoEcho, safety = mutating, since = \"0.1.0\", effects = {}, methods = []",
        )
        .unwrap_or_else(|e| panic!("all declared: {e}"));
        assert_eq!(ok.safety, "Mutating");
        assert_eq!(ok.since.value(), "0.1.0");
        assert!(ok.effects.reads.is_empty() && ok.effects.writes.is_empty());
    }

    /// The service-level effect set: every kind and reach spelling, and a ref
    /// checked against the manifest embedded at build time.
    #[test]
    fn the_impl_macro_parses_the_effects_vocabulary() {
        let parse = |src: &str| syn::parse_str::<ImplMacroInput>(src);
        let ok = parse(
            r#"service = S, impl = D, instance = || D, safety = mutating, since = "0.1", methods = [],
               effects = { reads: ["imbib/bibliography-entry", target(id), children(collection_id), prefix("impress/ui/")],
                           writes: [any("dispatches whatever the surface calls")],
                           reach: [fs, network, subprocess, device, provider, app("imbib")] }"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let text = ok.effects.literal().to_string();
        assert!(
            text.contains("Kind :: Ref (\"imbib/bibliography-entry\")"),
            "{text}"
        );
        assert!(text.contains("Kind :: Target (\"id\")"), "{text}");
        assert!(
            text.contains("Kind :: Children (\"collection_id\")"),
            "{text}"
        );
        assert!(text.contains("Kind :: Prefix (\"impress/ui/\")"), "{text}");
        assert!(text.contains("Kind :: Any ("), "{text}");
        assert!(text.contains("Reach :: App (\"imbib\")"), "{text}");
        assert_eq!(ok.effects.reach.len(), 6);

        let err = parse(
            r#"service = S, impl = D, instance = || D, safety = mutating, since = "0.1", methods = [],
               effects = { reach: [telepathy] }"#,
        )
        .err()
        .expect("outside the vocabulary");
        assert!(
            err.to_string().contains("unknown reach `telepathy`"),
            "{err}"
        );

        let err = parse(
            r#"service = S, impl = D, instance = || D, safety = mutating, since = "0.1", methods = [],
               effects = { reads: [nothing(x)] }"#,
        )
        .err()
        .expect("outside the vocabulary");
        assert!(err.to_string().contains("unknown kind `nothing`"), "{err}");

        // The manifest check runs whenever the manifest was beside the
        // workspace at build time, which it is in this repository.
        if !schema_refs::CANONICAL_SCHEMA_REFS.is_empty() {
            let err = parse(
                r#"service = S, impl = D, instance = || D, safety = mutating, since = "0.1", methods = [],
                   effects = { reads: ["imbib/bibliography-entry@1.0.0"] }"#,
            )
            .err()
            .expect("a misspelt ref");
            assert!(
                err.to_string().contains("not a canonical schema ref"),
                "{err}"
            );
        }
    }

    /// A method's `effects(…)` replaces the service's set and lands in the
    /// method table; the undeclared method carries `None`.
    #[test]
    fn method_effects_are_captured() {
        let ts = expand(
            r#"
            pub trait S: Send + Sync + 'static {
                /// Star it.
                #[impress_method(effects(writes = [target(id)], reach = [fs]))]
                async fn set_starred(&self, id: String) -> bool;
                /// Read it.
                #[impress_method]
                async fn get(&self, id: String) -> String;
            }
            "#,
        )
        .expect("expands")
        .to_string();
        assert!(ts.contains("Kind :: Target (\"id\")"), "{ts}");
        assert!(ts.contains("Reach :: Fs"), "{ts}");
        assert!(
            ts.contains("effects : :: core :: option :: Option :: None"),
            "{ts}"
        );
        let err = expand(
            r#"
            pub trait S: Send + Sync + 'static {
                /// Doc.
                #[impress_method(effects(touches = [target(id)]))]
                async fn f(&self, id: String) -> bool;
            }
            "#,
        )
        .expect_err("unknown effects key");
        assert!(err.to_string().contains("unknown effects key"), "{err}");
    }

    /// `deprecated(since = "…", note = "…")` and `aliases = […]` (P3) land
    /// in the method table.
    #[test]
    fn deprecated_and_aliases_are_captured() {
        let ts = expand(
            r#"
            pub trait S: Send + Sync + 'static {
                /// Delete it, the new way.
                #[impress_method]
                async fn delete(&self, id: String) -> bool;
                /// Delete it, the old way.
                #[impress_method(
                    deprecated(since = "0.9.0", note = "renamed to delete"),
                    aliases = ["remove", "destroy"]
                )]
                async fn old_delete(&self, id: String) -> bool;
            }
            "#,
        )
        .expect("expands")
        .to_string();
        assert!(ts.contains("\"remove\""), "{ts}");
        assert!(ts.contains("\"destroy\""), "{ts}");
        assert!(ts.contains("\"0.9.0\""), "{ts}");
        assert!(ts.contains("\"renamed to delete\""), "{ts}");
        assert!(
            ts.contains("alias_of : :: core :: option :: Option :: None"),
            "{ts}"
        );
        // The undeclared method carries an empty alias list and no
        // deprecation.
        assert!(
            ts.contains("aliases : & []") || ts.contains("aliases : &[]"),
            "{ts}"
        );
    }

    /// `aliases = […]` with no `deprecated(…)` is a compile error: a rename
    /// implies deprecating the old name.
    #[test]
    fn aliases_without_deprecated_is_a_compile_error() {
        let err = expand(
            r#"
            pub trait S: Send + Sync + 'static {
                /// Doc.
                #[impress_method(aliases = ["old"])]
                async fn f(&self, id: String) -> bool;
            }
            "#,
        )
        .expect_err("aliases needs deprecated");
        assert!(err.to_string().contains("needs `deprecated"), "{err}");
    }

    /// An unknown `deprecated(…)` key is a compile error.
    #[test]
    fn unknown_deprecated_key_is_a_compile_error() {
        let err = expand(
            r#"
            pub trait S: Send + Sync + 'static {
                /// Doc.
                #[impress_method(deprecated(reason = "x"), aliases = ["old"])]
                async fn f(&self, id: String) -> bool;
            }
            "#,
        )
        .expect_err("unknown deprecated key");
        assert!(err.to_string().contains("unknown deprecated key"), "{err}");
    }

    /// `#[impress_private]` on a `methods = […]` argument is recorded (and
    /// stripped from the field, so the args struct stays plain serde).
    #[test]
    fn private_arguments_are_recorded() {
        let ok = syn::parse_str::<ImplMacroInput>(
            r#"service = S, impl = D, instance = || D, safety = mutating, since = "0.1", effects = {},
               methods = [ note(id: String, #[impress_private] body: String) -> bool ]"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ok.methods[0].private_args, vec!["body".to_string()]);
        assert!(ok.methods[0]
            .args
            .iter()
            .all(|(attrs, _, _)| attrs.is_empty()));
        let err = syn::parse_str::<ImplMacroInput>(
            r#"service = S, impl = D, instance = || D, safety = mutating, since = "0.1", effects = {},
               methods = [ note(#[serde(default)] body: String) ]"#,
        )
        .err()
        .expect("only doc and impress_private");
        assert!(err.to_string().contains("impress_private"), "{err}");
    }
}
