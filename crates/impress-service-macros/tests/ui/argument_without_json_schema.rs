use impress_service_core::async_trait;
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A type from a library: it has no `JsonSchema`, and a verb's arguments need one.
pub struct Matrix {
    pub rows: Vec<Vec<f64>>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Trace {
    pub value: f64,
}

#[impress_service]
pub trait MatrixService: Send + Sync + 'static {
    /// The trace of a matrix.
    #[impress_method]
    async fn trace(&self, matrix: Matrix) -> Trace;
}

#[derive(Clone, Default)]
pub struct DefaultMatrixService;

#[async_trait::async_trait]
impl MatrixService for DefaultMatrixService {
    async fn trace(&self, matrix: Matrix) -> Trace {
        Trace { value: matrix.rows.len() as f64 }
    }
}

impress_service_impl! {
    service = MatrixService,
    impl = DefaultMatrixService,
    instance = || DefaultMatrixService,
    safety = read_only,
    since = "0.1.0",
    effects = {},
    methods = [
        trace(matrix: Matrix) -> Trace,
    ],
}

fn main() {}
