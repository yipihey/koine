use impress_service_macros::impress_service;

#[impress_service]
pub trait UndocumentedService {
    #[impress_method]
    async fn run(&self);
}
