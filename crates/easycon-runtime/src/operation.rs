use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

use easycon_model::{EasyConError, ErrorCode, ErrorDomain, OperationId, TaskId};

use crate::cancellation::CancellationToken;
use crate::concurrency::{
    TerminalArbiterState, TerminalClaimResult, TerminalEvidenceKind, TerminalWinnerKind,
    catch_isolated, drop_isolated, evidence_allows_winner, unlink_then_notify,
};
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
    /// Owner cleanup panicked and the transaction committed an internal failure instead.
    CleanupFailed,
}

/// Mutually exclusive evidence consumed by a legitimate operation settlement owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementEvidence {
    /// The domain effect was fully accepted.
    EffectAccepted,
    /// The domain proved that the effect was not delivered.
    NotDelivered,
    /// Execution failed without an accepted effect.
    ExecutionFailed,
}

impl SettlementEvidence {
    const fn kind(self) -> TerminalEvidenceKind {
        match self {
            Self::EffectAccepted => TerminalEvidenceKind::EffectAccepted,
            Self::NotDelivered => TerminalEvidenceKind::NotDelivered,
            Self::ExecutionFailed => TerminalEvidenceKind::ExecutionFailed,
        }
    }
}

/// Whether Runtime supervision pre-holds a transferable settlement owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementOwnerMode {
    /// Only the original registered owner may claim settlement.
    Exclusive,
    /// Runtime supervision may hand off after the original owner is joined and evidence is shared.
    Transferable,
}

/// Generic terminal candidate paired with settlement evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalCandidate {
    /// An accepted effect projects to success after cleanup.
    Success(OperationValue),
    /// Execution failure remains the primary error after cleanup.
    Failure(EasyConError),
    /// Not-delivered evidence projects the immutable first cancellation reason.
    Cancellation,
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

/// Unique settlement capability for an explicitly owned operation.
#[must_use = "dropping an unsettled owner may force conservative Runtime close failure"]
pub struct OperationSettlementOwner {
    pub(crate) inner: Arc<OperationInner>,
    owner_id: u64,
}

/// Result of attempting to fix the winner of an owned terminal transaction.
///
/// Claiming only seals the winner and child admission. Call
/// [`OperationSettlementClaim::finish`] after domain bookkeeping and cleanup are ready to
/// commit the terminal result.
pub enum ClaimOutcome {
    /// This caller fixed the winner and owns the deferred terminal transaction.
    Claimed(OperationSettlementClaim),
    /// Another owner already fixed a winner. This path never waits for terminal completion.
    Observe,
    /// The caller or its evidence cannot claim this operation.
    Rejected,
    /// The operation had already committed a terminal state.
    Closed,
}

/// One non-cloneable deferred terminal transaction returned by a successful claim.
///
/// Dropping a claim never synthesizes success. It preserves the frozen winner and marks the
/// transaction abandoned so Runtime close can fail closed rather than wait forever.
#[must_use = "a claimed terminal transaction must be finished or will fail closed"]
pub struct OperationSettlementClaim {
    inner: Arc<OperationInner>,
    owner_id: u64,
    active: bool,
}

pub(crate) struct OperationInner {
    id: OperationId,
    runtime: std::sync::Weak<RuntimeInner>,
    cancellation: CancellationToken,
    deadline_ns: Option<u64>,
    state: Mutex<OperationData>,
    changed: Condvar,
    #[cfg(test)]
    wait_observers: Mutex<Vec<std::sync::mpsc::Sender<()>>>,
}

struct OperationData {
    state: OperationState,
    result: Option<OperationValue>,
    error: Option<EasyConError>,
    cancellation_reason: Option<CancellationReason>,
    terminal_in_progress: bool,
    owner: Option<OperationOwnerData>,
}

pub(crate) type OwnerCleanup = Box<dyn FnOnce() -> Result<(), EasyConError> + Send + 'static>;
pub(crate) type OwnerCleanupSettled = Box<dyn FnOnce(Result<(), EasyConError>) + Send + 'static>;

pub(crate) struct OperationOwnerSetup {
    owner_id: u64,
    transfer_owner_id: Option<u64>,
    cleanup: OwnerCleanup,
    on_cleanup_settled: OwnerCleanupSettled,
}

impl OperationOwnerSetup {
    pub(crate) fn new(
        owner_id: u64,
        transfer_owner_id: Option<u64>,
        cleanup: OwnerCleanup,
        on_cleanup_settled: OwnerCleanupSettled,
    ) -> Self {
        Self {
            owner_id,
            transfer_owner_id,
            cleanup,
            on_cleanup_settled,
        }
    }
}

