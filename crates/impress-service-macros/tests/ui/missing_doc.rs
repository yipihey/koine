use impress_service_core::async_trait;
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct EchoReply {
    pub reply: String,
}

#[impress_service]
pub trait EchoService: Send + Sync + 'static {
    #[impress_method]
    async fn echo(&self, message: String) -> EchoReply;
}

#[derive(Clone, Default)]
pub struct DefaultEchoService;

#[async_trait::async_trait]
impl EchoService for DefaultEchoService {
    async fn echo(&self, message: String) -> EchoReply {
        EchoReply { reply: message }
    }
}

impress_service_impl! {
    service = EchoService,
    impl = DefaultEchoService,
    instance = || DefaultEchoService,
    safety = read_only,
    since = "0.1.0",
    effects = {},
    methods = [
        echo(message: String) -> EchoReply,
    ],
}

fn main() {}
