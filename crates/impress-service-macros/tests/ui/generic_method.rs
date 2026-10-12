use impress_service_core::async_trait;
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Size {
    pub bytes: u64,
}

#[impress_service]
pub trait SizeService: Send + Sync + 'static {
    /// The size in bytes of anything that can say so.
    #[impress_method]
    async fn size_of<T: Serialize + Send + 'static>(&self, value: T) -> Size;
}

#[derive(Clone, Default)]
pub struct DefaultSizeService;

#[async_trait::async_trait]
impl SizeService for DefaultSizeService {
    async fn size_of<T: Serialize + Send + 'static>(&self, _value: T) -> Size {
        Size { bytes: 0 }
    }
}

impress_service_impl! {
    service = SizeService,
    impl = DefaultSizeService,
    instance = || DefaultSizeService,
    safety = read_only,
    since = "0.1.0",
    effects = {},
    methods = [
        size_of(value: serde_json::Value) -> Size,
    ],
}

fn main() {}
