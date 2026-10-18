//! A cloneable descriptor that can own runtime metadata without changing the
//! generated, static linked descriptors.

use std::borrow::Cow;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::adverb::Adverb;
use crate::descriptor::{AdjectiveDecl, Effects, ExampleTier, Reach, Safety, VerbDescriptor};
use crate::provider::{ProviderStatus, ProviderVerb};

const PROVIDER_EFFECTS: Effects = Effects {
    reads: &[],
    writes: &[],
    reach: &[Reach::Provider],
};

#[derive(Debug, Clone)]
pub enum VerbHandle {
    Linked(&'static VerbDescriptor),
    Provider(Arc<ProviderVerb>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleSource<'a> {
    Linked,
    Provider(&'a str),
}

#[derive(Debug, Clone, Copy)]
pub struct ExampleView<'a> {
    pub name: &'a str,
    pub args: &'a str,
    pub expect: Option<&'a str>,
    pub tier: ExampleTier,
}

impl ExampleView<'_> {
    /// Parse the declared argument object as static `Example::args_value` does.
    pub fn args_value(&self) -> Value {
        serde_json::from_str(self.args).unwrap_or(Value::Null)
    }
}

impl VerbHandle {
    pub fn linked(&self) -> Option<&'static VerbDescriptor> {
        match self {
            Self::Linked(verb) => Some(verb),
            Self::Provider(_) => None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.name,
            Self::Provider(verb) => &verb.name,
        }
    }

