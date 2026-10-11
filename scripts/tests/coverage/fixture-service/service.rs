pub trait CounterService {
    /// Adds one.
    #[impress_method]
    async fn bump(&self) -> u32;

    /// Reads the total.
    ///
    /// # covers: Counter::read_total
    #[impress_method]
    async fn total(&self) -> u32;
}
