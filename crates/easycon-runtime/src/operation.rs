use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use easycon_model::{EasyConError, ErrorCode, ErrorDomain, OperationId};

use crate::cancellation::CancellationToken;
use crate::event::{EventDraft, EventKind, Severity};
use crate::runtime::RuntimeInner;
use crate::wait::WaitTimeout;

/// Canonical operation state shared by every language binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
    /// Created but not yet admitted to work.
    Pending,
    /// Admitted and executing or awaiting a supervised resource.
    Running,
    /// Cancellation was requested and cleanup is incomplete.
    Cancelling,
    /// Result committed exactly once.
    Succeeded,
    /// Error committed exactly once.
    Failed,
    /// Cancellation cleanup completed.
    Cancelled,
}

impl OperationState {
    /// Returns whether no further transition is legal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// First cancellation cause retained by an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationReason {
    /// Explicit caller request.
    Requested,
    /// Operation execution deadline.
    Deadline,
    /// Parent Runtime shutdown.
    ParentClose,
}

/// Immutable operation result payload used before a formal public ABI exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationValue {
    /// Successful command with no additional value.
    Unit,
    /// Stable numeric result.
    U64(u64),
    /// Owned byte result.
    Bytes(Arc<[u8]>),
}

/// Authoritative immutable view of an operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationSnapshot {
    /// Canonical state.
    pub state: OperationState,
    /// Result present only for `Succeeded`.
    pub result: Option<OperationValue>,
    /// Error present for `Failed` and reason-bearing `Cancelled`.
    pub error: Option<EasyConError>,
    /// First cancellation cause, if cancellation was requested.
    pub cancellation_reason: Option<CancellationReason>,
}

/// Result of an attempted operation transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionOutcome {
    /// The transition was committed.
    Applied,
    /// The requested idempotent transition had already been requested.
    Unchanged,
    /// The operation already has an immutable terminal state.
    AlreadyTerminal,
    /// The requested edge is not legal from the current non-terminal state.
    Invalid,
}

/// Result of waiting for an operation terminal state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WaitResult {
    /// The operation reached a terminal state.
    Completed(OperationSnapshot),
    /// Only this wait elapsed; the operation continues unchanged.
    Timeout,
}

/// Cloneable observation handle for a Runtime-supervised operation.
#[derive(Clone)]
pub struct Operation {
    pub(crate) inner: Arc<OperationInner>,
}

pub(crate) struct OperationInner {
    id: OperationId,
    runtime: std::sync::Weak<RuntimeInner>,
    cancellation: CancellationToken,
    deadline_ns: Option<u64>,
    state: Mutex<OperationData>,
    changed: Condvar,
}

struct OperationData {
    state: OperationState,
    result: Option<OperationValue>,
    error: Option<EasyConError>,
    cancellation_reason: Option<CancellationReason>,
}

impl Operation {
    pub(crate) fn new(
        id: OperationId,
        runtime: std::sync::Weak<RuntimeInner>,
        cancellation: CancellationToken,
        deadline_ns: Option<u64>,
    ) -> Self {
        let operation = Self {
            inner: Arc::new(OperationInner {
                id,
                runtime,
                cancellation,
                deadline_ns,
                state: Mutex::new(OperationData {
                    state: OperationState::Pending,
                    result: None,
                    error: None,
                    cancellation_reason: None,
                }),
                changed: Condvar::new(),
            }),
        };
        let weak = Arc::downgrade(&operation.inner);
        operation.inner.cancellation.on_cancel(move || {
            if let Some(inner) = weak.upgrade() {
                let _ = inner.request_cancel(CancellationReason::ParentClose);
            }
        });
        operation
    }

    /// Returns the Runtime-local operation identifier.
    #[must_use]
    pub fn id(&self) -> OperationId {
        self.inner.id
    }

    /// Returns the optional absolute execution deadline.
    #[must_use]
    pub fn deadline_ns(&self) -> Option<u64> {
        self.inner.deadline_ns
    }