struct OperationOwnerData {
    arbiter: TerminalArbiterState,
    owner_task: Option<TaskId>,
    cleanup: Option<OwnerCleanup>,
    on_cleanup_settled: Option<OwnerCleanupSettled>,
    handoff_record: Option<(SettlementEvidence, TerminalCandidate)>,
    claimed: Option<ClaimedTerminalTransaction>,
}

struct ClaimedTerminalTransaction {
    owner_id: u64,
    evidence: SettlementEvidence,
    candidate: TerminalCandidate,
    primary: TerminalPrimary,
    winner: TerminalWinnerKind,
    propagation: Option<crate::cancellation::CancellationPropagation>,
    state: ClaimedTerminalState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaimedTerminalState {
    Held,
    Finishing,
    Abandoned,
}

#[derive(Clone)]
enum TerminalPrimary {
    Success(OperationValue),
    Failure(EasyConError),
    Cancellation(CancellationReason),
}

enum LegacyTerminalRequest {
    Execution(Result<OperationValue, EasyConError>),
    Cancellation,
}

impl TerminalPrimary {
    const fn winner_kind(&self) -> TerminalWinnerKind {
        match self {
            Self::Success(_) => TerminalWinnerKind::Success,
            Self::Failure(_) => TerminalWinnerKind::Failure,
            Self::Cancellation(_) => TerminalWinnerKind::Cancellation,
        }
    }
}

pub(crate) enum OperationFallbackOutcome {
    Legacy,
    AlreadyTerminal,
    Settled,
    OwnershipLost,
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
                    terminal_in_progress: false,
                    owner: None,
                }),
                changed: Condvar::new(),
                #[cfg(test)]
                wait_observers: Mutex::new(Vec::new()),
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

    pub(crate) fn new_owned(
        id: OperationId,
        runtime: std::sync::Weak<RuntimeInner>,
        cancellation: CancellationToken,
        deadline_ns: Option<u64>,
        setup: OperationOwnerSetup,
    ) -> (Self, OperationSettlementOwner) {
        let OperationOwnerSetup {
            owner_id,
            transfer_owner_id,
            cleanup,
            on_cleanup_settled,
        } = setup;
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
                    terminal_in_progress: false,
                    owner: Some(OperationOwnerData {
                        arbiter: TerminalArbiterState::new(owner_id, transfer_owner_id),
                        owner_task: None,
                        cleanup: Some(cleanup),
                        on_cleanup_settled: Some(on_cleanup_settled),
                        handoff_record: None,
                        claimed: None,
                    }),
                }),
                changed: Condvar::new(),
                #[cfg(test)]
                wait_observers: Mutex::new(Vec::new()),
            }),
        };
        let weak = Arc::downgrade(&operation.inner);
        operation.inner.cancellation.on_cancel(move || {
            if let Some(inner) = weak.upgrade() {
                let _ = inner.request_cancel(CancellationReason::ParentClose);
            }
        });
        let owner = OperationSettlementOwner {
            inner: Arc::clone(&operation.inner),
            owner_id,
        };
        (operation, owner)
    }

    /// Returns the Runtime-local operation identifier.
    #[must_use]
    pub fn id(&self) -> OperationId {
        self.inner.id
    }

    #[cfg(test)]
    pub(crate) fn observe_next_wait_blocked(&self, observer: std::sync::mpsc::Sender<()>) {
        self.inner
            .wait_observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(observer);
    }

    #[cfg(test)]
    pub(crate) fn poison_state_for_test(&self) {
        let _state = self.inner.state.lock().expect("operation state lock");
        panic!("failpoint:operation.state.poison");
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

    /// Runs owner cleanup outside the operation state lock, then commits success once.
    ///
    /// A cleanup panic is isolated and commits an internal failure with
    /// [`TransitionOutcome::CleanupFailed`].
    pub fn succeed_after_cleanup(
        &self,
        result: OperationValue,
        cleanup: impl FnOnce(),
    ) -> TransitionOutcome {
        self.inner.finish_legacy_after_cleanup(
            LegacyTerminalRequest::Execution(Ok(result)),
            || {
                cleanup();
                Ok(())
            },
            |_| {},
        )
    }

    /// Commits failure once.
    pub fn fail(&self, error: EasyConError) -> TransitionOutcome {
        self.inner.finish_failure(error)
    }

    /// Commits a proven cleanup failure even when cancellation was already requested.
    ///
    /// This narrow path is for resource cleanup failures whose contract requires the retained
    /// first failure to prevail over ordinary cancellation precedence. Owned operations must use
    /// their [`OperationSettlementOwner`] claim transaction instead.
    pub fn fail_strict_cleanup(&self, error: EasyConError) -> TransitionOutcome {
        self.inner.finish_strict_cleanup_failure(error)
    }

    /// Runs owner cleanup outside the operation state lock, then commits failure once.
    ///
    /// A cleanup panic is isolated and reports [`TransitionOutcome::CleanupFailed`] without
    /// replacing the primary failure.
    pub fn fail_after_cleanup(
        &self,
        error: EasyConError,
        cleanup: impl FnOnce(),
    ) -> TransitionOutcome {
        self.inner.finish_legacy_after_cleanup(
            LegacyTerminalRequest::Execution(Err(error)),
            || {
                cleanup();
                Ok(())
            },
            |_| {},
        )
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

impl OperationSettlementOwner {
    /// Fixes the unique winner from settlement evidence without committing a terminal result.
    ///
    /// The claim critical section performs no cleanup, event publication, wake, Clock access, or
    /// foreign callback. A cancellation winner publishes its public `Cancelling` state only after
    /// that critical section ends. The returned token owns the deferred finish transaction.
    pub fn claim(
        &self,
        evidence: SettlementEvidence,
        candidate: TerminalCandidate,
    ) -> ClaimOutcome {
        OperationInner::claim_owned(&self.inner, self.owner_id, evidence, candidate)
    }

    /// Claims the unique winner, runs shared cleanup, and commits once.
    ///
    /// This remains a compatibility wrapper for ordinary Runtime owner users. Controller report
    /// paths use [`Self::claim`] and finish only after their post-write bookkeeping is durable.
    pub fn settle(
        self,
        evidence: SettlementEvidence,
        candidate: TerminalCandidate,
    ) -> TransitionOutcome {
        match self.claim(evidence, candidate) {
            ClaimOutcome::Claimed(claim) => claim.finish(Ok(())),
            // `claim` is explicitly nonblocking, but the established `settle` API only returns
            // after the competing terminal transaction has committed.
            ClaimOutcome::Observe => self.inner.observe_terminal_completion(),
            ClaimOutcome::Rejected => TransitionOutcome::Invalid,
            ClaimOutcome::Closed => TransitionOutcome::AlreadyTerminal,
        }
    }

    /// Stores mutually exclusive evidence in the pre-registered transferable record.
    ///
    /// This does not claim a terminal winner. Runtime supervision may consume the record only after
    /// the original owner task has been joined.
    pub fn record_for_handoff(
        &self,
        evidence: SettlementEvidence,
        candidate: TerminalCandidate,
    ) -> TransitionOutcome {
        self.inner
            .record_for_handoff(self.owner_id, evidence, candidate)
    }

    pub(crate) fn belongs_to(&self, runtime: &Arc<RuntimeInner>) -> bool {
        self.inner.runtime.ptr_eq(&Arc::downgrade(runtime))
    }

    pub(crate) fn bind_task(&self, task_id: TaskId) -> bool {
        self.inner.bind_owner_task(self.owner_id, task_id)
    }
}

impl OperationSettlementClaim {
    /// Completes the frozen transaction after domain-specific cleanup has settled.
    ///
    /// A domain error is retained as the first cleanup failure while Runtime still executes its
    /// pre-registered cleanup and observer exactly once.
    pub fn finish(mut self, domain_cleanup: Result<(), EasyConError>) -> TransitionOutcome {
        self.active = false;
        self.inner.finish_claimed(self.owner_id, domain_cleanup)
    }
}

impl Drop for OperationSettlementClaim {
    fn drop(&mut self) {
        if self.active {
            self.inner.abandon_claimed(self.owner_id);
        }
    }
}

impl OperationInner {
    fn transition_running(&self) -> TransitionOutcome {
        let mut data = self.lock_state();
        if data.state.is_terminal() {
            return TransitionOutcome::AlreadyTerminal;
        }
        if data.terminal_in_progress {
            return TransitionOutcome::Invalid;
        }
        match data.state {
            OperationState::Pending => {
                data.state = OperationState::Running;
                self.publish(&data);
                TransitionOutcome::Applied
            }
            _ => TransitionOutcome::Invalid,
        }
    }

    fn request_cancel(&self, reason: CancellationReason) -> TransitionOutcome {
        let (outcome, propagation) = {
            let mut data = self.lock_state();
            if data.state.is_terminal() {
                return TransitionOutcome::AlreadyTerminal;
            }
            if data.owner.is_some() {
                if data.cancellation_reason.is_some() {
                    return TransitionOutcome::Unchanged;
                }
                data.cancellation_reason = Some(reason);
                let propagation = self.cancellation.cancel_deferred();
                (TransitionOutcome::Applied, Some(propagation))
            } else {
                if data.terminal_in_progress {
                    return if data.state == OperationState::Cancelling {
                        TransitionOutcome::Unchanged
                    } else {
                        TransitionOutcome::Invalid
                    };
                }
                match data.state {
                    OperationState::Pending | OperationState::Running => {
                        data.state = OperationState::Cancelling;
                        data.cancellation_reason = Some(reason);
                        let propagation = self.cancellation.cancel_deferred();
                        self.publish(&data);
                        (TransitionOutcome::Applied, Some(propagation))
                    }
                    OperationState::Cancelling => (TransitionOutcome::Unchanged, None),
                    _ => unreachable!("terminal states returned before transition dispatch"),
                }
            }
        };
        if let Some(propagation) = propagation {
            let _ = catch_isolated(|| propagation.propagate());
            self.changed.notify_all();
        }
        outcome
    }

    fn finish_success(&self, result: OperationValue) -> TransitionOutcome {
        self.finish_legacy_after_cleanup(
            LegacyTerminalRequest::Execution(Ok(result)),
            || Ok(()),
            |_| {},
        )
    }

    fn finish_failure(&self, error: EasyConError) -> TransitionOutcome {
        self.finish_legacy_after_cleanup(
            LegacyTerminalRequest::Execution(Err(error)),
            || Ok(()),
            |_| {},
        )
    }

    fn finish_strict_cleanup_failure(&self, error: EasyConError) -> TransitionOutcome {
        let propagation = {
            let mut data = self.lock_state();
            if data.state.is_terminal() {
                return TransitionOutcome::AlreadyTerminal;
            }
            if data.owner.is_some() || data.terminal_in_progress {
                return TransitionOutcome::Invalid;
            }
            match data.state {
                OperationState::Pending | OperationState::Running | OperationState::Cancelling => {}
                OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled => {
                    unreachable!("terminal states returned before strict cleanup failure")
                }
            }
            data.terminal_in_progress = true;
            self.cancellation.deactivate_deferred()
        };
        self.finish_claimed_terminal(
            TerminalPrimary::Failure(error),
            None,
            propagation,
            || Ok(()),
            |_| {},
            Ok(()),
        )
    }

    fn finish_cancelled(&self) -> TransitionOutcome {
        self.finish_legacy_after_cleanup(LegacyTerminalRequest::Cancellation, || Ok(()), |_| {})
    }

    fn finish_legacy_after_cleanup<C, S>(
        &self,
        request: LegacyTerminalRequest,
        cleanup: C,
        on_cleanup_settled: S,
    ) -> TransitionOutcome
    where
        C: FnOnce() -> Result<(), EasyConError>,
        S: FnOnce(Result<(), EasyConError>),
    {
        let mut data = self.lock_state();
        if data.state.is_terminal() {
            drop(data);
            discard_terminal_callbacks(cleanup, on_cleanup_settled);
            return TransitionOutcome::AlreadyTerminal;
        }
        if data.owner.is_some() {
            drop(data);
            discard_terminal_callbacks(cleanup, on_cleanup_settled);
            return TransitionOutcome::Invalid;
        }
        if data.terminal_in_progress {
            drop(data);
            discard_terminal_callbacks(cleanup, on_cleanup_settled);
            return self.observe_terminal_completion();
        }
        let primary = match (data.state, request) {
            (OperationState::Running, LegacyTerminalRequest::Execution(Ok(result))) => {
                TerminalPrimary::Success(result)
            }
            (
                OperationState::Pending | OperationState::Running,
                LegacyTerminalRequest::Execution(Err(error)),
            ) => TerminalPrimary::Failure(error),
            (OperationState::Cancelling, LegacyTerminalRequest::Cancellation) => {
                TerminalPrimary::Cancellation(
                    data.cancellation_reason
                        .unwrap_or(CancellationReason::Requested),
                )
            }
            _ => {
                drop(data);
                discard_terminal_callbacks(cleanup, on_cleanup_settled);
                return TransitionOutcome::Invalid;
            }
        };
        data.terminal_in_progress = true;
        let propagation = self.cancellation.deactivate_deferred();
        drop(data);
        self.finish_claimed_terminal(
            primary,
            None,
            propagation,
            cleanup,
            on_cleanup_settled,
            Ok(()),
        )
    }

    fn claim_owned(
        self: &Arc<Self>,
        owner_id: u64,
        evidence: SettlementEvidence,
        candidate: TerminalCandidate,
    ) -> ClaimOutcome {
        let mut data = self.lock_state();
        if data.state.is_terminal() {
            return ClaimOutcome::Closed;
        }
        if let Some(owner) = data.owner.as_mut()
            && let Some(claimed) = owner.claimed.as_mut()
        {
            match claimed.state {
                ClaimedTerminalState::Abandoned
                    if claimed.owner_id == owner_id
                        && claimed.evidence == evidence
                        && claimed.candidate == candidate =>
                {
                    // The arbiter winner is already frozen. The original owner may only recover
                    // this exact abandoned transaction; it does not make a second arbiter claim.
                    claimed.state = ClaimedTerminalState::Held;
                    return ClaimOutcome::Claimed(OperationSettlementClaim {
                        inner: Arc::clone(self),
                        owner_id,
                        active: true,
                    });
                }
                ClaimedTerminalState::Finishing | ClaimedTerminalState::Held => {
                    return ClaimOutcome::Observe;
                }
                ClaimedTerminalState::Abandoned => return ClaimOutcome::Rejected,
            }
        }
        let Some(primary) =
            primary_from_candidate(candidate.clone(), data.state, data.cancellation_reason)
        else {
            return ClaimOutcome::Rejected;
        };
        let winner = primary.winner_kind();
        let claim = {
            let Some(owner) = data.owner.as_mut() else {
                return ClaimOutcome::Rejected;
            };
            owner.arbiter.claim(owner_id, evidence.kind(), winner)
        };
        match claim {
            TerminalClaimResult::Claimed => {}
            TerminalClaimResult::Observe => return ClaimOutcome::Observe,
            TerminalClaimResult::RejectedOwner | TerminalClaimResult::RejectedEvidence => {
                return ClaimOutcome::Rejected;
            }
        }
        data.terminal_in_progress = true;
        // This only seals cancellation admission. Propagation remains part of the deferred finish;
        // a cancellation winner publishes its public state after the claim critical section.
        let propagation = self.cancellation.deactivate_deferred();
        let cancellation_winner = matches!(&primary, TerminalPrimary::Cancellation(_));
        let owner = data
            .owner
            .as_mut()
            .expect("claimed owned operation retains its authority");
        debug_assert!(owner.claimed.is_none());
        owner.claimed = Some(ClaimedTerminalTransaction {
            owner_id,
            evidence,
            candidate,
            primary,
            winner,
            propagation: Some(propagation),
            state: ClaimedTerminalState::Held,
        });
        let claim = OperationSettlementClaim {
            inner: Arc::clone(self),
            owner_id,
            active: true,
        };
        drop(data);
        if cancellation_winner {
            self.publish_claimed_cancellation();
        }
        ClaimOutcome::Claimed(claim)
    }

    fn finish_claimed(
        &self,
        owner_id: u64,
        domain_cleanup: Result<(), EasyConError>,
    ) -> TransitionOutcome {
        let (primary, winner, propagation, cleanup, on_cleanup_settled) = {
            let mut data = self.lock_state();
            if data.state.is_terminal() {
                return TransitionOutcome::AlreadyTerminal;
            }
            let Some(owner) = data.owner.as_mut() else {
                return TransitionOutcome::Invalid;
            };
            let Some(claimed) = owner.claimed.as_mut() else {
                return TransitionOutcome::Invalid;
            };
            if claimed.owner_id != owner_id || claimed.state != ClaimedTerminalState::Held {
                return TransitionOutcome::Invalid;
            }
            claimed.state = ClaimedTerminalState::Finishing;
            let primary = claimed.primary.clone();
            let winner = claimed.winner;
            let propagation = claimed
                .propagation
                .take()
                .expect("claimed transaction retains deferred cancellation propagation");
            let cleanup = owner
                .cleanup
                .take()
                .expect("owned operation cleanup is consumed once at finish");
            let on_cleanup_settled = owner
                .on_cleanup_settled
                .take()
                .expect("owned operation settlement observer is consumed once at finish");
            (primary, winner, propagation, cleanup, on_cleanup_settled)
        };
        self.finish_claimed_terminal(
            primary,
            Some((owner_id, winner)),
            propagation,
            cleanup,
            on_cleanup_settled,
            domain_cleanup,
        )
    }

    fn abandon_claimed(&self, owner_id: u64) {
        let abandoned = {
            let mut data = self.lock_state();
            let Some(owner) = data.owner.as_mut() else {
                return;
            };
            let Some(claimed) = owner.claimed.as_mut() else {
                return;
            };
            if claimed.owner_id != owner_id || claimed.state != ClaimedTerminalState::Held {
                return;
            }
            claimed.state = ClaimedTerminalState::Abandoned;
            true
        };
        if abandoned {
            self.changed.notify_all();
        }
    }

    fn record_for_handoff(
        &self,
        owner_id: u64,
        evidence: SettlementEvidence,
        candidate: TerminalCandidate,
    ) -> TransitionOutcome {
        let mut data = self.lock_state();
        if data.state.is_terminal() {
            return TransitionOutcome::AlreadyTerminal;
        }
        let Some(primary) =
            primary_from_candidate(candidate.clone(), data.state, data.cancellation_reason)
        else {
            return TransitionOutcome::Invalid;
        };
        let winner = primary.winner_kind();
        if !evidence_allows_winner(evidence.kind(), winner) {
            return TransitionOutcome::Invalid;
        }
        let Some(owner) = data.owner.as_mut() else {
            return TransitionOutcome::Invalid;
        };
        if owner.arbiter.primary_owner() != owner_id
            || owner.arbiter.transfer_owner().is_none()
            || owner.arbiter.winner().is_some()
            || owner.owner_task.is_none()
        {
            return TransitionOutcome::Invalid;
        }
        if let Some(existing) = &owner.handoff_record {
            return if existing == &(evidence, candidate) {
                TransitionOutcome::Unchanged
            } else {
                TransitionOutcome::Invalid
            };
        }
        owner.handoff_record = Some((evidence, candidate));
        TransitionOutcome::Applied
    }

    fn bind_owner_task(&self, owner_id: u64, task_id: TaskId) -> bool {
        let mut data = self.lock_state();
        let Some(owner) = data.owner.as_mut() else {
            return false;
        };
        if owner.arbiter.primary_owner() != owner_id || owner.owner_task.is_some() {
            return false;
        }
        owner.owner_task = Some(task_id);
        true
    }

    pub(crate) fn mark_owner_task_joined(&self, task_id: TaskId) {
        let mut data = self.lock_state();
        let Some(owner) = data.owner.as_mut() else {
            return;
        };
        if owner.owner_task != Some(task_id) {
            return;
        }
        let primary_owner = owner.arbiter.primary_owner();
        let _ = owner.arbiter.mark_primary_owner_joined(primary_owner);
    }

    pub(crate) fn settle_after_owner_join(&self) -> OperationFallbackOutcome {
        let mut data = self.lock_state();
        if data.state.is_terminal() {
            return OperationFallbackOutcome::AlreadyTerminal;
        }
        let operation_state = data.state;
        let cancellation_reason = data.cancellation_reason;
        let terminal_in_progress = data.terminal_in_progress;
        let Some(owner) = data.owner.as_mut() else {
            return OperationFallbackOutcome::Legacy;
        };
        if terminal_in_progress {
            let Some(claimed) = owner.claimed.as_ref() else {
                return OperationFallbackOutcome::OwnershipLost;
            };
            if claimed.state == ClaimedTerminalState::Finishing {
                // A legitimate owner already started its lock-free finish transaction. Preserve
                // the established close contract by observing that commit rather than reporting
                // ownership loss or inventing another result.
                drop(data);
                let _ = self.observe_terminal_completion();
                return OperationFallbackOutcome::Settled;
            }
            // A held or abandoned deferred claim may still require domain bookkeeping outside
            // Runtime. Close cannot manufacture that result and must remain fail-closed.
            return OperationFallbackOutcome::OwnershipLost;
        }
        if !owner.arbiter.primary_owner_joined() {
            return OperationFallbackOutcome::OwnershipLost;
        }
        let Some(transfer_owner) = owner.arbiter.transfer_owner() else {
            return OperationFallbackOutcome::OwnershipLost;
        };
        let Some((evidence, candidate)) = owner.handoff_record.take() else {
            return OperationFallbackOutcome::OwnershipLost;
        };
        let Some(primary) = primary_from_candidate(candidate, operation_state, cancellation_reason)
        else {
            return OperationFallbackOutcome::OwnershipLost;
        };
        let winner = primary.winner_kind();
        if !owner.arbiter.handoff(transfer_owner)
            || owner.arbiter.claim(transfer_owner, evidence.kind(), winner)
                != TerminalClaimResult::Claimed
        {
            return OperationFallbackOutcome::OwnershipLost;
        }
        data.terminal_in_progress = true;
        let cancellation_winner = matches!(&primary, TerminalPrimary::Cancellation(_));
        let owner = data
            .owner
            .as_mut()
            .expect("handoff retains shared owner record");
        let cleanup = owner
            .cleanup
            .take()
            .expect("handoff cleanup is pre-registered once");
        let on_cleanup_settled = owner
            .on_cleanup_settled
            .take()
            .expect("handoff observer is pre-registered once");
        let propagation = self.cancellation.deactivate_deferred();
        drop(data);
        if cancellation_winner {
            self.publish_claimed_cancellation();
        }
        let _ = self.finish_claimed_terminal(
            primary,
            Some((transfer_owner, winner)),
            propagation,
            cleanup,
            on_cleanup_settled,
            Ok(()),
        );
        OperationFallbackOutcome::Settled
    }

    fn publish_claimed_cancellation(&self) {
        let published = {
            let mut data = self.lock_state();
            debug_assert!(data.terminal_in_progress);
            debug_assert!(data.cancellation_reason.is_some());
            match data.state {
                OperationState::Pending | OperationState::Running => {
                    data.state = OperationState::Cancelling;
                    true
                }
                OperationState::Cancelling => false,
                OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled => {
                    false
                }
            }
        };
        if published {
            self.publish_state(OperationState::Cancelling);
            self.changed.notify_all();
        }
    }

    fn finish_claimed_terminal<C, S>(
        &self,
        primary: TerminalPrimary,
        owner_commit: Option<(u64, TerminalWinnerKind)>,
        propagation: crate::cancellation::CancellationPropagation,
        cleanup: C,
        on_cleanup_settled: S,
        domain_cleanup: Result<(), EasyConError>,
    ) -> TransitionOutcome
    where
        C: FnOnce() -> Result<(), EasyConError>,
        S: FnOnce(Result<(), EasyConError>),
    {
        propagation.propagate();
        let runtime_cleanup = match catch_isolated(cleanup) {
            Ok(outcome) => outcome,
            Err(()) => Err(cleanup_panic_error()),
        };
        let domain_failed = domain_cleanup.is_err();
        let mut cleanup_outcome = match domain_cleanup {
            Err(error) => Err(error),
            Ok(()) => runtime_cleanup,
        };
        if catch_isolated(|| on_cleanup_settled(cleanup_outcome.clone())).is_err()
            && cleanup_outcome.is_ok()
        {
            cleanup_outcome = Err(cleanup_settlement_panic_error());
        }
        let cleanup_failed = cleanup_outcome.is_err();
        let mut data = self.lock_state();
        if let Some((owner_id, winner)) = owner_commit {
            let owner = data
                .owner
                .as_mut()
                .expect("owned claim retains arbiter through commit");
            assert!(
                owner.arbiter.commit(owner_id, winner),
                "settlement owner commits its claimed winner once"
            );
            debug_assert!(owner.arbiter.committed());
        }
        data.terminal_in_progress = false;
        data.result = None;
        data.error = None;
        if domain_failed {
            data.state = OperationState::Failed;
            data.error = cleanup_outcome.err();
        } else {
            match primary {
                TerminalPrimary::Success(result) => match cleanup_outcome {
                    Ok(()) => {
                        data.state = OperationState::Succeeded;
                        data.result = Some(result);
                    }
                    Err(error) => {
                        data.state = OperationState::Failed;
                        data.error = Some(error);
                    }
                },
                TerminalPrimary::Failure(error) => {
                    data.state = OperationState::Failed;
                    data.error = Some(error);
                }
                TerminalPrimary::Cancellation(reason) => {
                    data.state = OperationState::Cancelled;
                    data.error = Some(cancellation_error(reason));
                }
            }
        }
        let outcome = if cleanup_failed {
            TransitionOutcome::CleanupFailed
        } else {
            TransitionOutcome::Applied
        };
        self.publish(&data);
        unlink_then_notify(
            || self.unlink_registry(),
            || {
                drop(data);
                self.changed.notify_all();
            },
        );
        outcome
    }

    fn observe_terminal_completion(&self) -> TransitionOutcome {
        let mut data = self.lock_state();
        while !data.state.is_terminal() {
            #[cfg(test)]
            if let Some(observer) = self
                .wait_observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop()
            {
                let _ = observer.send(());
            }
            data = self
                .changed
                .wait(data)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        TransitionOutcome::AlreadyTerminal
    }

    fn unlink_registry(&self) {
        if let Some(runtime) = self.runtime.upgrade() {
            let unlinked = catch_isolated(|| {
                assert!(
                    !runtime.take_operation_registry_unlink_failpoint(),
                    "failpoint:runtime.operation.registry_unlink"
                );
                runtime.unregister_operation(self.id);
            });
            if unlinked.is_err() {
                let _ = catch_isolated(|| runtime.unregister_operation(self.id));
            }
        }
    }

    fn snapshot(&self) -> OperationSnapshot {
        snapshot_data(&self.lock_state())
    }

    fn lock_state(&self) -> MutexGuard<'_, OperationData> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait(&self, wait: WaitTimeout) -> WaitResult {
        let started = Instant::now();
        let mut data = self.lock_state();
        loop {
            if data.state.is_terminal() {
                return WaitResult::Completed(snapshot_data(&data));
            }
            #[cfg(test)]
            if let Some(observer) = self
                .wait_observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop()
            {
                let _ = observer.send(());
            }
            match wait {
                WaitTimeout::Poll => return WaitResult::Timeout,
                WaitTimeout::Infinite => {
                    data = self
                        .changed
                        .wait(data)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                WaitTimeout::For(limit) => {
                    let Some(remaining) = limit.checked_sub(started.elapsed()) else {
                        return WaitResult::Timeout;
                    };
                    let (next, timed_out) = self
                        .changed
                        .wait_timeout(data, remaining)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    data = next;
                    if timed_out.timed_out() && !data.state.is_terminal() {
                        return WaitResult::Timeout;
                    }
                }
            }
        }
    }

    fn publish(&self, data: &OperationData) {
        self.publish_state(data.state);
    }

    fn publish_state(&self, state: OperationState) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let (kind, code, severity) = if state.is_terminal() {
            let severity = if state == OperationState::Failed {
                Severity::Error
            } else {
                Severity::Info
            };
            (EventKind::Terminal, terminal_code(state), severity)
        } else {
            (EventKind::State, state_code(state), Severity::Info)
        };
        let draft = EventDraft::critical(kind, code, severity);
        let _ = catch_isolated(|| {
            assert!(
                !(state.is_terminal() && runtime.take_operation_terminal_event_failpoint()),
                "failpoint:runtime.operation.terminal_event"
            );
            let _ = runtime.try_publish_event(draft.with_operation(self.id));
        });
    }
}

fn primary_from_candidate(
    candidate: TerminalCandidate,
    state: OperationState,
    cancellation_reason: Option<CancellationReason>,
) -> Option<TerminalPrimary> {
    match (state, candidate) {
        (OperationState::Running, TerminalCandidate::Success(result)) => {
            Some(TerminalPrimary::Success(result))
        }
        (OperationState::Pending | OperationState::Running, TerminalCandidate::Failure(error)) => {
            Some(TerminalPrimary::Failure(error))
        }
        (OperationState::Pending | OperationState::Running, TerminalCandidate::Cancellation) => {
            cancellation_reason.map(TerminalPrimary::Cancellation)
        }
        _ => None,
    }
}

fn cancellation_error(reason: CancellationReason) -> EasyConError {
    let (code, message) = match reason {
        CancellationReason::Requested => (ErrorCode::Cancelled, "operation cancelled"),
        CancellationReason::Deadline => (ErrorCode::DeadlineExceeded, "operation deadline elapsed"),
        CancellationReason::ParentClose => (
            ErrorCode::Cancelled,
            "parent operation, resource, or Runtime ended",
        ),
    };
    EasyConError::new(ErrorDomain::Runtime, code, message)
}

fn cleanup_panic_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Internal,
        ErrorCode::Internal,
        "operation owner cleanup panicked",
    )
}

fn cleanup_settlement_panic_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Internal,
        ErrorCode::Internal,
        "operation cleanup settlement observer panicked",
    )
}

fn discard_terminal_callbacks<C, S>(cleanup: C, on_cleanup_settled: S) {
    let _ = drop_isolated(cleanup);
    let _ = drop_isolated(on_cleanup_settled);
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
