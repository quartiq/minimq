/// Minimal timer contract required by the async runner.
#[allow(async_fn_in_trait)]
pub trait Timer {
    type Error;

    /// Current monotonic time in milliseconds.
    fn now(&mut self) -> Result<u64, Self::Error>;

    /// Sleep until the provided monotonic deadline.
    async fn sleep_until(&mut self, deadline_ms: u64) -> Result<(), Self::Error>;
}