    /// Returns a child cancellation token owned by this operation.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.inner.cancellation.clone()
    }

    /// Registers a non-blocking wake hook for supervised work.
    pub fn on_cancel(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.inner.cancellation.on_cancel(hook);
    }

    /// Admits a pending operation.
    pub fn start(&self) -> TransitionOutcome {
        self.inner.transition_running()
    }

    /// Idempotently requests caller cancellation.
    pub fn cancel(&self) -> TransitionOutcome {
        self.request_cancel(CancellationReason::Requested)
    }

    /// Requests cancellation with an explicit core reason.
    pub fn request_cancel(&self, reason: CancellationReason) -> TransitionOutcome {
        self.inner.request_cancel(reason)
    }

    /// Commits success once.
    pub fn succeed(&self, result: OperationValue) -> TransitionOutcome {
        self.inner.finish_success(result)
    }

    /// Runs non-blocking owner cleanup under the transition lock, then commits success once.
    ///
    /// The cleanup must not call back into this operation.
    pub fn succeed_after_cleanup(
        &self,
        result: OperationValue,
        cleanup: impl FnOnce(),
    ) -> TransitionOutcome {
        self.inner.finish_terminal_after_cleanup(
            OperationState::Succeeded,
            Some(result),
            None,
            cleanup,
        )
    }

    /// Commits failure once.
    pub fn fail(&self, error: EasyConError) -> TransitionOutcome {
        self.inner.finish_failure(error)
    }

    /// Runs non-blocking owner cleanup under the transition lock, then commits failure once.
    ///
    /// The cleanup must not call back into this operation.
    pub fn fail_after_cleanup(
        &self,
        error: EasyConError,
        cleanup: impl FnOnce(),
    ) -> TransitionOutcome {
        self.inner
            .finish_terminal_after_cleanup(OperationState::Failed, None, Some(error), cleanup)
    }

    /// Commits `Cancelled` after owner-specific cleanup completes.
    pub fn finish_cancelled(&self) -> TransitionOutcome {
        self.inner.finish_cancelled()
    }

    /// Returns authoritative state without consuming the operation.
    #[must_use]
    pub fn snapshot(&self) -> OperationSnapshot {
        self.inner.snapshot()
    }

    /// Waits only for terminal observation; timeout never cancels this operation.
    #[must_use]
    pub fn wait(&self, wait: WaitTimeout) -> WaitResult {
        self.inner.wait(wait)
    }
}

impl OperationInner {
    fn transition_running(&self) -> TransitionOutcome {
        let mut data = self.state.lock().expect("operation state lock poisoned");
        match data.state {
            OperationState::Pending => {
                data.state = OperationState::Running;
                self.publish(&data);
                TransitionOutcome::Applied
            }
            state if state.is_terminal() => TransitionOutcome::AlreadyTerminal,
            _ => TransitionOutcome::Invalid,
        }
    }

    fn request_cancel(&self, reason: CancellationReason) -> TransitionOutcome {
        let outcome = {
            let mut data = self.state.lock().expect("operation state lock poisoned");
            match data.state {
                OperationState::Pending | OperationState::Running => {
                    data.state = OperationState::Cancelling;
                    data.cancellation_reason = Some(reason);
                    self.publish(&data);
                    TransitionOutcome::Applied
                }
                OperationState::Cancelling => TransitionOutcome::Unchanged,
                _ => TransitionOutcome::AlreadyTerminal,
            }
        };
        if outcome == TransitionOutcome::Applied {
            self.cancellation.cancel();
            self.changed.notify_all();
        }
        outcome
    }

    fn finish_success(&self, result: OperationValue) -> TransitionOutcome {
        self.finish_terminal(OperationState::Succeeded, Some(result), None)
    }

    fn finish_failure(&self, error: EasyConError) -> TransitionOutcome {
        self.finish_terminal(OperationState::Failed, None, Some(error))
    }

