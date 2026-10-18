//! The generator over a small inventory (docs/plan-koine-trust.md TR-D14):
//! entities from lookup and search verbs, intents with typed and entity
//! parameters, confirmation for a destructive verb, a skip with a reason, and
//! deterministic output.

use impress_service_core::{call, VerbHandle};
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use koine_app_intents::{generate, AppInfo, GENERATED_MARK};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Paper {
    pub bibcode: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Papers {
    pub ok: bool,
    pub rows: Vec<Paper>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Nested {
    pub inner: Vec<Paper>,
}

#[impress_service]
pub trait PaperService: Send + Sync + 'static {
    /// Papers by id.
    #[impress_method(
        returns_kind = "paper",
        rows(id = "bibcode", title = "title"),
        entity(lookup)
    )]
    async fn get_papers(&self, ids: Vec<String>) -> Papers;

    /// Papers matching a query.
    #[impress_method(
        returns_kind = "paper",
        rows(id = "bibcode", title = "title"),
        entity(search)
    )]
    async fn search_papers(&self, query: String) -> Papers;

    /// Add a paper to the library.
    #[impress_method(
        safety = mutating,
        intent(title = "Add Paper", phrase = "Add a paper to ${applicationName}")
    )]
    async fn add_paper(&self, title: String, year: Option<i64>) -> Papers;

    /// Delete papers from the library.
    #[impress_method(
        safety = destructive,
        returns_kind = "paper",
        rows(id = "bibcode", title = "title"),
        targets = "ids",
        intent(title = "Delete Papers")
    )]
    async fn delete_papers(&self, ids: Vec<String>) -> Papers;

    /// Import a structure.
    #[impress_method(safety = mutating, intent(title = "Import"))]
    async fn import(&self, nested: Nested) -> Papers;

    /// One note by id.
    #[impress_method(returns_kind = "note", entity(lookup))]
    async fn get_note(&self, id: String) -> Option<Paper>;

    /// Notes matching a query.
    #[impress_method(
        returns_kind = "note",
        rows(id = "bibcode", title = "title"),
        entity(search)
    )]
    async fn search_notes(&self, query: String, limit: u32) -> Vec<Paper>;
}

pub struct Library;

#[async_trait::async_trait]
impl PaperService for Library {
    async fn get_papers(&self, _ids: Vec<String>) -> Papers {
        Papers::default()
    }
    async fn search_papers(&self, _query: String) -> Papers {
        Papers::default()
    }
    async fn add_paper(&self, _title: String, _year: Option<i64>) -> Papers {
        Papers::default()
    }
    async fn delete_papers(&self, _ids: Vec<String>) -> Papers {
        Papers::default()
    }
    async fn import(&self, _nested: Nested) -> Papers {
        Papers::default()
    }
    async fn get_note(&self, _id: String) -> Option<Paper> {
        None
    }
    async fn search_notes(&self, _query: String, _limit: u32) -> Vec<Paper> {
        Vec::new()
    }
}

impress_service_impl! {
    service = PaperService,
    safety = read_only,
    since = "0.1.0",
    effects = { reads: [], writes: [], reach: [] },
    impl = Library,
    instance = || Library,
    methods = [
        /// Papers by id.
        get_papers(
            /// The ids.
            ids: Vec<String>
        ) -> Papers,
        /// Papers matching a query.
        search_papers(
            /// The text.
            query: String
        ) -> Papers,
        /// Add a paper.
        add_paper(
            /// Its title.
            title: String,
            /// Its year.
            year: Option<i64>
        ) -> Papers,
        /// Delete papers.
        delete_papers(
            /// The papers.
            ids: Vec<String>
        ) -> Papers,
        /// Import a structure.
        import(
            /// The structure.
            nested: Nested
        ) -> Papers,
        /// One note.
        get_note(
            /// Its id.
            id: String
        ) -> Option<Paper>,
        /// Notes.
        search_notes(
            /// The text.
            query: String,
            /// At most this many.
            limit: u32
        ) -> Vec<Paper>,
    ],
}

