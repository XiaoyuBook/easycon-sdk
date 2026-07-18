#![forbid(unsafe_code)]
//! Operations, cancellation, events, clocks, and deterministic runtime shutdown.

mod cancellation;
mod clock;
mod event;
mod operation;
mod runtime;
mod wait;

pub use cancellation::{CancellationHookRegistration, CancellationToken};
pub use clock::{
    Clock, ClockChangeRegistration, DeadlineId, DeadlineTrace, SystemClock, VirtualClock,
};
pub use event::{
    Event, EventClass, EventDraft, EventGap, EventKind, EventSubscription, Severity,
    SubscriptionOptions, SubscriptionRead,
};
pub use operation::{
    CancellationReason, Operation, OperationSnapshot, OperationState, OperationValue,
    TransitionOutcome, WaitResult,
};
pub use runtime::{
    CloseRejection, ManagedResource, ResourceRegistration, Runtime, RuntimeCounts, RuntimeState,
    SupervisedTask, SupervisedTaskOutcome, TaskJoinError,
};
pub use wait::WaitTimeout;

/// Behavior schema version implemented by this crate.
pub const BEHAVIOR_SCHEMA_VERSION: u32 = easycon_model::BEHAVIOR_SCHEMA_VERSION;
