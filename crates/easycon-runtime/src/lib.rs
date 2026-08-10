#![forbid(unsafe_code)]
//! Operations, cancellation, events, clocks, and deterministic runtime shutdown.

mod cancellation;
mod clock;
mod concurrency;
mod deadline;
mod event;
mod operation;
mod runtime;
mod wait;

pub use cancellation::{CancellationHookRegistration, CancellationToken};
pub use clock::{
    Clock, ClockChangeRegistration, DeadlineId, DeadlineTrace, SystemClock, VirtualClock,
};
pub use deadline::{
    DeadlineOutcome, DeadlineRegistration, DeadlineRegistrationId, DeadlineResolution,
    DeadlineSignal, DeadlineWaitResult,
};
pub use event::{
    Event, EventClass, EventDraft, EventGap, EventKind, EventSubscription, Severity,
    SubscriptionOptions, SubscriptionRead,
};
pub use operation::{
    CancellationReason, ClaimOutcome, Operation, OperationSettlementClaim,
    OperationSettlementOwner, OperationSnapshot, OperationState, OperationValue,
    SettlementEvidence, SettlementOwnerMode, TerminalCandidate, TransitionOutcome, WaitResult,
};
pub use runtime::{
    CloseOutcome, ClosePhase, CloseRejection, CloseReport, ManagedResource, ResourceRegistration,
    Runtime, RuntimeCounts, RuntimeState, SupervisedTask, SupervisedTaskOutcome, TaskJoinError,
};
pub use wait::WaitTimeout;

#[cfg(feature = "runtime-model")]
#[doc(hidden)]
pub mod runtime_model {
    pub use crate::concurrency::{
        CancellationNode, TaskLifecycleState, TaskOwnerBinding, TerminalArbiterState,
        TerminalClaimResult, TerminalEvidenceKind, TerminalWinnerKind, admit_child_while_locked,
        cancellation_admission_open, claim_cancellation, invoke_isolated, runtime_close_rejected,
        seal_cancelled_tree, seal_deactivated_tree, task_join_rejected, unlink_then_notify,
    };
    pub use crate::deadline::DeadlineResolutionState;
}

/// Behavior schema version implemented by this crate.
pub const BEHAVIOR_SCHEMA_VERSION: u32 = easycon_model::BEHAVIOR_SCHEMA_VERSION;