fn ours() -> Vec<VerbHandle> {
    call::descriptors()
        .filter(|v| v.service() == "paper-service")
        .collect()
}

#[test]
fn intents_entities_and_shortcuts_are_generated_and_deterministic() {
    let app = AppInfo::impress("Lib");
    let generated = generate(&ours(), &app);
    assert_eq!(generated, generate(&ours(), &app), "deterministic");
    for (name, text) in &generated.files {
        assert!(text.starts_with(GENERATED_MARK), "{name}");
    }

    let entities = &generated.files["LibEntities.swift"];
    assert!(
        entities.contains("public struct PaperEntity: AppEntity"),
        "{entities}"
    );
    assert!(entities.contains(r#"row["bibcode"]"#));
    assert!(entities.contains(r#"call("paper-service_get-papers", ["ids": identifiers])"#));
    assert!(entities.contains(r#"call("paper-service_search-papers", ["query": string])"#));

    // A lookup by one id is called per id; a search's other required
    // arguments are filled.
    assert!(
        entities.contains("public struct NoteEntity: AppEntity"),
        "{entities}"
    );
    assert!(entities.contains("for id in identifiers"), "{entities}");
    assert!(
        entities.contains(r#"["query": string, "limit": 50]"#),
        "{entities}"
    );

    let intents = &generated.files["LibIntents.swift"];
    assert!(
        intents.contains("public struct PaperAddPaperIntent: AppIntent"),
        "{intents}"
    );
    assert!(intents.contains("var title: String\n"));
    assert!(intents.contains("var year: Int?"));
    assert!(intents.contains(r#"if let value = year { args["year"] = value }"#));
    // The destructive verb takes paper entities and asks first.
    assert!(intents.contains("var ids: [PaperEntity]"));
    assert!(intents.contains(r#"args["ids"] = ids.map(\.id)"#));
    assert!(intents.contains("try await requestConfirmation"));
    assert!(intents.contains("waits for your approval"));
    // Lookups and searches are not intents of their own.
    assert!(!intents.contains("GetPapersIntent"));

    let shortcuts = &generated.files["LibShortcuts.swift"];
    assert!(
        shortcuts.contains(r#"phrases: ["Add a paper to \(.applicationName)"]"#),
        "{shortcuts}"
    );

    let support = &generated.files["LibIntentsSupport.swift"];
    assert!(support.contains("public struct LibIntentsPackage: AppIntentsPackage {}"));
    assert!(support.contains("await dispatchVerbAsync("));
    assert!(support.contains(r#"\"kind\":\"person\""#));

    assert_eq!(generated.skipped.len(), 1, "{:?}", generated.skipped);
    assert_eq!(generated.skipped[0].0, "paper-service_import");
}

/// When `xcrun swiftc` is on the machine, the generated Swift type-checks
/// against a stub of the FFI. `cargo test -- --ignored` runs it.
#[test]
#[ignore]
fn the_generated_swift_type_checks() {
    let dir = std::env::temp_dir().join(format!("koine-app-intents-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let generated = generate(&ours(), &AppInfo::impress("Lib"));
    let mut files = Vec::new();
    for (name, text) in &generated.files {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        files.push(path);
    }
    let stub = dir.join("Stub.swift");
    std::fs::write(
        &stub,
        "public struct DispatchResult { public let status: UInt16; public let bodyJson: String }\n\
         public func dispatchVerbAsync(name: String, argsJson: String, callerJson: String) async -> DispatchResult {\n\
             DispatchResult(status: 200, bodyJson: \"{}\")\n\
         }\n",
    )
    .unwrap();
    files.push(stub);
    let output = std::process::Command::new("xcrun")
        .args(["swiftc", "-typecheck", "-target", "arm64-apple-macos26.0"])
        .args(&files)
        .output()
        .expect("xcrun swiftc");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
