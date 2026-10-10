use impress_service_macros::impress_service;

#[impress_service]
pub trait ExampleService {
    /// Echo a value.
    #[impress_method]
    #[impress_example(name = "one", args = "{}", tier = "network")]
    async fn echo(&self) -> String;
}
