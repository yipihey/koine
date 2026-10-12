//! The smallest correct service: one verb, a service-wide default for safety
//! and effects, an example the Tier A selftest can run.
use impress_service_core::async_trait;
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EchoReply {
    pub reply: String,
}

#[impress_service]
pub trait EchoService: Send + Sync + 'static {
    /// Echo a message back, prefixed with "echo: ".
    #[impress_method]
    #[impress_example(name = "hello", args = r#"{"message": "hi"}"#)]
    async fn echo(&self, message: String) -> EchoReply;

    /// Say whether a count is even; may be asked tentatively and within a deadline.
    #[impress_method(supports(tentatively, within))]
    async fn is_even(&self, count: u64) -> bool;
}

#[derive(Clone, Default)]
pub struct DefaultEchoService;

#[async_trait::async_trait]
impl EchoService for DefaultEchoService {
    async fn echo(&self, message: String) -> EchoReply {
        EchoReply {
            reply: format!("echo: {message}"),
        }
    }

    async fn is_even(&self, count: u64) -> bool {
        count % 2 == 0
    }
}

impress_service_impl! {
    service = EchoService,
    impl = DefaultEchoService,
    instance = || DefaultEchoService,
    safety = read_only,
    since = "0.1.0",
    effects = { reads: [], writes: [], reach: [] },
    methods = [
        echo(message: String) -> EchoReply,
        is_even(count: u64) -> bool,
    ],
}

fn main() {}
