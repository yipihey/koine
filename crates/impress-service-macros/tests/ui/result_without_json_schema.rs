use impress_service_core::async_trait;
// `#[impress_service]` consumes `#[impress_method]`, so the import reads as unused.
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};

/// A type from a library: it has no `JsonSchema`, and a verb's result needs one.
pub struct Matrix {
    pub rows: Vec<Vec<f64>>,
}

#[impress_service]
pub trait MatrixService: Send + Sync + 'static {
    /// The identity matrix of a given size.
    #[impress_method]
    async fn identity(&self, size: u32) -> Matrix;
}

#[derive(Clone, Default)]
pub struct DefaultMatrixService;

#[async_trait::async_trait]
impl MatrixService for DefaultMatrixService {
    async fn identity(&self, size: u32) -> Matrix {
        Matrix { rows: vec![vec![0.0; size as usize]; size as usize] }
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
        identity(size: u32) -> Matrix,
    ],
}

fn main() {}
