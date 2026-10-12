//! A verb that needs a secret, and one that says which field of its rows is the id.
use impress_service_core::async_trait;
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Paper {
    pub bibcode: String,
    pub title: String,
    pub year: Option<i32>,
}

#[impress_service]
pub trait PapersService: Send + Sync + 'static {
    /// Search papers; needs the SciX bearer.
    #[impress_method(
        secrets = ["scix-api-token", "other_secret"],
        returns_kind = "paper",
        rows(id = "bibcode", title = "title", columns = ["bibcode", "title", "year"])
    )]
    async fn search(&self, query: String) -> Vec<Paper>;

    /// List papers; the title and columns take their defaults.
    #[impress_method(returns_kind = "paper", rows(id = "bibcode"))]
    async fn recent(&self) -> Vec<Paper>;
}

#[derive(Clone, Default)]
pub struct DefaultPapersService;

#[async_trait::async_trait]
impl PapersService for DefaultPapersService {
    async fn search(&self, _query: String) -> Vec<Paper> {
        Vec::new()
    }

    async fn recent(&self) -> Vec<Paper> {
        Vec::new()
    }
}

impress_service_impl! {
    service = PapersService,
    impl = DefaultPapersService,
    instance = || DefaultPapersService,
    safety = read_only,
    since = "0.1.0",
    effects = { reads: [], writes: [], reach: [] },
    methods = [
        search(query: String) -> Vec<Paper>,
        recent() -> Vec<Paper>,
    ],
}

fn main() {
    let search = impress_service_core::VerbDescriptor::iter()
        .find(|verb| verb.method == "search")
        .expect("search is linked");
    assert_eq!(search.secrets, &["scix-api-token", "other_secret"]);
    assert_eq!(search.rows.map(|rows| rows.id), Some("bibcode"));
    assert_eq!(search.row_schema_check(), Ok(()));
}