    pub fn service(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.service,
            Self::Provider(verb) => &verb.service,
        }
    }

    pub fn method(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.method,
            Self::Provider(verb) => &verb.method,
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.description,
            Self::Provider(verb) => &verb.description,
        }
    }

    pub fn since(&self) -> &str {
        match self {
            Self::Linked(verb) => verb.since,
            Self::Provider(verb) => &verb.since,
        }
    }

    pub fn safety(&self) -> Safety {
        match self {
            Self::Linked(verb) => verb.safety,
            Self::Provider(verb) => verb.effective_safety,
        }
    }

    pub fn mcp_annotations(&self) -> Value {
        self.safety().mcp_annotations()
    }

    /// A sha256 of what a caller relies on: the name, the input and output
    /// schemas, the safety class and idempotence, and the effects
    /// (plan-koine-trust TR-D2). It changes exactly when a grant made for
    /// the verb should be asked for again. A linked verb's is computed once
    /// per process; a provider's each time, since a provider may re-register
    /// with a changed descriptor.
    pub fn fingerprint(&self) -> String {
        match self {
            Self::Linked(verb) => {
                static CACHE: std::sync::LazyLock<
                    std::sync::Mutex<std::collections::HashMap<usize, String>>,
                > = std::sync::LazyLock::new(Default::default);
                let key = *verb as *const VerbDescriptor as usize;
                if let Some(hit) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
                    return hit.clone();
                }
                let fingerprint = self.compute_fingerprint();
                CACHE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, fingerprint.clone());
                fingerprint
            }
            Self::Provider(_) => self.compute_fingerprint(),
        }
    }

    fn compute_fingerprint(&self) -> String {
        let safety = self.safety();
        let facts = serde_json::json!({
            "name": self.name(),
            "input": self.input_schema().as_ref(),
            "output": self.output_schema().as_ref(),
            "safety": safety.class.as_str(),
            "idempotent": safety.idempotent,
            "effects": format!("{:?}", self.effects()),
            "impact": self.consent().impact_for(safety.class).as_str(),
            "risk": format!("{:?}", self.consent().risk),
        });
        crate::pipeline::adverbs::fingerprint(&facts)
    }

    pub fn effects(&self) -> Effects {
        match self {
            Self::Linked(verb) => verb.effects,
            Self::Provider(_) => PROVIDER_EFFECTS,
        }
    }

    pub fn strict(&self) -> bool {
        match self {
            Self::Linked(verb) => verb.strict,
            Self::Provider(_) => true,
        }
    }

    pub fn budget_ms(&self) -> Option<u64> {
        match self {
            Self::Linked(verb) => verb.budget_ms,
            Self::Provider(_) => None,
        }
    }

    /// Whether the verb honors `adverb`. A runtime provider declares no
    /// adverb support, so only the universal three.
    pub fn supports(&self, adverb: Adverb) -> bool {
        match self {
            Self::Linked(verb) => verb.supports(adverb),
            Self::Provider(_) => adverb.is_universal(),
        }
    }

    /// The derived adjective the verb defines; never a provider's.
    pub fn adjective(&self) -> Option<AdjectiveDecl> {
        match self {
            Self::Linked(verb) => verb.adjective,
            Self::Provider(_) => None,
        }
    }

    /// The pane-query kind id of the rows the verb's result carries
    /// (`#[impress_method(returns_kind = "…")]`, plan-koine-host H-D8); a
    /// provider's verbs declare none.
    pub fn returns_kind(&self) -> Option<&'static str> {
        match self {
            Self::Linked(verb) => verb.returns_kind,
            Self::Provider(_) => None,
        }
    }

    /// Which fields of the verb's rows are the id, the title and the columns
    /// (`#[impress_method(rows(…))]`, plan-koine-followups F-D2); a
    /// provider's verbs declare none.
    pub fn rows(&self) -> Option<crate::descriptor::RowSchema> {
        match self {
            Self::Linked(verb) => verb.rows,
            Self::Provider(_) => None,
        }
    }

    /// The row columns a table of this verb shows: the declared list, else
    /// the row object's properties in schema order. `None` without `rows(…)`.
    pub fn row_columns(&self) -> Option<Vec<String>> {
        let view = self.linked()?.rows_view()?;
        Some(
            view["columns"]
                .as_array()?
                .iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect(),
        )
    }

    /// The secrets the verb declares (`#[impress_method(secrets = […])]`,
    /// plan-koine-followups F-D1); a provider's verbs declare none.
    /// How it takes part in App Intents (TR-D14); a provider verb never does.
    pub fn intent(&self) -> crate::IntentDecl {
        match self {
            Self::Linked(verb) => verb.intent,
            Self::Provider(_) => crate::IntentDecl::NONE,
        }
    }

    /// How a person is asked about this verb (TR-D6); a provider verb
    /// declares nothing beyond its safety class.
    pub fn consent(&self) -> crate::Consent {
        match self {
            Self::Linked(verb) => verb.consent,
            Self::Provider(_) => crate::Consent::DEFAULT,
        }
    }

    pub fn secrets(&self) -> &'static [&'static str] {
        match self {
            Self::Linked(verb) => verb.secrets,
            Self::Provider(_) => &[],
        }
    }

    /// The verb's risk in words; see [`VerbDescriptor::risk_words`]. A
    /// provider verb that leaves the process sends data out.
    pub fn risk_words(&self) -> Vec<&'static str> {
        match self {
            Self::Linked(verb) => verb.risk_words(),
            Self::Provider(_) if self.safety().class == crate::SafetyClass::External => {
                vec!["egress"]
            }
            Self::Provider(_) => Vec::new(),
        }
    }

    /// The additive MCP `_meta` object for a `tools/list` entry; see
    /// [`VerbDescriptor::mcp_meta`].
    pub fn mcp_meta(&self) -> Option<Value> {
        self.linked().and_then(VerbDescriptor::mcp_meta)
    }

    pub fn replay_full(&self) -> bool {
        match self {
            Self::Linked(verb) => verb.replay_full,
            Self::Provider(_) => false,
        }
    }

    pub fn source(&self) -> HandleSource<'_> {
        match self {
            Self::Linked(_) => HandleSource::Linked,
            Self::Provider(verb) => HandleSource::Provider(&verb.provider_id),
        }
    }

    /// The linked schema is generated on demand here; the pipeline's
    /// existing static cache can still be used through [`Self::linked`].
    pub fn input_schema(&self) -> Cow<'_, Value> {
        match self {
            Self::Linked(verb) => Cow::Owned((verb.input_schema)()),
            Self::Provider(verb) => Cow::Borrowed(&verb.input_schema),
        }
    }

    pub fn output_schema(&self) -> Cow<'_, Value> {
        match self {
            Self::Linked(verb) => Cow::Owned((verb.output_schema)()),
            Self::Provider(verb) => Cow::Borrowed(&verb.output_schema),
        }
    }

    pub fn examples(&self) -> Vec<ExampleView<'_>> {
        match self {
            Self::Linked(verb) => verb
                .examples
                .iter()
                .map(|example| ExampleView {
                    name: example.name,
                    args: example.args,
                    expect: example.expect,
                    tier: example.tier,
                })
                .collect(),
            Self::Provider(verb) => verb
                .examples
                .iter()
                .map(|example| ExampleView {
                    name: &example.name,
                    args: &example.args,
                    expect: example.expect.as_deref(),
                    tier: ExampleTier::B,
                })
                .collect(),
        }
    }

    pub fn has_alias(&self, name: &str) -> bool {
        self.linked().is_some_and(|verb| verb.has_alias(name))
    }

    pub fn deprecation_notice(&self) -> Option<Value> {
        match self {
            Self::Linked(verb) => verb.deprecation_notice(),
            Self::Provider(verb) => verb.deprecated_since.as_ref().map(|since| {
                json!({
                    "since": since,
                    "use": verb.name,
                    "note": "provider no longer advertises this verb",
                })
            }),
        }
    }

    pub fn alias_deprecation_notice(&self) -> Option<Value> {
        self.linked()
            .and_then(VerbDescriptor::alias_deprecation_notice)
    }

    pub fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Linked(_) => None,
            Self::Provider(verb) => Some(&verb.provider_id),
        }
    }

    pub fn provider_status(&self) -> Option<ProviderStatus> {
        match self {
            Self::Linked(_) => None,
            Self::Provider(verb) => Some(verb.status),
        }
    }

    pub fn generation(&self) -> Option<u64> {
        match self {
            Self::Linked(_) => None,
            Self::Provider(verb) => Some(verb.generation),
        }
    }
}