    fn finish_cancelled(&self) -> TransitionOutcome {
        let mut data = self.state.lock().expect("operation state lock poisoned");
        match data.state {
            OperationState::Cancelling => {
                let reason = data
                    .cancellation_reason
                    .unwrap_or(CancellationReason::Requested);
                let (code, message) = match reason {
                    CancellationReason::Requested => (ErrorCode::Cancelled, "operation cancelled"),
                    CancellationReason::Deadline => {
                        (ErrorCode::DeadlineExceeded, "operation deadline elapsed")
                    }
                    CancellationReason::ParentClose => (
                        ErrorCode::Cancelled,
                        "parent operation, resource, or Runtime ended",
                    ),
                };
                data.error = Some(EasyConError::new(ErrorDomain::Runtime, code, message));
                data.state = OperationState::Cancelled;
                self.publish(&data);
                drop(data);
                self.terminal_committed();
                TransitionOutcome::Applied
            }
            state if state.is_terminal() => TransitionOutcome::AlreadyTerminal,
            _ => TransitionOutcome::Invalid,
        }
    }

    fn finish_terminal(
        &self,
        terminal: OperationState,
        result: Option<OperationValue>,
        error: Option<EasyConError>,
    ) -> TransitionOutcome {
        self.finish_terminal_after_cleanup(terminal, result, error, || {})
    }

    fn finish_terminal_after_cleanup(
        &self,
        terminal: OperationState,
        result: Option<OperationValue>,
        error: Option<EasyConError>,
        cleanup: impl FnOnce(),
    ) -> TransitionOutcome {
        let mut data = self.state.lock().expect("operation state lock poisoned");
        match data.state {
            OperationState::Pending if terminal == OperationState::Failed => {}
            OperationState::Running => {}
            state if state.is_terminal() => return TransitionOutcome::AlreadyTerminal,
            _ => return TransitionOutcome::Invalid,
        }
        cleanup();
        data.state = terminal;
        data.result = result;
        data.error = error;
        self.publish(&data);
        drop(data);
        self.terminal_committed();
        TransitionOutcome::Applied
    }

    fn terminal_committed(&self) {
        self.cancellation.deactivate();
        self.changed.notify_all();
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.unregister_operation(self.id);
        }
    }

    fn snapshot(&self) -> OperationSnapshot {
        snapshot_data(&self.state.lock().expect("operation state lock poisoned"))
    }

    fn wait(&self, wait: WaitTimeout) -> WaitResult {
        let started = Instant::now();
        let mut data = self.state.lock().expect("operation state lock poisoned");
        loop {
            if data.state.is_terminal() {
                return WaitResult::Completed(snapshot_data(&data));
            }
            match wait {
                WaitTimeout::Poll => return WaitResult::Timeout,
                WaitTimeout::Infinite => {
                    data = self
                        .changed
                        .wait(data)
                        .expect("operation state lock poisoned while waiting");
                }
                WaitTimeout::For(limit) => {
                    let Some(remaining) = limit.checked_sub(started.elapsed()) else {
                        return WaitResult::Timeout;
                    };
                    let (next, timed_out) = self
                        .changed
                        .wait_timeout(data, remaining)
                        .expect("operation state lock poisoned while waiting");
                    data = next;
                    if timed_out.timed_out() && !data.state.is_terminal() {
                        return WaitResult::Timeout;
                    }
                }
            }
        }
    }

    fn publish(&self, data: &OperationData) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let (kind, code, severity) = if data.state.is_terminal() {
            let severity = if data.state == OperationState::Failed {
                Severity::Error
            } else {
                Severity::Info
            };
            (EventKind::Terminal, terminal_code(data.state), severity)
        } else {
            (EventKind::State, state_code(data.state), Severity::Info)
        };
        let draft = EventDraft::critical(kind, code, severity);
        let _ = runtime.try_publish_event(draft.with_operation(self.id));
    }
}

fn snapshot_data(data: &OperationData) -> OperationSnapshot {
    debug_assert!(!(data.result.is_some() && data.error.is_some()));
    OperationSnapshot {
        state: data.state,
        result: data.result.clone(),
        error: data.error.clone(),
        cancellation_reason: data.cancellation_reason,
    }
}

const fn state_code(state: OperationState) -> &'static str {
    match state {
        OperationState::Pending => "runtime.operation.pending",
        OperationState::Running => "runtime.operation.running",
        OperationState::Cancelling => "runtime.operation.cancelling",
        OperationState::Succeeded => "runtime.operation.succeeded",
        OperationState::Failed => "runtime.operation.failed",
        OperationState::Cancelled => "runtime.operation.cancelled",
    }
}

const fn terminal_code(state: OperationState) -> &'static str {
    state_code(state)
}
