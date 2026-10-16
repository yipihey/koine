//! The task-category catalogue: the stable ids apps use when they ask for
//! "the model for library Q&A" or "the model for keyword tagging". Ported
//! from the Swift `AITaskCategory` table so both sides agree on ids; the
//! per-category model assignments live in the preferences file.
//!
//! [`TASK_CATEGORIES`] is the built-in table. A package adds more
//! (docs/plan-koine-ai-tasks.md P-D9): each is a `koine/ai-category@1.0.0`
//! row, and [`all_in`] is the table followed by every installed one whose id
//! the table does not already hold.

use impress_item::{AttributedStore, ItemQuery, SchemaRef, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskCategory {
    pub id: &'static str,
    pub name: &'static str,
    /// SF Symbol name for the GUI.
    pub icon: &'static str,
    pub description: &'static str,
    pub parent: Option<&'static str>,
    pub apps: &'static [&'static str],
    pub supports_comparison: bool,
}

impl TaskCategory {
    pub fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}

pub const TASK_CATEGORIES: &[TaskCategory] = &[
    // Roots.
    TaskCategory {
        id: "writing",
        name: "Writing",
        icon: "pencil.and.outline",
        description: "Text editing and improvement tasks",
        parent: None,
        apps: &["imprint"],
        supports_comparison: false,
    },
    TaskCategory {
        id: "research",
        name: "Research",
        icon: "magnifyingglass",
        description: "Academic research assistance and AI counsel conversations",
        parent: None,
        apps: &["imbib", "imprint", "impart"],
        supports_comparison: false,
    },
    TaskCategory {
        id: "citation",
        name: "Citations",
        icon: "quote.opening",
        description: "Citation finding and formatting",
        parent: None,
        apps: &["imprint", "imbib"],
        supports_comparison: false,
    },
    TaskCategory {
        id: "analysis",
        name: "Analysis",
        icon: "chart.bar.doc.horizontal",
        description: "Content analysis and review",
        parent: None,
        apps: &["imprint"],
        supports_comparison: false,
    },
    TaskCategory {
        id: "data",
        name: "Data",
        icon: "tablecells",
        description: "Data generation and interpretation",
        parent: None,
        apps: &["implore"],
        supports_comparison: false,
    },
    TaskCategory {
        id: "agent",
        name: "Background Agents",
        icon: "gearshape.2",
        description: "Model tiers for impel's background executors",
        parent: None,
        apps: &["impel"],
        supports_comparison: false,
    },
    // Writing.
    TaskCategory {
        id: "writing.rewrite",
        name: "Text Rewriting",
        icon: "arrow.2.squarepath",
        description: "Improve clarity, concision, and tone",
        parent: Some("writing"),
        apps: &["imprint"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "writing.grammar",
        name: "Grammar & Style",
        icon: "textformat.abc",
        description: "Spelling and grammar fixes",
        parent: Some("writing"),
        apps: &["imprint"],
        supports_comparison: true,
    },
    // Research.
    TaskCategory {
        id: "research.search",
        name: "Query Expansion",
        icon: "text.magnifyingglass",
        description: "Expand search queries with synonyms and related concepts",
        parent: Some("research"),
        apps: &["imbib"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "research.summarize",
        name: "Summarization",
        icon: "doc.text.magnifyingglass",
        description: "Generate abstract and document summaries",
        parent: Some("research"),
        apps: &["imbib", "imprint"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "research.discover",
        name: "Paper Discovery",
        icon: "sparkle.magnifyingglass",
        description: "Find related papers and suggestions",
        parent: Some("research"),
        apps: &["imbib"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "research.compare",
        name: "Paper Comparison",
        icon: "rectangle.2.swap",
        description: "Compare methods, evidence, and conclusions across publications",
        parent: Some("research"),
        apps: &["imbib"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "research.rag",
        name: "Library Q&A",
        icon: "books.vertical",
        description: "Answer questions grounded in the selected publication corpus",
        parent: Some("research"),
        apps: &["imbib"],
        supports_comparison: true,
    },
    // Citations.
    TaskCategory {
        id: "citation.find",
        name: "Citation Finding",
        icon: "doc.text.magnifyingglass",
        description: "Identify citations needed for claims",
        parent: Some("citation"),
        apps: &["imprint", "imbib"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "citation.format",
        name: "Citation Formatting",
        icon: "list.bullet.indent",
        description: "Generate and format BibTeX entries",
        parent: Some("citation"),
        apps: &["imbib"],
        supports_comparison: false,
    },
    // Analysis.
    TaskCategory {
        id: "analysis.review",
        name: "Content Review",
        icon: "checkmark.circle",
        description: "Review logical flow and arguments",
        parent: Some("analysis"),
        apps: &["imprint"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "analysis.explain",
        name: "Explanation",
        icon: "lightbulb",
        description: "Simplify or add detail to explanations",
        parent: Some("analysis"),
        apps: &["imprint"],
        supports_comparison: true,
    },
    // Data.
    TaskCategory {
        id: "data.generate",
        name: "Formula Generation",
        icon: "function",
        description: "Generate mathematical formulas from descriptions",
        parent: Some("data"),
        apps: &["implore"],
        supports_comparison: true,
    },
    TaskCategory {
        id: "data.interpret",
        name: "Data Interpretation",
        icon: "chart.xyaxis.line",
        description: "Describe statistical patterns and insights",
        parent: Some("data"),
        apps: &["implore"],
        supports_comparison: true,
    },
    // Background agents (impel-taskd executors). These never inherit the
    // interactive selection: a cloud model chosen for chat must not silently
    // start paying for background work.
    TaskCategory {
        id: "agent.classify",
        name: "Keyword Tagging",
        icon: "tag",
        description: "Propose tags for enriched publications",
        parent: Some("agent"),
        apps: &["impel"],
        supports_comparison: false,
    },
    TaskCategory {
        id: "agent.memory",
        name: "Memory Consolidation",
        icon: "brain",
        description: "Distil durable claims during memory consolidation",
        parent: Some("agent"),
        apps: &["impel"],
        supports_comparison: false,
    },
    TaskCategory {
        id: "agent.throughline",
        name: "Throughline Drafting",
        icon: "text.badge.checkmark",
        description: "Draft throughline proposals for review",
        parent: Some("agent"),
        apps: &["impel"],
        supports_comparison: false,
    },
    // The agent desk (docs/plan-koine-local-models.md LM-D11). Not impel's
    // executors, so no app lists them; the desk and the routing preset do.
    TaskCategory {
        id: "agent.route",
        name: "Request Routing",
        icon: "arrow.triangle.branch",
        description: "Route a request to a model",
        parent: Some("agent"),
        apps: &[],
        supports_comparison: false,
    },
    TaskCategory {
        id: "agent.frontier",
        name: "Strongest Model",
        icon: "bolt",
        description: "The strongest model, when asked for",
        parent: Some("agent"),
        apps: &[],
        supports_comparison: false,
    },
];

pub fn category(id: &str) -> Option<&'static TaskCategory> {
    TASK_CATEGORIES.iter().find(|category| category.id == id)
}

pub fn root_categories() -> impl Iterator<Item = &'static TaskCategory> {
    TASK_CATEGORIES.iter().filter(|category| category.is_root())
}

/// Leaf categories, optionally only those an app supports.
pub fn leaf_categories(app: Option<&str>) -> Vec<&'static TaskCategory> {
    TASK_CATEGORIES
        .iter()
        .filter(|category| !category.is_root())
        .filter(|category| app.is_none_or(|app| category.apps.contains(&app)))
        .collect()
}

/// Triage sees at most this much of a prompt: the on-device model's window
/// is small.
pub const TRIAGE_PROMPT_CHARS: usize = 2_000;

/// The structured call triage makes: a short instruction, the request's
/// prompt, and a schema whose only property is an enum of `ids`.
pub fn triage_request(model: &str, prompt: &str, ids: &[String]) -> crate::types::ChatRequest {
    let described: Vec<String> = ids
        .iter()
        .map(|id| match (id.as_str(), category(id)) {
            // The table describes these for settings, not for choosing.
            ("agent", _) => format!("- {id}: everyday questions and tasks; the default"),
            ("agent.frontier", _) => format!(
                "- {id}: difficult reasoning, proofs or careful review that need the strongest model"
            ),
            (_, Some(category)) => format!("- {id}: {}", category.description),
            (_, None) => format!("- {id}"),
        })
        .collect();
    let prompt: String = prompt.chars().take(TRIAGE_PROMPT_CHARS).collect();
    crate::types::ChatRequest {
        model: model.to_string(),
        messages: vec![
            crate::types::ModelMessage::text(
                crate::types::Role::System,
                format!(
                    "You route a request to the kind of model that should answer it. Answer \
                     with JSON naming one category.\n{}\nUse `agent` when unsure.",
                    described.join("\n")
                ),
            ),
            crate::types::ModelMessage::text(crate::types::Role::User, prompt),
        ],
        temperature: Some(0.0),
        max_tokens: 64,
        response_format: Some(crate::types::ResponseFormat::JsonSchema {
            name: "route".into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": { "category": { "type": "string", "enum": ids } },
                "required": ["category"]
            }),
            strict: true,
        }),
        ..crate::types::ChatRequest::default()
    }
}

/// The category in triage's answer, if it is one of `ids`.
pub fn parse_triage(content: &str, ids: &[String]) -> Option<String> {
    use serde_json::Value;
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    let answer: Value = serde_json::from_str(content.get(start..=end)?).ok()?;
    let category = answer["category"].as_str()?.trim();
    ids.iter().find(|id| id.as_str() == category).cloned()
}

/// The kind a package's category is stored as.
pub const AI_CATEGORY: &str = "koine/ai-category@1.0.0";

/// A category as a list shows it: built in, or declared by a package.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CategoryRow {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub description: String,
    pub parent: Option<String>,
    /// The apps that offer it; empty is every app.
    pub apps: Vec<String>,
    pub supports_comparison: bool,
    /// The package that declared it; `None` when built in.
    pub package: Option<String>,
}

impl From<&TaskCategory> for CategoryRow {
    fn from(category: &TaskCategory) -> Self {
        Self {
            id: category.id.into(),
            name: category.name.into(),
            icon: Some(category.icon.into()),
            description: category.description.into(),
            parent: category.parent.map(Into::into),
            apps: category.apps.iter().map(|app| app.to_string()).collect(),
            supports_comparison: category.supports_comparison,
            package: None,
        }
    }
}

fn text(payload: &std::collections::BTreeMap<String, Value>, field: &str) -> Option<String> {
    match payload.get(field) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The categories installed packages declared, in id order.
pub fn installed_in(store: &dyn AttributedStore) -> Vec<CategoryRow> {
    let rows = store
        .query(&ItemQuery {
            schema: Some(SchemaRef::from_stored(AI_CATEGORY.to_owned())),
            include_tags: false,
            include_references: false,
            ..Default::default()
        })
        .unwrap_or_default();
    let mut categories: Vec<CategoryRow> = rows
        .iter()
        .filter_map(|item| {
            Some(CategoryRow {
                id: text(&item.payload, "id")?,
                name: text(&item.payload, "name")?,
                icon: text(&item.payload, "icon"),
                description: text(&item.payload, "description").unwrap_or_default(),
                parent: text(&item.payload, "parent"),
                apps: match item.payload.get("apps") {
                    Some(Value::Array(apps)) => apps
                        .iter()
                        .filter_map(|app| match app {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                },
                supports_comparison: false,
                package: text(&item.payload, "package"),
            })
        })
        .collect();
    categories.sort_by(|a, b| a.id.cmp(&b.id));
    categories
}

/// The built-in table, then every installed category whose id it does not
/// hold.
pub fn all_in(store: &dyn AttributedStore) -> Vec<CategoryRow> {
    let mut all: Vec<CategoryRow> = TASK_CATEGORIES.iter().map(CategoryRow::from).collect();
    for category in installed_in(store) {
        if !all.iter().any(|known| known.id == category.id) {
            all.push(category);
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_parents_exist() {
        let mut seen = std::collections::BTreeSet::new();
        for category in TASK_CATEGORIES {
            assert!(seen.insert(category.id), "duplicate {}", category.id);
            if let Some(parent) = category.parent {
                let root = self::category(parent).expect(parent);
                assert!(root.is_root());
                assert!(category.id.starts_with(&format!("{parent}.")));
            }
        }
        assert_eq!(root_categories().count(), 6);
        assert_eq!(leaf_categories(None).len(), 18);
        assert_eq!(leaf_categories(Some("implore")).len(), 2);
        assert!(leaf_categories(Some("impel"))
            .iter()
            .all(|category| category.parent == Some("agent")));
        assert!(category("research.rag").unwrap().supports_comparison);
    }

    #[test]
    fn an_installed_category_follows_the_table_and_cannot_shadow_it() {
        use impress_item::ItemStore;
        let store = koine_store_memory::MemoryStore::new();
        let row = |id: &str, name: &str| {
            let mut payload = std::collections::BTreeMap::new();
            payload.insert("id".to_string(), Value::String(id.into()));
            payload.insert("name".to_string(), Value::String(name.into()));
            payload.insert("parent".to_string(), Value::String("research".into()));
            payload.insert("package".to_string(), Value::String("lab".into()));
            serde_json::from_value::<impress_item::Item>(serde_json::json!({
                "id": uuid::Uuid::new_v4(), "schema": AI_CATEGORY, "payload": {},
                "created": "2026-01-01T00:00:00Z", "modified": "2026-01-01T00:00:00Z",
                "author": "t", "author_kind": "System", "logical_clock": 0,
                "origin": null, "canonical_id": null, "tags": [], "flag": null,
                "is_read": false, "is_starred": false, "priority": "normal",
                "visibility": "private", "message_type": null, "produced_by": null,
                "version": null, "batch_id": null, "references": [], "parent": null
            }))
            .map(|mut item| {
                item.payload = payload;
                item
            })
            .unwrap()
        };
        store
            .insert(row("research.lab-notes", "Lab notes"))
            .unwrap();
        store
            .insert(row("research.rag", "Not the built-in one"))
            .unwrap();
        let all = all_in(&store);
        assert_eq!(all.len(), TASK_CATEGORIES.len() + 1);
        let added = all.last().unwrap();
        assert_eq!(added.id, "research.lab-notes");
        assert_eq!(added.package.as_deref(), Some("lab"));
        let rag = all.iter().find(|c| c.id == "research.rag").unwrap();
        assert!(rag.package.is_none(), "the built-in one wins");
        assert_eq!(installed_in(&store).len(), 2);
    }
}
