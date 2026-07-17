use std::time::Duration;

/// Caller-side wait limit. It never changes the observed operation or resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitTimeout {
    /// Return immediately if no value is ready.
    Poll,
    /// Wait for at most the supplied monotonic duration.
    For(Duration),
    /// Wait until the value is ready or its producer closes.
    Infinite,
}
