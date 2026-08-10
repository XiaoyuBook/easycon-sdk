use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::task::{Context, Poll, Waker};

use easycon_model::{
    Button, EasyConError, ErrorCode, ErrorDomain, Hat, OperationId, ResourceId, StickPosition,
};
use easycon_runtime::{
    CancellationHookRegistration, CancellationReason, CancellationToken, ClaimOutcome, Clock,
    ClockChangeRegistration, DeadlineId, DeadlineRegistration, DeadlineResolution, DeadlineSignal,
    EventDraft, EventKind, ManagedResource, Operation, OperationSettlementClaim,
    OperationSettlementOwner, OperationState, OperationValue, ResourceRegistration, Runtime,
    SettlementEvidence, SettlementOwnerMode, Severity, SupervisedTask, SupervisedTaskOutcome,
    TerminalCandidate, TransitionOutcome,
};

use crate::amiibo::{
    AMIIBO_ACK, AMIIBO_CHUNK_SIZE, AMIIBO_DEFAULT_RESET_TIMEOUT_NS, AMIIBO_RESET_REPLY,
    AmiiboLimits, AmiiboSaveOptions, AmiiboSelectOptions, MAX_AMIIBO_CHUNK_RETRIES, reset_command,
    save_header, select_command,
};
use crate::protocol::SwitchReport;
use crate::sequence::PreciseSequence;
use crate::transport::{
    AUTO_BAUD_RATES, AckRequest, ControllerTransport, DirectWriteTiming, HANDSHAKE_REPLY,
    HANDSHAKE_REQUEST, HandshakeRequest, TransportError, TransportErrorKind, WriteContext,
    WriteKind, WriteRequest, WriteSettlement, WriteSettlementOutcome,
};

/// Controller connection/resource state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerState {
    /// No open transport; connect may be submitted.
    Disconnected,
    /// One connect operation owns handshake attempts.
    Connecting,
    /// Reports may be submitted.
    Connected,
    /// Safety cleanup is closing the transport.
    Disconnecting,
    /// Resource close completed and the lane joined.
    Closed,
}

/// Authoritative controller write-lease owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerLeaseState {
    /// Direct writes and sequence admission are available.
    Available,
    /// A precise sequence owns writes until terminal cleanup.
    Sequence(OperationId),
    /// A future Automation adapter owns the primitive lease.
    Automation(u64),
}

/// Immutable state query snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerSnapshot {
    /// Authoritative connection state.
    pub state: ControllerState,
    /// Lane-owned desired report.
    pub desired_report: SwitchReport,
    /// Number of complete logical reports accepted by transport.
    pub accepted_report_count: u64,
    /// Runtime-clock time of the latest complete report acceptance.
    pub last_report_timestamp_ns: Option<u64>,
    /// Current exclusive write owner.
    pub lease: ControllerLeaseState,
}

impl Default for ControllerSnapshot {
    fn default() -> Self {
        Self {
            state: ControllerState::Disconnected,
            desired_report: SwitchReport::NEUTRAL,
            accepted_report_count: 0,
            last_report_timestamp_ns: None,
            lease: ControllerLeaseState::Available,
        }
    }
}

/// Runtime options for one controller lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerOptions {
    /// Minimum interval between complete report acceptances.
    pub minimum_report_interval_ns: u64,
    /// Maximum duration allowed for one complete logical transport write.
    pub write_timeout_ns: u64,
    /// Explicit hardware-derived Amiibo limits; absent by default in Phase 2A.
    pub amiibo_limits: Option<AmiiboLimits>,
}

impl Default for ControllerOptions {
    fn default() -> Self {
        Self {
            minimum_report_interval_ns: 30_000_000,
            write_timeout_ns: 1_000_000_000,
            amiibo_limits: None,
        }
    }
}

/// Independent connect and protocol timeout limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectOptions {
    /// Absolute operation execution deadline, if any.
    pub operation_deadline_ns: Option<u64>,
    /// Per-baud hello timeout.
    pub protocol_timeout_ns: u64,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            operation_deadline_ns: None,
            protocol_timeout_ns: 1_000_000_000,
        }
    }
}

/// One desired-state mutation submitted to the single writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerAction {
    /// Set one button bit.
    ButtonDown(Button),
    /// Clear one button bit.
    ButtonUp(Button),
    /// Replace the complete HAT value.
    Hat(Hat),
    /// Replace the left stick coordinates.
    LeftStick(StickPosition),
    /// Replace the right stick coordinates.
    RightStick(StickPosition),
    /// Reset every desired input to neutral.
    Reset,
}

impl ControllerAction {
    fn apply(self, report: &mut SwitchReport) {
        match self {
            Self::ButtonDown(button) => report.press(button),
            Self::ButtonUp(button) => report.release(button),
            Self::Hat(hat) => report.set_hat(hat),
            Self::LeftStick(position) => report.set_left_stick(position),
            Self::RightStick(position) => report.set_right_stick(position),
            Self::Reset => report.reset(),
        }
    }
}

/// Cloneable Controller resource backed by one dedicated writer thread.
#[derive(Clone)]
pub struct ControllerSession {
    inner: Arc<ControllerInner>,
}

/// Final outcome of one Automation lease acquisition request.
pub enum AutomationLeaseAcquireOutcome {
    /// The request owns the returned non-zero generation.
    Granted(AutomationLease),
    /// Caller cancellation won the request completion record.
    Cancelled,
    /// The Runtime-clock absolute deadline won the request completion record.
    Deadline,
    /// Controller close won the request completion record.
    Closed,
    /// Admission failed without a higher-priority signal.
    Failure(EasyConError),
}

/// Registered completion handle for one Automation lease acquisition.
pub struct AutomationLeaseAcquire {
    record: Option<Arc<AcquireRecord>>,
    _cancellation_registration: Option<CancellationHookRegistration>,
}

/// Settled neutralization result for one Automation lease generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomationLeaseReleaseOutcome {
    /// A complete neutral report was accepted by transport.
    NeutralAccepted,
    /// Neutral was not delivered, but transport close proved the stream settled.
    NeutralNotDeliveredStreamSettled(EasyConError),
}

/// Registered completion handle for one Automation lease release.
pub struct AutomationLeaseRelease {
    record: Option<Arc<ReleaseRecord>>,
}

/// Exclusive primitive reserved for the future Automation adapter.
pub struct AutomationLease {
    controller_id: ResourceId,
    controller_identity: Arc<()>,
    generation: Arc<LeaseGeneration>,
}

impl AutomationLease {
    /// Controller resource that issued this lease.
    #[must_use]
    pub const fn controller_id(&self) -> ResourceId {
        self.controller_id
    }

    /// Runtime-local lease generation.
    #[must_use]
    pub fn lease_id(&self) -> u64 {
        self.generation.id
    }

    /// Seals action admission and starts one settled neutralization/release cleanup.
    pub fn neutralize_and_release(self) -> AutomationLeaseRelease {
        AutomationLeaseRelease {
            record: Some(self.generation.request_cleanup()),
        }
    }
}

impl Drop for AutomationLease {
    fn drop(&mut self) {
        let _ = self.generation.request_cleanup();
    }
}

struct AcquireRecord {
    cancellation: CancellationToken,
    deadline_ns: Option<u64>,
    clock: Arc<dyn Clock>,
    closing: Arc<AtomicBool>,
    deadline_signal: Option<DeadlineSignal>,
    deadline_registration: Mutex<Option<DeadlineRegistration>>,
    state: Mutex<AcquireRecordState>,
}

struct AcquireRecordState {
    status: AcquireStatus,
    waker: Option<Waker>,
}

enum AcquireStatus {
    Pending,
    /// The record has a single resolver, but its outcome is still outside the record lock.
    /// This prevents a public Clock, Runtime event, or lease Drop path from running while the
    /// record mutex is held.
    Resolving {
        abandoned: bool,
    },
    Ready(Option<AutomationLeaseAcquireOutcome>),
    Abandoned,
}

impl AcquireRecord {
    fn new(
        cancellation: CancellationToken,
        deadline_ns: Option<u64>,
        clock: Arc<dyn Clock>,
        closing: Arc<AtomicBool>,
        deadline_registration: Option<DeadlineRegistration>,
    ) -> Self {
        let deadline_signal = deadline_registration
            .as_ref()
            .map(DeadlineRegistration::signal);
        Self {
            cancellation,
            deadline_ns,
            clock,
            closing,
            deadline_signal,
            deadline_registration: Mutex::new(deadline_registration),
            state: Mutex::new(AcquireRecordState {
                status: AcquireStatus::Pending,
                waker: None,
            }),
        }
    }

    fn resolve_with(&self, fallback: impl FnOnce() -> AutomationLeaseAcquireOutcome) -> bool {
        let observed = self.observed_outcome();
        if !self.reserve_resolution() {
            return false;
        }
        let outcome = match observed {
            Some(outcome) => outcome,
            None => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(fallback)) {
                Ok(outcome) => outcome,
                Err(payload) => {
                    // A foreign panic payload may panic again during Drop. The record already
                    // owns its resolver slot, so contain it and publish one typed failure.
                    std::mem::forget(payload);
                    acquire_resolution_panic_error()
                }
            },
        };
        self.publish_reserved(outcome)
    }

    fn resolve_deadline(&self, resolution: DeadlineResolution) {
        match resolution {
            DeadlineResolution::Fired => {
                let _ = self.resolve_with(|| AutomationLeaseAcquireOutcome::Deadline);
            }
            DeadlineResolution::Disarmed | DeadlineResolution::RuntimeClosed => {
                let _ = self.resolve_with(|| AutomationLeaseAcquireOutcome::Closed);
            }
        }
    }

    fn resolve_visible_signal(&self) {
        if let Some(outcome) = self.observed_outcome() {
            let _ = self.resolve_with(|| outcome);
        }
    }

    fn observed_outcome(&self) -> Option<AutomationLeaseAcquireOutcome> {
        if self.cancellation.is_cancelled() {
            return Some(AutomationLeaseAcquireOutcome::Cancelled);
        }
        if self.deadline_signal.as_ref().is_some_and(|signal| {
            matches!(signal.resolution(), Some(outcome) if outcome.resolution == DeadlineResolution::Fired)
        }) {
            return Some(AutomationLeaseAcquireOutcome::Deadline);
        }
        if let Some(deadline) = self.deadline_ns {
            let now = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.clock.now_ns()
            })) {
                Ok(now) => now,
                Err(payload) => {
                    // The Clock is a public Runtime boundary. Do not poison an acquire record
                    // merely because it panics while the record is being observed.
                    std::mem::forget(payload);
                    return Some(acquire_clock_panic_error());
                }
            };
            if now >= deadline {
                return Some(AutomationLeaseAcquireOutcome::Deadline);
            }
        }
        if self.closing.load(Ordering::Acquire) {
            return Some(AutomationLeaseAcquireOutcome::Closed);
        }
        None
    }

    fn reserve_resolution(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("lease acquire record lock poisoned");
        if !matches!(state.status, AcquireStatus::Pending) {
            return false;
        }
        state.status = AcquireStatus::Resolving { abandoned: false };
        true
    }

    fn publish_reserved(&self, outcome: AutomationLeaseAcquireOutcome) -> bool {
        let (waker, abandoned_outcome) = {
            let mut state = self
                .state
                .lock()
                .expect("lease acquire record lock poisoned");
            match std::mem::replace(&mut state.status, AcquireStatus::Abandoned) {
                AcquireStatus::Resolving { abandoned: false } => {
                    state.status = AcquireStatus::Ready(Some(outcome));
                    (state.waker.take(), None)
                }
                AcquireStatus::Resolving { abandoned: true } => (None, Some(outcome)),
                other => {
                    state.status = other;
                    return false;
                }
            }
        };
        self.disarm_deadline();
        if let Some(waker) = waker {
            waker.wake();
        }
        drop(abandoned_outcome);
        true
    }

    fn disarm_deadline(&self) {
        drop(
            self.deadline_registration
                .lock()
                .expect("lease acquire deadline lock poisoned")
                .take(),
        );
    }

    fn abandon(&self) {
        let outcome = {
            let mut state = self
                .state
                .lock()
                .expect("lease acquire record lock poisoned");
            let outcome = match std::mem::replace(&mut state.status, AcquireStatus::Abandoned) {
                AcquireStatus::Ready(outcome) => outcome,
                AcquireStatus::Resolving { .. } => {
                    state.status = AcquireStatus::Resolving { abandoned: true };
                    None
                }
                AcquireStatus::Pending | AcquireStatus::Abandoned => None,
            };
            state.waker = None;
            outcome
        };
        self.disarm_deadline();
        drop(outcome);
    }
}

impl Future for AutomationLeaseAcquire {
    type Output = AutomationLeaseAcquireOutcome;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let record = this
            .record
            .as_ref()
            .expect("AutomationLeaseAcquire polled after completion")
            .clone();
        if let Some(signal) = &record.deadline_signal
            && let Poll::Ready(outcome) = signal.poll_resolution(context)
        {
            record.resolve_deadline(outcome.resolution);
        }
        record.resolve_visible_signal();
        let mut state = record
            .state
            .lock()
            .expect("lease acquire record lock poisoned");
        match &mut state.status {
            AcquireStatus::Ready(outcome) => {
                let outcome = outcome
                    .take()
                    .expect("lease acquire outcome consumed exactly once");
                drop(state);
                this.record = None;
                Poll::Ready(outcome)
            }
            AcquireStatus::Pending | AcquireStatus::Resolving { .. } => {
                state.waker = Some(context.waker().clone());
                Poll::Pending
            }
            AcquireStatus::Abandoned => panic!("abandoned AutomationLeaseAcquire was polled"),
        }
    }
}

impl Drop for AutomationLeaseAcquire {
    fn drop(&mut self) {
        if let Some(record) = self.record.take() {
            record.abandon();
        }
    }
}

struct ReleaseRecord {
    cancellation: CancellationToken,
    state: Mutex<ReleaseRecordState>,
}

/// Retains the exclusive Runtime settlement capability while a direct report or precise
/// sequence completes its required Controller cleanup. Only the effect-completing report may
/// consume it through the backend final-byte gate.
struct OperationSettlementRecord {
    state: Mutex<OperationSettlementState>,
}

/// A report operation retained through final transport close so a proven close failure can win
/// over the earlier close cancellation intent.
enum CloseOperationSettlement {
    Owner {
        owner: OperationSettlementOwner,
        operation: Operation,
    },
    Record {
        record: Arc<OperationSettlementRecord>,
        operation: Operation,
    },
}

impl CloseOperationSettlement {
    fn settle_cancellation(self) {
        match self {
            Self::Owner { owner, operation } => settle_owned_cancellation(owner, &operation),
            Self::Record { record, operation } => record.settle_cancellation(&operation),
        }
    }

    fn settle_strict_failure(self, error: EasyConError) {
        match self {
            Self::Owner { owner, .. } => settle_owned_strict_failure(owner, error),
            Self::Record { record, .. } => record.settle_strict_failure(error),
        }
    }
}

enum OperationSettlementState {
    Pending(OperationSettlementOwner),
    FullAcceptedReserved {
        owner: OperationSettlementOwner,
        strict_failure: Option<EasyConError>,
    },
    ClaimingFull {
        strict_failure: Option<EasyConError>,
    },
    ClaimedFull(OperationSettlementClaim),
    Finished,
    Failed,
}

impl OperationSettlementRecord {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, OperationSettlementState> {
        // This record retains the frozen physical-acceptance owner. An isolated observer panic
        // must not make the lane lose that owner or turn a final-byte success into a hang.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn new(owner: OperationSettlementOwner) -> Self {
        Self {
            state: Mutex::new(OperationSettlementState::Pending(owner)),
        }
    }

    /// Reserves the owned record in the same short transaction as the backend final-byte gate.
    /// This performs no Runtime work; cancellation and close can no longer take the owner after
    /// it succeeds.
    fn reserve_full_accepted(&self) -> bool {
        let mut state = self.lock_state();
        let owner = match std::mem::replace(&mut *state, OperationSettlementState::Failed) {
            OperationSettlementState::Pending(owner) => owner,
            other => {
                *state = other;
                return false;
            }
        };
        *state = OperationSettlementState::FullAcceptedReserved {
            owner,
            strict_failure: None,
        };
        true
    }

    /// Claims Runtime success after the backend gate has already reserved physical acceptance.
    /// The Runtime claim is deliberately outside this record lock, and no terminal result is
    /// committed here.
    fn claim_reserved_full_accepted(&self) -> bool {
        let owner = {
            let mut state = self.lock_state();
            match std::mem::replace(
                &mut *state,
                OperationSettlementState::ClaimingFull {
                    strict_failure: None,
                },
            ) {
                OperationSettlementState::FullAcceptedReserved {
                    owner,
                    strict_failure,
                } => {
                    *state = OperationSettlementState::ClaimingFull { strict_failure };
                    owner
                }
                other => {
                    *state = other;
                    return false;
                }
            }
        };
        let outcome = owner.claim(
            SettlementEvidence::EffectAccepted,
            TerminalCandidate::Success(OperationValue::Unit),
        );
        self.complete_full_claim(outcome)
    }

    fn complete_full_claim(&self, outcome: ClaimOutcome) -> bool {
        let strict_finish = {
            let mut state = self.lock_state();
            let OperationSettlementState::ClaimingFull { strict_failure } = &mut *state else {
                if let ClaimOutcome::Claimed(claim) = outcome {
                    drop(claim);
                }
                return false;
            };
            match outcome {
                ClaimOutcome::Claimed(claim) => {
                    if let Some(error) = strict_failure.take() {
                        *state = OperationSettlementState::Failed;
                        Some((claim, error))
                    } else {
                        *state = OperationSettlementState::ClaimedFull(claim);
                        None
                    }
                }
                ClaimOutcome::Observe | ClaimOutcome::Rejected | ClaimOutcome::Closed => {
                    *state = OperationSettlementState::Failed;
                    None
                }
            }
        };
        if let Some((claim, error)) = strict_finish {
            let _ = claim.finish(Err(error));
            return false;
        }
        self.claimed_full()
    }

    fn claimed_full(&self) -> bool {
        matches!(*self.lock_state(), OperationSettlementState::ClaimedFull(_))
    }

    fn is_pending(&self) -> bool {
        matches!(
            *self.lock_state(),
            OperationSettlementState::Pending(_)
                | OperationSettlementState::FullAcceptedReserved { .. }
                | OperationSettlementState::ClaimingFull { .. }
                | OperationSettlementState::ClaimedFull(_)
        )
    }

    fn is_finished(&self) -> bool {
        matches!(
            *self.lock_state(),
            OperationSettlementState::Finished | OperationSettlementState::Failed
        )
    }

    /// Commits the already gate-claimed success only after the lane has recorded acceptance and
    /// released any final sequence bookkeeping.
    fn finish_success(&self) -> bool {
        let claim = {
            let mut state = self.lock_state();
            match std::mem::replace(&mut *state, OperationSettlementState::Finished) {
                OperationSettlementState::ClaimedFull(claim) => claim,
                other => {
                    *state = other;
                    return false;
                }
            }
        };
        let _ = claim.finish(Ok(()));
        true
    }

    /// A lane panic after physical acceptance retains enough backend evidence in the shared
    /// record to claim and finish success during join takeover. Resource ownership loss is
    /// reported separately by Runtime close; it cannot rewrite final-byte acceptance.
    fn finish_claimed_after_lane_join(&self) -> bool {
        if !self.claimed_full() {
            let _ = self.claim_reserved_full_accepted();
        }
        self.finish_success()
    }

    fn settle_cancellation(&self, operation: &Operation) {
        let owner = {
            let mut state = self.lock_state();
            match std::mem::replace(&mut *state, OperationSettlementState::Finished) {
                OperationSettlementState::Pending(owner) => Some(owner),
                other => {
                    *state = other;
                    None
                }
            }
        };
        if let Some(owner) = owner {
            settle_owned_cancellation(owner, operation);
        }
    }

    fn settle_failure(&self, operation: &Operation, error: EasyConError) {
        let owner = {
            let mut state = self.lock_state();
            match std::mem::replace(&mut *state, OperationSettlementState::Failed) {
                OperationSettlementState::Pending(owner) => Some(owner),
                other => {
                    *state = other;
                    None
                }
            }
        };
        if let Some(owner) = owner {
            settle_owned_failure(owner, operation, error);
        }
    }

    /// Strict cleanup failure is never allowed to fall back to generic cancellation precedence.
    fn settle_strict_failure(&self, error: EasyConError) {
        enum StrictFinish {
            Owner(OperationSettlementOwner),
            Claim(OperationSettlementClaim),
        }

        let finish = {
            let mut state = self.lock_state();
            match std::mem::replace(&mut *state, OperationSettlementState::Failed) {
                OperationSettlementState::Pending(owner) => Some(StrictFinish::Owner(owner)),
                OperationSettlementState::FullAcceptedReserved {
                    owner,
                    strict_failure,
                } => {
                    *state = OperationSettlementState::FullAcceptedReserved {
                        owner,
                        strict_failure: strict_failure.or(Some(error.clone())),
                    };
                    None
                }
                OperationSettlementState::ClaimingFull { strict_failure } => {
                    *state = OperationSettlementState::ClaimingFull {
                        strict_failure: strict_failure.or(Some(error.clone())),
                    };
                    None
                }
                OperationSettlementState::ClaimedFull(claim) => Some(StrictFinish::Claim(claim)),
                other => {
                    *state = other;
                    None
                }
            }
        };
        match finish {
            Some(StrictFinish::Owner(owner)) => settle_owned_strict_failure(owner, error),
            Some(StrictFinish::Claim(claim)) => {
                let _ = claim.finish(Err(error));
            }
            None => {}
        }
    }
}

#[derive(Default)]
struct ControllerCleanupFailure {
    first: Mutex<Option<EasyConError>>,
    ownership_lost: AtomicBool,
}

impl ControllerCleanupFailure {
    fn record(&self, error: EasyConError) -> EasyConError {
        let mut first = self
            .first
            .lock()
            .expect("controller cleanup failure lock poisoned");
        if first.is_none() {
            *first = Some(error);
        }
        first
            .as_ref()
            .expect("controller cleanup failure was recorded")
            .clone()
    }

    fn get(&self) -> Option<EasyConError> {
        self.first
            .lock()
            .expect("controller cleanup failure lock poisoned")
            .clone()
    }

    fn record_ownership_loss(&self, error: EasyConError) -> EasyConError {
        let mut first = self
            .first
            .lock()
            .expect("controller cleanup failure lock poisoned");
        if first.is_none() {
            *first = Some(error);
            self.ownership_lost.store(true, Ordering::Release);
        }
        first
            .as_ref()
            .expect("controller ownership loss was recorded")
            .clone()
    }

    fn ownership_lost(&self) -> bool {
        self.ownership_lost.load(Ordering::Acquire)
    }
}

struct ReleaseRecordState {
    phase: ReleasePhase,
    waker: Option<Waker>,
}

enum ReleasePhase {
    Undispatched {
        close_acquired: bool,
    },
    Outstanding {
        close_acquired: bool,
    },
    GateClaimed {
        close_acquired_before_claim: bool,
    },
    Committed {
        close_consumed: bool,
        outcome: Result<AutomationLeaseReleaseOutcome, EasyConError>,
    },
}

impl ReleaseRecord {
    fn new() -> Self {
        Self {
            cancellation: CancellationToken::root(),
            state: Mutex::new(ReleaseRecordState {
                phase: ReleasePhase::Undispatched {
                    close_acquired: false,
                },
                waker: None,
            }),
        }
    }

    fn begin_neutral(&self) -> (CancellationToken, bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let close_owned = match &state.phase {
            ReleasePhase::Undispatched { close_acquired } => *close_acquired,
            _ => panic!("Automation release neutral dispatches exactly once"),
        };
        state.phase = ReleasePhase::Outstanding {
            close_acquired: close_owned,
        };
        let cancellation = if close_owned {
            CancellationToken::root()
        } else {
            self.cancellation.clone()
        };
        (cancellation, close_owned)
    }

    /// Freezes the close ordering at the same physical final-byte acceptance gate that commits
    /// the release neutral. The write-return path cannot later reinterpret this ordering.
    fn reserve_full_accepted(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let close_acquired_before_claim = match &state.phase {
            ReleasePhase::Outstanding { close_acquired } => *close_acquired,
            _ => return false,
        };
        state.phase = ReleasePhase::GateClaimed {
            close_acquired_before_claim,
        };
        true
    }

    /// Atomically records whether resource close acquired this record before completion.
    ///
    /// A settled lane-owned record cannot consume a later close: that close needs its own final
    /// paced neutral. A close-owned settled record proves the opposite ordering and consumes the
    /// same release neutral.
    fn request_close_interrupt(&self) -> bool {
        let (close_consumed, interrupt) = {
            let mut state = self
                .state
                .lock()
                .expect("lease release record lock poisoned");
            match &mut state.phase {
                ReleasePhase::Undispatched { close_acquired } => {
                    *close_acquired = true;
                    (true, false)
                }
                ReleasePhase::Outstanding { close_acquired } => {
                    let interrupt = !*close_acquired;
                    *close_acquired = true;
                    (true, interrupt)
                }
                ReleasePhase::GateClaimed {
                    close_acquired_before_claim,
                } => (*close_acquired_before_claim, false),
                ReleasePhase::Committed { close_consumed, .. } => (*close_consumed, false),
            }
        };
        if interrupt {
            self.cancellation.cancel();
        }
        close_consumed
    }

    /// Completes the record and atomically reports whether close acquired it before completion.
    fn complete(&self, outcome: Result<AutomationLeaseReleaseOutcome, EasyConError>) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("lease release record lock poisoned");
        let close_consumed = match &state.phase {
            ReleasePhase::Undispatched { close_acquired }
            | ReleasePhase::Outstanding { close_acquired } => *close_acquired,
            ReleasePhase::GateClaimed {
                close_acquired_before_claim,
            } => *close_acquired_before_claim,
            ReleasePhase::Committed { .. } => return false,
        };
        state.phase = ReleasePhase::Committed {
            close_consumed,
            outcome,
        };
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        close_consumed
    }
}

fn release_write_settlement(record: Arc<ReleaseRecord>) -> WriteSettlement {
    WriteSettlement::tracked_with_full_reservation(
        Some(Box::new(move || record.reserve_full_accepted())),
        None,
    )
}

impl Future for AutomationLeaseRelease {
    type Output = Result<AutomationLeaseReleaseOutcome, EasyConError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let record = this
            .record
            .as_ref()
            .expect("AutomationLeaseRelease polled after completion")
            .clone();
        let mut state = record
            .state
            .lock()
            .expect("lease release record lock poisoned");
        if let ReleasePhase::Committed { outcome, .. } = &state.phase {
            let outcome = outcome.clone();
            drop(state);
            this.record = None;
            Poll::Ready(outcome)
        } else {
            state.waker = Some(context.waker().clone());
            Poll::Pending
        }
    }
}

#[derive(Default)]
struct ReleaseInterruptRegistry {
    state: Mutex<ReleaseInterruptState>,
}

#[derive(Default)]
struct ReleaseInterruptState {
    records: Vec<Weak<ReleaseRecord>>,
    retained_ownership_loss_records: Vec<Arc<ReleaseRecord>>,
    close_requested: bool,
    stream_settled: bool,
    cleanup_projection: Option<ReleaseCleanupProjection>,
}

#[derive(Clone)]
enum ReleaseCleanupProjection {
    Proven(EasyConError),
    OwnershipLost,
}

impl ReleaseInterruptRegistry {
    fn track(&self, record: &Arc<ReleaseRecord>) {
        let (close_requested, stream_settled, cleanup_failure, ownership_lost) = {
            let mut state = self.state.lock().expect("release registry lock poisoned");
            state
                .records
                .retain(|existing| existing.upgrade().is_some());
            state.records.push(Arc::downgrade(record));
            let cleanup_failure = match state.cleanup_projection.as_ref() {
                Some(ReleaseCleanupProjection::Proven(error)) => Some(error.clone()),
                Some(ReleaseCleanupProjection::OwnershipLost) => None,
                None => None,
            };
            let ownership_lost = matches!(
                state.cleanup_projection.as_ref(),
                Some(ReleaseCleanupProjection::OwnershipLost)
            );
            if ownership_lost {
                state.retained_ownership_loss_records.push(record.clone());
            }
            (
                state.close_requested,
                state.stream_settled,
                cleanup_failure,
                ownership_lost,
            )
        };
        if ownership_lost {
            return;
        }
        if let Some(error) = cleanup_failure {
            record.complete(Err(error));
        } else if stream_settled {
            record.complete(Ok(
                AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(
                    closed_lane_error(),
                ),
            ));
        } else if close_requested {
            record.request_close_interrupt();
        }
    }

    fn record_cleanup_failure(&self, error: EasyConError) {
        let (error, records) = {
            let mut state = self.state.lock().expect("release registry lock poisoned");
            if state.cleanup_projection.is_none() {
                state.cleanup_projection = Some(ReleaseCleanupProjection::Proven(error));
            }
            let error = match state.cleanup_projection.as_ref() {
                Some(ReleaseCleanupProjection::Proven(first)) => first.clone(),
                Some(ReleaseCleanupProjection::OwnershipLost) => return,
                None => unreachable!("cleanup projection was initialized"),
            };
            state
                .records
                .retain(|existing| existing.upgrade().is_some());
            let records: Vec<Arc<ReleaseRecord>> =
                state.records.iter().filter_map(Weak::upgrade).collect();
            (error, records)
        };
        for record in records {
            record.complete(Err(error.clone()));
        }
    }

    fn record_ownership_loss(&self) {
        let mut state = self.state.lock().expect("release registry lock poisoned");
        if matches!(
            state.cleanup_projection.as_ref(),
            Some(ReleaseCleanupProjection::Proven(_))
        ) {
            return;
        }
        state.cleanup_projection = Some(ReleaseCleanupProjection::OwnershipLost);
        state.stream_settled = false;
        state
            .records
            .retain(|existing| existing.upgrade().is_some());
        let retained: Vec<_> = state.records.iter().filter_map(Weak::upgrade).collect();
        state.retained_ownership_loss_records.extend(retained);
    }

    fn request_close_interrupts(&self) -> bool {
        let records: Vec<_> = {
            let mut state = self.state.lock().expect("release registry lock poisoned");
            state.close_requested = true;
            state
                .records
                .retain(|existing| existing.upgrade().is_some());
            state.records.iter().filter_map(Weak::upgrade).collect()
        };
        let mut close_consumed_release = false;
        for record in records {
            close_consumed_release |= record.request_close_interrupt();
        }
        close_consumed_release
    }

    fn settle_stream_after_close(&self) {
        let records: Vec<_> = {
            let mut state = self.state.lock().expect("release registry lock poisoned");
            state.close_requested = true;
            if matches!(
                state.cleanup_projection.as_ref(),
                Some(ReleaseCleanupProjection::OwnershipLost)
            ) {
                return;
            }
            state.stream_settled = true;
            state
                .records
                .retain(|existing| existing.upgrade().is_some());
            state.records.iter().filter_map(Weak::upgrade).collect()
        };
        for record in records {
            record.complete(Ok(
                AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(
                    closed_lane_error(),
                ),
            ));
        }
    }
}

struct LeaseGeneration {
    id: u64,
    sender: Sender<LaneCommand>,
    release_interrupts: Arc<ReleaseInterruptRegistry>,
    permanent_failure: Arc<ControllerCleanupFailure>,
    gate: Mutex<LeaseGenerationGate>,
    cleanup_failure: Mutex<Option<EasyConError>>,
}

#[derive(Clone)]
struct LeaseGenerationAction {
    operation: Operation,
    settlement: Arc<OperationSettlementRecord>,
}

enum LeaseGenerationGate {
    Open {
        actions: Vec<LeaseGenerationAction>,
        reservations: usize,
    },
    Sealed {
        record: Arc<ReleaseRecord>,
        actions: Vec<LeaseGenerationAction>,
        reservations: usize,
    },
}

impl LeaseGeneration {
    fn admit(
        &self,
        submit: impl FnOnce() -> Result<LeaseGenerationAction, EasyConError>,
    ) -> Result<Operation, EasyConError> {
        if let Some(error) = self.permanent_failure.get() {
            return Err(error);
        }
        {
            let mut gate = self
                .gate
                .lock()
                .expect("lease generation gate lock poisoned");
            let LeaseGenerationGate::Open { reservations, .. } = &mut *gate else {
                return Err(invalid_automation_lease_error());
            };
            *reservations = reservations
                .checked_add(1)
                .expect("Automation lease admission reservations exhausted");
        }

        // Runtime construction, Clock sampling, channel sends, and cancellation registration all
        // occur after the generation gate is released. A concurrent release can seal the
        // generation, but its cleanup waits for this reservation to resolve below.
        let submitted = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(submit)) {
            Ok(result) => result,
            Err(payload) => {
                std::mem::forget(payload);
                Err(automation_admission_panic_error())
            }
        };
        let mut rejected_after_seal = None;
        let (result, wake_cleanup) = {
            let mut gate = self
                .gate
                .lock()
                .expect("lease generation gate lock poisoned");
            match &mut *gate {
                LeaseGenerationGate::Open {
                    actions,
                    reservations,
                } => {
                    *reservations = reservations
                        .checked_sub(1)
                        .expect("Automation lease admission reservation underflow");
                    match submitted {
                        Ok(action) => {
                            let operation = action.operation.clone();
                            actions.push(action);
                            (Ok(operation), false)
                        }
                        Err(error) => (Err(error), false),
                    }
                }
                LeaseGenerationGate::Sealed {
                    actions,
                    reservations,
                    ..
                } => {
                    *reservations = reservations
                        .checked_sub(1)
                        .expect("Automation lease admission reservation underflow");
                    if let Ok(action) = submitted {
                        rejected_after_seal = Some(action.clone());
                        actions.push(action);
                    }
                    (Err(invalid_automation_lease_error()), true)
                }
            }
        };
        if let Some(action) = rejected_after_seal {
            action.settlement.settle_cancellation(&action.operation);
        }
        if wake_cleanup {
            let _ = self.sender.send(LaneCommand::Wake);
        }
        result
    }

    fn is_sealed(&self) -> bool {
        matches!(
            *self
                .gate
                .lock()
                .expect("lease generation gate lock poisoned"),
            LeaseGenerationGate::Sealed { .. }
        )
    }

    fn actions_settled(&self) -> bool {
        let gate = self
            .gate
            .lock()
            .expect("lease generation gate lock poisoned");
        let (actions, reservations) = match &*gate {
            LeaseGenerationGate::Open {
                actions,
                reservations,
            }
            | LeaseGenerationGate::Sealed {
                actions,
                reservations,
                ..
            } => (actions, *reservations),
        };
        reservations == 0
            && actions
                .iter()
                .all(|action| action.operation.snapshot().state.is_terminal())
    }

    fn record_cleanup_failure(&self, error: EasyConError) {
        let mut failure = self
            .cleanup_failure
            .lock()
            .expect("lease generation cleanup failure lock poisoned");
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    fn cleanup_failure(&self) -> Option<EasyConError> {
        self.cleanup_failure
            .lock()
            .expect("lease generation cleanup failure lock poisoned")
            .clone()
    }

    fn request_cleanup(self: &Arc<Self>) -> Arc<ReleaseRecord> {
        let (record, actions, created) = {
            let mut gate = self
                .gate
                .lock()
                .expect("lease generation gate lock poisoned");
            match &*gate {
                LeaseGenerationGate::Sealed { record, .. } => (record.clone(), Vec::new(), false),
                LeaseGenerationGate::Open { .. } => {
                    let (actions, reservations) = match std::mem::replace(
                        &mut *gate,
                        LeaseGenerationGate::Open {
                            actions: Vec::new(),
                            reservations: 0,
                        },
                    ) {
                        LeaseGenerationGate::Open {
                            actions,
                            reservations,
                        } => (actions, reservations),
                        LeaseGenerationGate::Sealed { .. } => unreachable!("lease gate was open"),
                    };
                    let record = Arc::new(ReleaseRecord::new());
                    *gate = LeaseGenerationGate::Sealed {
                        record: record.clone(),
                        actions: actions.clone(),
                        reservations,
                    };
                    (record, actions, true)
                }
            }
        };
        if !created {
            return record;
        }
        let cleanup_failure = self
            .permanent_failure
            .get()
            .or_else(|| self.cleanup_failure());
        for action in actions {
            if action.operation.snapshot().state.is_terminal() {
                continue;
            }
            if let Some(error) = cleanup_failure.as_ref() {
                action.settlement.settle_strict_failure(error.clone());
            } else {
                let _ = action
                    .operation
                    .request_cancel(CancellationReason::ParentClose);
                action.settlement.settle_cancellation(&action.operation);
            }
        }
        self.release_interrupts.track(&record);
        if let Some(error) = cleanup_failure {
            record.complete(Err(error));
            return record;
        }
        if self
            .sender
            .send(LaneCommand::BeginAutomationLeaseRelease {
                generation: self.clone(),
                completed: record.clone(),
            })
            .is_err()
        {
            record.complete(Err(EasyConError::new(
                ErrorDomain::Internal,
                ErrorCode::Internal,
                "controller lane ended before Automation lease cleanup was registered",
            )));
        }
        record
    }
}

struct ControllerInner {
    runtime: Runtime,
    resource_id: OnceLock<ResourceId>,
    identity: Arc<()>,
    sender: Sender<LaneCommand>,
    snapshot: Arc<Mutex<ControllerSnapshot>>,
    worker: Mutex<WorkerState>,
    worker_ready: Condvar,
    close_gate: Mutex<()>,
    admission_gate: Mutex<()>,
    closing: Arc<AtomicBool>,
    pending_acquires: Mutex<Vec<Weak<AcquireRecord>>>,
    release_interrupts: Arc<ReleaseInterruptRegistry>,
    operation_settlements: Arc<Mutex<Vec<Arc<OperationSettlementRecord>>>>,
    permanent_failure: Arc<ControllerCleanupFailure>,
    registration: Mutex<Option<ResourceRegistration>>,
    resource_cancellation: easycon_runtime::CancellationToken,
    amiibo_limits: Option<AmiiboLimits>,
}

enum WorkerState {
    Starting,
    Running(SupervisedTask),
    Failed,
    Joined,
}

enum LaneCommand {
    Connect {
        operation: Operation,
        options: ConnectOptions,
    },
    Direct {
        operation: Operation,
        settlement_owner: Option<OperationSettlementOwner>,
        operation_settlement: Option<Arc<OperationSettlementRecord>>,
        action: ControllerAction,
        lease_access: LeaseAccess,
        command_admitted_ns: u64,
    },
    Sequence {
        operation: Operation,
        sequence: PreciseSequence,
        operation_settlement: Arc<OperationSettlementRecord>,
    },
    Ack {
        operation: Operation,
        command: Arc<[u8]>,
        expected_reply: u8,
        protocol_timeout_ns: u64,
    },
    AmiiboSave {
        operation: Operation,
        slot: u8,
        data: Arc<[u8]>,
        options: AmiiboSaveOptions,
    },
    AmiiboSelect {
        operation: Operation,
        slot: u8,
        options: AmiiboSelectOptions,
    },
    AcquireAutomationLease {
        record: Arc<AcquireRecord>,
    },
    BeginAutomationLeaseRelease {
        generation: Arc<LeaseGeneration>,
        completed: Arc<ReleaseRecord>,
    },
    Wake,
    Close {
        completed: SyncSender<()>,
    },
}

impl LaneCommand {
    fn operation(&self) -> Option<&Operation> {
        match self {
            Self::Connect { operation, .. }
            | Self::Direct { operation, .. }
            | Self::Sequence { operation, .. }
            | Self::Ack { operation, .. }
            | Self::AmiiboSave { operation, .. }
            | Self::AmiiboSelect { operation, .. } => Some(operation),
            Self::AcquireAutomationLease { .. }
            | Self::BeginAutomationLeaseRelease { .. }
            | Self::Wake
            | Self::Close { .. } => None,
        }
    }

    fn into_operation(self) -> Option<Operation> {
        match self {
            Self::Connect { operation, .. }
            | Self::Direct { operation, .. }
            | Self::Sequence { operation, .. }
            | Self::Ack { operation, .. }
            | Self::AmiiboSave { operation, .. }
            | Self::AmiiboSelect { operation, .. } => Some(operation),
            Self::AcquireAutomationLease { .. }
            | Self::BeginAutomationLeaseRelease { .. }
            | Self::Wake
            | Self::Close { .. } => None,
        }
    }
}

#[derive(Clone)]
enum LeaseAccess {
    Direct,
    Automation(Arc<LeaseGeneration>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeaseOwner {
    Sequence(OperationId),
    Automation(u64),
}

struct WaitingSequence {
    operation: Operation,
    sequence: PreciseSequence,
    operation_settlement: Arc<OperationSettlementRecord>,
    origin_ns: u64,
}

enum ReportCompletion {
    Direct,
    Sequence {
        final_report: bool,
    },
    Cancelled {
        release_sequence: bool,
    },
    Failed {
        error: EasyConError,
        release_sequence: bool,
    },
}

struct ScheduledReport {
    target_ns: u64,
    due_ns: u64,
    deadline_id: DeadlineId,
    operation: Operation,
    settlement_owner: Option<OperationSettlementOwner>,
    operation_settlement: Option<Arc<OperationSettlementRecord>>,
    automation_generation: Option<Arc<LeaseGeneration>>,
    report: SwitchReport,
    mutation: Option<ControllerAction>,
    direct_timing: Option<DirectWriteTiming>,
    kind: WriteKind,
    completion: ReportCompletion,
}

struct PayloadWrite<'a> {
    bytes: &'a [u8],
    cancellation_override: Option<CancellationToken>,
    settlement: &'a WriteSettlement,
}

struct AutomationCleanup {
    generation: Arc<LeaseGeneration>,
    completed: Arc<ReleaseRecord>,
    target_ns: u64,
    deadline_id: DeadlineId,
}

struct ControllerLane {
    runtime: Runtime,
    resource_id: ResourceId,
    controller_identity: Arc<()>,
    clock: Arc<dyn Clock>,
    options: ControllerOptions,
    transport: Box<dyn ControllerTransport>,
    sender: Sender<LaneCommand>,
    receiver: Receiver<LaneCommand>,
    snapshot: Arc<Mutex<ControllerSnapshot>>,
    desired_report: SwitchReport,
    pending_reports: VecDeque<ScheduledReport>,
    deferred_commands: VecDeque<LaneCommand>,
    close_deferred_operations: Vec<Operation>,
    last_report_acceptance_ns: Option<u64>,
    next_write_sequence: u64,
    resource_cancellation: easycon_runtime::CancellationToken,
    _clock_hook: ClockChangeRegistration,
    lease_owner: Option<LeaseOwner>,
    automation_cleanup: Option<AutomationCleanup>,
    close_consumed_release: bool,
    release_interrupts: Arc<ReleaseInterruptRegistry>,
    operation_settlements: Arc<Mutex<Vec<Arc<OperationSettlementRecord>>>>,
    permanent_failure: Arc<ControllerCleanupFailure>,
    next_lease_generation: u64,
    waiting_sequence: Option<WaitingSequence>,
    next_ack_generation: u64,
}

impl ControllerSession {
    /// Creates and registers a controller without opening transport or hardware.
    pub fn new(
        runtime: &Runtime,
        transport: Box<dyn ControllerTransport>,
        options: ControllerOptions,
    ) -> Result<Self, EasyConError> {
        if options.minimum_report_interval_ns == 0 || options.write_timeout_ns == 0 {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "minimum report interval and write timeout must be non-zero",
            ));
        }
        let (sender, receiver) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(ControllerSnapshot::default()));
        let resource_cancellation = runtime.child_cancellation_token();
        let supervised_runtime = runtime.clone_for_supervision();
        let closing = Arc::new(AtomicBool::new(false));
        let identity = Arc::new(());
        let release_interrupts = Arc::new(ReleaseInterruptRegistry::default());
        let operation_settlements = Arc::new(Mutex::new(Vec::new()));
        let permanent_failure = Arc::new(ControllerCleanupFailure::default());
        let inner = Arc::new(ControllerInner {
            runtime: supervised_runtime.clone(),
            resource_id: OnceLock::new(),
            identity: identity.clone(),
            sender: sender.clone(),
            snapshot: snapshot.clone(),
            worker: Mutex::new(WorkerState::Starting),
            worker_ready: Condvar::new(),
            close_gate: Mutex::new(()),
            admission_gate: Mutex::new(()),
            closing: closing.clone(),
            pending_acquires: Mutex::new(Vec::new()),
            release_interrupts: release_interrupts.clone(),
            operation_settlements: operation_settlements.clone(),
            permanent_failure: permanent_failure.clone(),
            registration: Mutex::new(None),
            resource_cancellation: resource_cancellation.clone(),
            amiibo_limits: options.amiibo_limits,
        });
        let managed: Arc<dyn ManagedResource> = inner.clone();
        let registration = match runtime.register_resource(managed) {
            Ok(registration) => registration,
            Err(error) => {
                *inner
                    .worker
                    .lock()
                    .expect("controller worker lock poisoned") = WorkerState::Failed;
                inner.worker_ready.notify_all();
                return Err(error);
            }
        };
        let resource_id = registration.id();
        inner
            .resource_id
            .set(resource_id)
            .expect("resource ID is assigned exactly once");
        *inner
            .registration
            .lock()
            .expect("controller registration lock poisoned") = Some(registration);
        let clock = runtime.clock();
        let wake_sender = sender.clone();
        let clock_hook = clock.on_change(Arc::new(move || {
            let _ = wake_sender.send(LaneCommand::Wake);
        }));
        let lane = ControllerLane {
            runtime: supervised_runtime,
            resource_id,
            controller_identity: identity,
            clock,
            options,
            transport,
            sender: sender.clone(),
            receiver,
            snapshot,
            desired_report: SwitchReport::NEUTRAL,
            pending_reports: VecDeque::new(),
            deferred_commands: VecDeque::new(),
            close_deferred_operations: Vec::new(),
            last_report_acceptance_ns: None,
            next_write_sequence: 1,
            resource_cancellation,
            _clock_hook: clock_hook,
            lease_owner: None,
            automation_cleanup: None,
            close_consumed_release: false,
            release_interrupts,
            operation_settlements,
            permanent_failure,
            next_lease_generation: 1,
            waiting_sequence: None,
            next_ack_generation: 1,
        };
        let worker = match runtime.spawn_supervised(
            format!("easycon-controller-{}", resource_id.get()),
            move || lane.run(),
        ) {
            Ok(worker) => worker,
            Err(error) => {
                let mut state = inner
                    .worker
                    .lock()
                    .expect("controller worker lock poisoned");
                *state = WorkerState::Failed;
                drop(state);
                inner.worker_ready.notify_all();
                return Err(error);
            }
        };
        let mut state = inner
            .worker
            .lock()
            .expect("controller worker lock poisoned");
        *state = WorkerState::Running(worker);
        drop(state);
        inner.worker_ready.notify_all();
        Ok(Self { inner })
    }

    /// Returns this controller's Runtime-local resource identifier.
    #[must_use]
    pub fn id(&self) -> ResourceId {
        *self
            .inner
            .resource_id
            .get()
            .expect("controller resource ID assigned before publication")
    }

    /// Returns an authoritative query snapshot.
    #[must_use]
    pub fn snapshot(&self) -> ControllerSnapshot {
        *self
            .inner
            .snapshot
            .lock()
            .expect("controller snapshot lock poisoned")
    }

    /// Submits source-compatible automatic-baud connect work.
    pub fn connect(&self, options: ConnectOptions) -> Result<Operation, EasyConError> {
        if options.protocol_timeout_ns == 0 {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "protocol timeout must be non-zero",
            ));
        }
        self.enqueue_operation(
            options.operation_deadline_ns,
            |operation, _admitted_at_ns| LaneCommand::Connect { operation, options },
        )
    }

    /// Submits one direct desired-state mutation.
    pub fn direct(&self, action: ControllerAction) -> Result<Operation, EasyConError> {
        self.submit_direct(action, LeaseAccess::Direct)
    }

    /// Submits an action authorized by a matching Automation primitive lease.
    pub fn direct_with_lease(
        &self,
        lease: &AutomationLease,
        action: ControllerAction,
    ) -> Result<Operation, EasyConError> {
        if let Some(error) = self.inner.permanent_failure.get() {
            return Err(error);
        }
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed_lane_error());
        }
        if lease.controller_id != self.id()
            || !Arc::ptr_eq(&lease.controller_identity, &self.inner.identity)
        {
            return Err(invalid_automation_lease_error());
        }
        self.submit_direct(action, LeaseAccess::Automation(lease.generation.clone()))
    }

    /// Submits a validated precise sequence with an exclusive write lease.
    pub fn precise_sequence(&self, sequence: PreciseSequence) -> Result<Operation, EasyConError> {
        self.enqueue_precise_sequence(sequence)
    }

    /// Submits a serialized command whose success requires a generation-matched ACK.
    pub fn command_with_ack(
        &self,
        command: impl Into<Arc<[u8]>>,
        expected_reply: u8,
        protocol_timeout_ns: u64,
    ) -> Result<Operation, EasyConError> {
        let command = command.into();
        if command.is_empty() || protocol_timeout_ns == 0 {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "ACK command and protocol timeout must be non-empty",
            ));
        }
        self.enqueue_operation(None, |operation, _admitted_at_ns| LaneCommand::Ack {
            operation,
            command,
            expected_reply,
            protocol_timeout_ns,
        })
    }

    /// Returns the explicit Amiibo limits configured for this candidate session.
    #[must_use]
    pub fn amiibo_limits(&self) -> Option<AmiiboLimits> {
        self.inner.amiibo_limits
    }

    /// Saves Amiibo bytes in source-exact 20-byte chunks under explicit device limits.
    pub fn save_amiibo(
        &self,
        slot: u8,
        data: impl Into<Arc<[u8]>>,
        options: AmiiboSaveOptions,
    ) -> Result<Operation, EasyConError> {
        if options.ack_timeout_ns == 0
            || options.reset_timeout_ns == 0
            || options.maximum_chunk_retries > MAX_AMIIBO_CHUNK_RETRIES
        {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "Amiibo timeouts must be non-zero and chunk retries must not exceed 8",
            ));
        }
        let limits = self.require_amiibo_limits()?;
        limits.validate_slot(slot)?;
        let data = data.into();
        limits.validate_data(data.len())?;
        self.enqueue_operation(
            options.operation_deadline_ns,
            |operation, _admitted_at_ns| LaneCommand::AmiiboSave {
                operation,
                slot,
                data,
                options,
            },
        )
    }

    /// Selects one zero-based Amiibo slot under explicit device limits.
    pub fn select_amiibo(
        &self,
        slot: u8,
        options: AmiiboSelectOptions,
    ) -> Result<Operation, EasyConError> {
        if options.ack_timeout_ns == 0 {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "Amiibo selection ACK timeout must be non-zero",
            ));
        }
        self.require_amiibo_limits()?.validate_slot(slot)?;
        self.enqueue_operation(
            options.operation_deadline_ns,
            |operation, _admitted_at_ns| LaneCommand::AmiiboSelect {
                operation,
                slot,
                options,
            },
        )
    }

    fn require_amiibo_limits(&self) -> Result<AmiiboLimits, EasyConError> {
        self.inner.amiibo_limits.ok_or_else(|| {
            EasyConError::new(
                ErrorDomain::Controller,
                ErrorCode::InvalidArgument,
                "Amiibo capability is Hardware Unverified; explicit limits are required",
            )
        })
    }

    /// Registers one cancellable, Runtime-clock-deadlined Automation lease acquisition.
    pub fn acquire_automation_lease(
        &self,
        cancellation: &CancellationToken,
        deadline_ns: Option<u64>,
    ) -> AutomationLeaseAcquire {
        let (deadline_registration, registration_failure) = match deadline_ns {
            Some(deadline) => match self.inner.runtime.register_deadline(deadline) {
                Ok(registration) => (Some(registration), None),
                Err(error) => (None, Some(error)),
            },
            None => (None, None),
        };
        let record = Arc::new(AcquireRecord::new(
            cancellation.clone(),
            deadline_ns,
            self.inner.runtime.clock(),
            self.inner.closing.clone(),
            deadline_registration,
        ));
        let weak_record = Arc::downgrade(&record);
        let cancellation_registration = cancellation.on_cancel_scoped(move || {
            if let Some(record) = weak_record.upgrade() {
                let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Cancelled);
            }
        });
        if let Some(error) = registration_failure {
            let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Failure(error));
        }
        record.resolve_visible_signal();
        let admission_outcome = {
            let _admission = self
                .inner
                .admission_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut pending = self
                .inner
                .pending_acquires
                .lock()
                .expect("controller pending acquire lock poisoned");
            pending.retain(|existing| existing.upgrade().is_some());
            pending.push(Arc::downgrade(&record));
            drop(pending);
            if let Some(error) = self.inner.permanent_failure.get() {
                Some(AutomationLeaseAcquireOutcome::Failure(error))
            } else if self.inner.closing.load(Ordering::Acquire)
                || self
                    .inner
                    .sender
                    .send(LaneCommand::AcquireAutomationLease {
                        record: record.clone(),
                    })
                    .is_err()
            {
                Some(AutomationLeaseAcquireOutcome::Closed)
            } else {
                None
            }
        };
        if let Some(outcome) = admission_outcome {
            let _ = record.resolve_with(|| outcome);
        }
        AutomationLeaseAcquire {
            record: Some(record),
            _cancellation_registration: Some(cancellation_registration),
        }
    }

    fn submit_direct(
        &self,
        action: ControllerAction,
        lease_access: LeaseAccess,
    ) -> Result<Operation, EasyConError> {
        match lease_access {
            LeaseAccess::Direct => self.enqueue_direct(action),
            LeaseAccess::Automation(generation) => {
                let command_generation = generation.clone();
                generation
                    .admit(|| self.enqueue_automation_direct(action, command_generation.clone()))
            }
        }
    }

    fn enqueue_automation_direct(
        &self,
        action: ControllerAction,
        generation: Arc<LeaseGeneration>,
    ) -> Result<LeaseGenerationAction, EasyConError> {
        let (operation, settlement) =
            self.enqueue_owned_direct(action, LeaseAccess::Automation(generation))?;
        Ok(LeaseGenerationAction {
            operation,
            settlement,
        })
    }

    fn enqueue_direct(&self, action: ControllerAction) -> Result<Operation, EasyConError> {
        self.enqueue_owned_direct(action, LeaseAccess::Direct)
            .map(|(operation, _)| operation)
    }

    fn enqueue_owned_direct(
        &self,
        action: ControllerAction,
        lease_access: LeaseAccess,
    ) -> Result<(Operation, Arc<OperationSettlementRecord>), EasyConError> {
        self.ensure_admission_open()?;
        let (operation, owner) = self.inner.runtime.create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Transferable,
            || Ok(()),
            |_| {},
        )?;
        if let Err(error) = self.inner.bind_settlement_owner_to_lane(&owner) {
            let _ = operation.request_cancel(CancellationReason::ParentClose);
            let _ = owner.settle(
                SettlementEvidence::NotDelivered,
                TerminalCandidate::Cancellation,
            );
            return Err(error);
        }
        let settlement = Arc::new(OperationSettlementRecord::new(owner));
        self.inner.track_operation_settlement(&settlement);
        self.attach_wake(&operation);
        let command_admitted_ns = match controller_clock_now(&self.inner.runtime.clock()) {
            Ok(timestamp_ns) => timestamp_ns,
            Err(error) => {
                settlement.settle_strict_failure(error.clone());
                return Err(error);
            }
        };
        let admission = {
            let _admission = self
                .inner
                .admission_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(error) = self.inner.permanent_failure.get() {
                Err((error, true))
            } else if self.inner.closing.load(Ordering::Acquire) {
                Err((closed_lane_error(), false))
            } else {
                Ok(self
                    .inner
                    .sender
                    .send(LaneCommand::Direct {
                        operation: operation.clone(),
                        settlement_owner: None,
                        operation_settlement: Some(settlement.clone()),
                        action,
                        lease_access,
                        command_admitted_ns,
                    })
                    .is_ok())
            }
        };
        match admission {
            Ok(true) => {}
            Ok(false) => settlement.settle_cancellation(&operation),
            Err((error, true)) => {
                settlement.settle_strict_failure(error.clone());
                return Err(error);
            }
            Err((error, false)) => {
                settlement.settle_cancellation(&operation);
                return Err(error);
            }
        }
        Ok((operation, settlement))
    }

    fn ensure_admission_open(&self) -> Result<(), EasyConError> {
        let _admission = self
            .inner
            .admission_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = self.inner.permanent_failure.get() {
            return Err(error);
        }
        if self.inner.closing.load(Ordering::Acquire) {
            return Err(closed_lane_error());
        }
        Ok(())
    }

    fn enqueue_precise_sequence(
        &self,
        sequence: PreciseSequence,
    ) -> Result<Operation, EasyConError> {
        self.ensure_admission_open()?;
        let (operation, owner) = self.inner.runtime.create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Transferable,
            || Ok(()),
            |_| {},
        )?;
        if let Err(error) = self.inner.bind_settlement_owner_to_lane(&owner) {
            let _ = operation.request_cancel(CancellationReason::ParentClose);
            let _ = owner.settle(
                SettlementEvidence::NotDelivered,
                TerminalCandidate::Cancellation,
            );
            return Err(error);
        }
        self.attach_wake(&operation);
        let settlement = Arc::new(OperationSettlementRecord::new(owner));
        self.inner.track_operation_settlement(&settlement);
        let admission = {
            let _admission = self
                .inner
                .admission_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(error) = self.inner.permanent_failure.get() {
                Err((error, true))
            } else if self.inner.closing.load(Ordering::Acquire) {
                Err((closed_lane_error(), false))
            } else {
                Ok(self
                    .inner
                    .sender
                    .send(LaneCommand::Sequence {
                        operation: operation.clone(),
                        sequence,
                        operation_settlement: settlement.clone(),
                    })
                    .is_ok())
            }
        };
        match admission {
            Ok(true) => {}
            Ok(false) => settlement.settle_cancellation(&operation),
            Err((error, true)) => {
                settlement.settle_strict_failure(error.clone());
                return Err(error);
            }
            Err((error, false)) => {
                settlement.settle_cancellation(&operation);
                return Err(error);
            }
        }
        Ok(operation)
    }

    fn enqueue_operation(
        &self,
        deadline_ns: Option<u64>,
        command: impl FnOnce(Operation, u64) -> LaneCommand,
    ) -> Result<Operation, EasyConError> {
        self.ensure_admission_open()?;
        let operation = self
            .inner
            .runtime
            .create_operation_with_parent(deadline_ns, &self.inner.resource_cancellation)?;
        self.attach_wake(&operation);
        let command_admitted_ns = match controller_clock_now(&self.inner.runtime.clock()) {
            Ok(timestamp_ns) => timestamp_ns,
            Err(error) => {
                fail_or_finish_cancelled(&operation, error.clone());
                return Err(error);
            }
        };
        let admission = {
            let _admission = self
                .inner
                .admission_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(error) = self.inner.permanent_failure.get() {
                Err((error, true))
            } else if self.inner.closing.load(Ordering::Acquire) {
                Err((closed_lane_error(), false))
            } else {
                Ok(self
                    .inner
                    .sender
                    .send(command(operation.clone(), command_admitted_ns))
                    .is_ok())
            }
        };
        match admission {
            Ok(true) => {}
            Ok(false) => fail_closed_lane(&operation),
            Err((error, true)) => {
                fail_strict_cleanup(&operation, error.clone());
                return Err(error);
            }
            Err((error, false)) => {
                fail_closed_lane(&operation);
                return Err(error);
            }
        }
        Ok(operation)
    }

    /// Submits a neutral reset through the same writer lane.
    pub fn reset(&self) -> Result<Operation, EasyConError> {
        self.direct(ControllerAction::Reset)
    }

    /// Idempotently neutralizes, closes transport, and joins the writer.
    pub fn close(&self) {
        let _ = self.inner.close_internal(false);
    }

    fn attach_wake(&self, operation: &Operation) {
        let sender = self.inner.sender.clone();
        operation.on_cancel(move || {
            let _ = sender.send(LaneCommand::Wake);
        });
    }
}

impl ControllerInner {
    fn bind_settlement_owner_to_lane(
        &self,
        owner: &OperationSettlementOwner,
    ) -> Result<(), EasyConError> {
        let worker = {
            let state = self.worker.lock().expect("controller worker lock poisoned");
            match &*state {
                WorkerState::Running(worker) => worker.clone(),
                WorkerState::Starting | WorkerState::Failed | WorkerState::Joined => {
                    return Err(closed_lane_error());
                }
            }
        };
        self.runtime
            .bind_operation_settlement_owner_to_task(owner, &worker)
    }

    fn track_operation_settlement(&self, record: &Arc<OperationSettlementRecord>) {
        let mut records = self
            .operation_settlements
            .lock()
            .expect("controller operation settlement registry lock poisoned");
        records.retain(|existing| !existing.is_finished());
        records.push(record.clone());
    }

    fn finish_claimed_settlements_after_lane_join(&self) {
        let records = self
            .operation_settlements
            .lock()
            .expect("controller operation settlement registry lock poisoned")
            .clone();
        for record in records {
            let _ = record.finish_claimed_after_lane_join();
        }
        self.operation_settlements
            .lock()
            .expect("controller operation settlement registry lock poisoned")
            .retain(|record| !record.is_finished());
    }

    fn close_internal(&self, report_runtime_failure: bool) -> Result<(), EasyConError> {
        let _close = self
            .close_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut worker_state = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while matches!(*worker_state, WorkerState::Starting) {
            worker_state = self
                .worker_ready
                .wait(worker_state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let running = matches!(*worker_state, WorkerState::Running(_));
        drop(worker_state);
        let close_channel = if running {
            let (completed, receiver) = mpsc::sync_channel(0);
            Some((completed, receiver))
        } else {
            None
        };
        {
            let _admission = self
                .admission_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.closing.store(true, Ordering::Release);
            self.mark_close_intent();
        }
        // Closing is now sealed. Every action below may invoke Runtime, Clock, or registered
        // cancellation callbacks, so it must remain outside the admission gate.
        self.settle_pending_acquires();
        let _ = self.release_interrupts.request_close_interrupts();
        self.resource_cancellation.cancel();
        let completion = close_channel.and_then(|(completed, receiver)| {
            self.sender
                .send(LaneCommand::Close { completed })
                .ok()
                .map(|()| receiver)
        });
        if let Some(receiver) = completion {
            let _ = receiver.recv();
        }
        let worker = {
            let mut worker_state = self
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match std::mem::replace(&mut *worker_state, WorkerState::Joined) {
                WorkerState::Running(worker) => Some(worker),
                WorkerState::Starting => unreachable!("worker construction barrier was crossed"),
                WorkerState::Failed | WorkerState::Joined => None,
            }
        };
        if let Some(worker) = worker {
            let worker_completed = worker.join() == Ok(SupervisedTaskOutcome::Completed);
            if !worker_completed {
                self.finish_claimed_settlements_after_lane_join();
                let _ = self.record_ownership_loss();
            }
        }
        let cleanup_failure = self.permanent_failure.get();
        if cleanup_failure.is_none()
            || (report_runtime_failure && !self.permanent_failure.ownership_lost())
        {
            self.registration
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
        }
        drop(_close);
        cleanup_failure.map_or(Ok(()), Err)
    }

    fn settle_pending_acquires(&self) {
        let records: Vec<_> = self
            .pending_acquires
            .lock()
            .expect("controller pending acquire lock poisoned")
            .drain(..)
            .filter_map(|record| record.upgrade())
            .collect();
        for record in records {
            let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Closed);
        }
    }

    fn mark_close_intent(&self) {
        let mut snapshot = self
            .snapshot
            .lock()
            .expect("controller snapshot lock poisoned");
        if snapshot.state == ControllerState::Connected {
            snapshot.state = ControllerState::Disconnecting;
        }
    }

    fn record_ownership_loss(&self) -> EasyConError {
        let error = self
            .permanent_failure
            .record_ownership_loss(controller_ownership_lost_error());
        if self.permanent_failure.ownership_lost() {
            self.release_interrupts.record_ownership_loss();
        }
        error
    }
}

impl ManagedResource for ControllerInner {
    fn close(&self) {
        let _ = self.close_internal(false);
    }

    fn close_checked(&self) -> Result<(), EasyConError> {
        self.close_internal(true)
    }
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        let _ = self.close_internal(false);
    }
}

impl ControllerLane {
    fn run(mut self) {
        loop {
            if self.resource_cancellation.is_cancelled() {
                let completed = self.await_close_command();
                self.close_lane(completed);
                break;
            }
            self.prune_finished_operation_settlements();
            self.observe_cancellation();
            self.dispatch_due_reports();
            self.dispatch_automation_cleanup();
            self.activate_waiting_sequence();

            let command = self.next_command();
            let Some(command) = command else {
                self.close_lane(None);
                break;
            };
            if let Some(error) = self.permanent_failure.get() {
                match command {
                    LaneCommand::Close { completed } => {
                        self.close_lane(Some(completed));
                        break;
                    }
                    LaneCommand::Wake => continue,
                    command => {
                        self.fail_command_after_cleanup_failure(command, error);
                        continue;
                    }
                }
            }
            match command {
                LaneCommand::Connect { operation, options } => {
                    self.handle_connect(operation, options);
                }
                LaneCommand::Direct {
                    operation,
                    settlement_owner,
                    operation_settlement,
                    action,
                    lease_access,
                    command_admitted_ns,
                } => {
                    let direct_timing = DirectWriteTiming {
                        command_admitted_ns,
                        lane_wake_ns: self.clock.now_ns(),
                    };
                    self.handle_direct(
                        operation,
                        settlement_owner,
                        operation_settlement,
                        action,
                        lease_access,
                        direct_timing,
                    );
                }
                LaneCommand::Sequence {
                    operation,
                    sequence,
                    operation_settlement,
                } => {
                    self.handle_sequence(operation, sequence, operation_settlement);
                }
                LaneCommand::Ack {
                    operation,
                    command,
                    expected_reply,
                    protocol_timeout_ns,
                } => {
                    if self.should_defer_request_response() {
                        self.deferred_commands.push_front(LaneCommand::Ack {
                            operation,
                            command,
                            expected_reply,
                            protocol_timeout_ns,
                        });
                    } else {
                        self.handle_ack(operation, command, expected_reply, protocol_timeout_ns);
                    }
                }
                LaneCommand::AmiiboSave {
                    operation,
                    slot,
                    data,
                    options,
                } => {
                    if self.should_defer_request_response() {
                        self.deferred_commands.push_front(LaneCommand::AmiiboSave {
                            operation,
                            slot,
                            data,
                            options,
                        });
                    } else {
                        self.handle_amiibo_save(operation, slot, data, options);
                    }
                }
                LaneCommand::AmiiboSelect {
                    operation,
                    slot,
                    options,
                } => {
                    if self.should_defer_request_response() {
                        self.deferred_commands
                            .push_front(LaneCommand::AmiiboSelect {
                                operation,
                                slot,
                                options,
                            });
                    } else {
                        self.handle_amiibo_select(operation, slot, options);
                    }
                }
                LaneCommand::AcquireAutomationLease { record } => {
                    self.acquire_automation_lease(record);
                }
                LaneCommand::BeginAutomationLeaseRelease {
                    generation,
                    completed,
                } => {
                    self.begin_automation_lease_release(generation, completed);
                }
                LaneCommand::Wake => {}
                LaneCommand::Close { completed } => {
                    self.close_lane(Some(completed));
                    break;
                }
            }
        }
    }

    /// Resource close cancels the lane before it enqueues its control command. Consume that
    /// command ahead of ordinary wake-driven dispatch so a due cancellation cleanup cannot race
    /// ahead and force an unnecessary second paced final neutral.
    fn await_close_command(&mut self) -> Option<SyncSender<()>> {
        loop {
            match self.receiver.recv() {
                Ok(LaneCommand::Close { completed }) => return Some(completed),
                Ok(command) => self.deferred_commands.push_back(command),
                Err(_) => return None,
            }
        }
    }

    fn prune_finished_operation_settlements(&self) {
        self.operation_settlements
            .lock()
            .expect("controller operation settlement registry lock poisoned")
            .retain(|record| !record.is_finished());
    }

    fn next_command(&mut self) -> Option<LaneCommand> {
        loop {
            let deferred_request_blocked = self.deferred_commands.front().is_some_and(|command| {
                matches!(
                    command,
                    LaneCommand::Ack { .. }
                        | LaneCommand::AmiiboSave { .. }
                        | LaneCommand::AmiiboSelect { .. }
                )
            }) && self.should_defer_request_response();
            if !deferred_request_blocked && let Some(command) = self.deferred_commands.pop_front() {
                return Some(command);
            }

            let command = match self.next_wait_duration() {
                Some(duration) => match self.receiver.recv_timeout(duration) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => Some(LaneCommand::Wake),
                    Err(RecvTimeoutError::Disconnected) => None,
                },
                None => self.receiver.recv().ok(),
            };
            match command {
                Some(command @ LaneCommand::Close { .. }) | Some(command @ LaneCommand::Wake) => {
                    return Some(command);
                }
                Some(command) if deferred_request_blocked => {
                    self.deferred_commands.push_back(command);
                }
                command => return command,
            }
        }
    }

    fn should_defer_request_response(&self) -> bool {
        self.lease_owner.is_none()
            && self.waiting_sequence.is_none()
            && !self.pending_reports.is_empty()
    }

    fn next_wait_duration(&self) -> Option<std::time::Duration> {
        let pending = self.pending_reports.front().map(|report| report.due_ns);
        let cleanup = self.automation_cleanup.as_ref().and_then(|cleanup| {
            cleanup
                .generation
                .actions_settled()
                .then_some(cleanup.target_ns)
        });
        pending
            .into_iter()
            .chain(cleanup)
            .min()
            .and_then(|target| self.clock.real_wait_duration(target))
    }

    fn handle_connect(&mut self, operation: Operation, options: ConnectOptions) {
        if self.finish_if_cancelled_before_start(&operation) {
            return;
        }
        if !start_or_finish_cancelled(&operation) {
            return;
        }
        if self.state() != ControllerState::Disconnected {
            fail_or_finish_cancelled(
                &operation,
                EasyConError::new(
                    ErrorDomain::Controller,
                    ErrorCode::ResourceBusy,
                    "controller is not disconnected",
                ),
            );
            return;
        }
        self.set_state(
            ControllerState::Connecting,
            "controller.connecting",
            Some(operation.id()),
        );

        let mut last_error = TransportError::new(
            TransportErrorKind::Timeout,
            "controller handshake timed out",
        );
        for baud_rate in AUTO_BAUD_RATES {
            if self.cancel_or_deadline(&operation, options.operation_deadline_ns) {
                let close_failure = self.close_transport_with_cleanup_failure(None).err();
                self.set_state(
                    ControllerState::Disconnected,
                    "controller.disconnected",
                    None,
                );
                if let Some(error) = close_failure {
                    fail_strict_cleanup(&operation, error);
                } else {
                    operation.finish_cancelled();
                }
                return;
            }
            let now = self.clock.now_ns();
            let protocol_deadline = now.saturating_add(options.protocol_timeout_ns);
            let attempt_deadline = options
                .operation_deadline_ns
                .map_or(protocol_deadline, |deadline| {
                    deadline.min(protocol_deadline)
                });
            let request = HandshakeRequest {
                operation_id: operation.id(),
                baud_rate,
                request_bytes: HANDSHAKE_REQUEST,
                expected_reply: HANDSHAKE_REPLY,
                deadline_ns: attempt_deadline,
                cancellation: operation.cancellation_token(),
                resource_cancellation: self.resource_cancellation.clone(),
            };
            match self.transport.handshake(request) {
                Ok(()) if !self.cancel_or_deadline(&operation, options.operation_deadline_ns) => {
                    let operation_id = operation.id();
                    let outcome = operation.succeed_after_cleanup(OperationValue::Unit, || {
                        self.set_state(
                            ControllerState::Connected,
                            "controller.connected",
                            Some(operation_id),
                        );
                    });
                    if outcome == TransitionOutcome::Invalid
                        && operation.snapshot().state == OperationState::Cancelling
                    {
                        let close_failure = self.close_transport_with_cleanup_failure(None).err();
                        self.set_state(
                            ControllerState::Disconnected,
                            "controller.disconnected",
                            None,
                        );
                        if let Some(error) = close_failure {
                            fail_strict_cleanup(&operation, error);
                        } else {
                            operation.finish_cancelled();
                        }
                    }
                    return;
                }
                Ok(()) => {
                    let close_failure = self.close_transport_with_cleanup_failure(None).err();
                    self.set_state(
                        ControllerState::Disconnected,
                        "controller.disconnected",
                        None,
                    );
                    if let Some(error) = close_failure {
                        fail_strict_cleanup(&operation, error);
                    } else {
                        operation.finish_cancelled();
                    }
                    return;
                }
                Err(error) if error.kind() == TransportErrorKind::Cancelled => {
                    if operation.snapshot().state != OperationState::Cancelling {
                        operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
                    }
                    let close_failure = self
                        .close_transport_with_cleanup_failure(Some(&error))
                        .err();
                    self.set_state(
                        ControllerState::Disconnected,
                        "controller.disconnected",
                        None,
                    );
                    if let Some(error) = close_failure {
                        fail_strict_cleanup(&operation, error);
                    } else {
                        operation.finish_cancelled();
                    }
                    return;
                }
                Err(error) => {
                    let close_failure = self
                        .close_transport_with_cleanup_failure(Some(&error))
                        .err();
                    last_error = error;
                    if let Some(error) = close_failure {
                        self.set_state(
                            ControllerState::Disconnected,
                            "controller.disconnected",
                            None,
                        );
                        fail_strict_cleanup(&operation, error);
                        return;
                    }
                    if self.cancel_or_deadline(&operation, options.operation_deadline_ns) {
                        self.set_state(
                            ControllerState::Disconnected,
                            "controller.disconnected",
                            None,
                        );
                        operation.finish_cancelled();
                        return;
                    }
                }
            }
        }

        self.set_state(
            ControllerState::Disconnected,
            "controller.disconnected",
            None,
        );
        fail_or_finish_cancelled(&operation, map_transport_error(last_error));
    }

    fn cancel_or_deadline(&self, operation: &Operation, deadline_ns: Option<u64>) -> bool {
        if operation.snapshot().state == OperationState::Cancelling {
            return true;
        }
        if deadline_ns.is_some_and(|deadline| self.clock.now_ns() >= deadline) {
            operation.request_cancel(easycon_runtime::CancellationReason::Deadline);
            return true;
        }
        if operation.cancellation_token().is_cancelled() {
            operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
            return true;
        }
        false
    }

    fn handle_direct(
        &mut self,
        operation: Operation,
        settlement_owner: Option<OperationSettlementOwner>,
        operation_settlement: Option<Arc<OperationSettlementRecord>>,
        action: ControllerAction,
        lease_access: LeaseAccess,
        direct_timing: DirectWriteTiming,
    ) {
        let automation_generation = match &lease_access {
            LeaseAccess::Direct => None,
            LeaseAccess::Automation(generation) => Some(generation.clone()),
        };
        let sealed_automation_generation = || {
            automation_generation
                .as_ref()
                .is_some_and(|generation| generation.is_sealed())
        };
        let lease_allowed = match (self.lease_owner, &lease_access) {
            (None, LeaseAccess::Direct) => true,
            (Some(LeaseOwner::Automation(owner)), LeaseAccess::Automation(access)) => {
                owner == access.id
            }
            _ => false,
        };
        if settlement_owner.is_some() || operation_settlement.is_some() {
            if operation.snapshot().cancellation_reason.is_some() || sealed_automation_generation()
            {
                settle_report_cancellation(settlement_owner, operation_settlement, &operation);
                return;
            }
            if operation.start() != TransitionOutcome::Applied {
                settle_report_failure(
                    settlement_owner,
                    operation_settlement,
                    &operation,
                    EasyConError::new(
                        ErrorDomain::Internal,
                        ErrorCode::Internal,
                        "Automation action could not enter the Controller lane",
                    ),
                );
                return;
            }
            if operation.snapshot().cancellation_reason.is_some() || sealed_automation_generation()
            {
                settle_report_cancellation(settlement_owner, operation_settlement, &operation);
                return;
            }
            if self.state() != ControllerState::Connected {
                settle_report_failure(
                    settlement_owner,
                    operation_settlement,
                    &operation,
                    EasyConError::new(
                        ErrorDomain::Controller,
                        ErrorCode::DeviceDisconnected,
                        "controller is not connected",
                    ),
                );
                return;
            }
            if !lease_allowed {
                settle_report_failure(
                    settlement_owner,
                    operation_settlement,
                    &operation,
                    resource_busy_error("controller write lease is owned"),
                );
                return;
            }
            action.apply(&mut self.desired_report);
            self.update_desired_snapshot();
            let now = self.clock.now_ns();
            let target_ns = self.last_report_acceptance_ns.map_or(now, |last| {
                last.saturating_add(self.options.minimum_report_interval_ns)
                    .max(now)
            });
            let deadline_id = self.clock.register_deadline(target_ns);
            self.pending_reports.push_back(ScheduledReport {
                target_ns,
                due_ns: target_ns,
                deadline_id,
                operation,
                settlement_owner,
                operation_settlement,
                automation_generation,
                report: self.desired_report,
                mutation: Some(action),
                direct_timing: Some(direct_timing),
                kind: WriteKind::Report,
                completion: ReportCompletion::Direct,
            });
            return;
        }

        if self.finish_if_cancelled_before_start(&operation) {
            return;
        }
        if !start_or_finish_cancelled(&operation) {
            return;
        }
        if self.state() != ControllerState::Connected {
            fail_or_finish_cancelled(
                &operation,
                EasyConError::new(
                    ErrorDomain::Controller,
                    ErrorCode::DeviceDisconnected,
                    "controller is not connected",
                ),
            );
            return;
        }
        if !lease_allowed {
            fail_or_finish_cancelled(
                &operation,
                resource_busy_error("controller write lease is owned"),
            );
            return;
        }

        action.apply(&mut self.desired_report);
        self.update_desired_snapshot();
        let now = self.clock.now_ns();
        let target_ns = self.last_report_acceptance_ns.map_or(now, |last| {
            last.saturating_add(self.options.minimum_report_interval_ns)
                .max(now)
        });
        let deadline_id = self.clock.register_deadline(target_ns);
        self.pending_reports.push_back(ScheduledReport {
            target_ns,
            due_ns: target_ns,
            deadline_id,
            operation,
            settlement_owner: None,
            operation_settlement: None,
            automation_generation: None,
            report: self.desired_report,
            mutation: Some(action),
            direct_timing: Some(direct_timing),
            kind: WriteKind::Report,
            completion: ReportCompletion::Direct,
        });
    }

    fn handle_sequence(
        &mut self,
        operation: Operation,
        sequence: PreciseSequence,
        operation_settlement: Arc<OperationSettlementRecord>,
    ) {
        if operation.snapshot().cancellation_reason.is_some() {
            operation_settlement.settle_cancellation(&operation);
            return;
        }
        if operation.start() != TransitionOutcome::Applied {
            operation_settlement.settle_failure(
                &operation,
                EasyConError::new(
                    ErrorDomain::Internal,
                    ErrorCode::Internal,
                    "precise sequence could not enter the Controller lane",
                ),
            );
            return;
        }
        if self.state() != ControllerState::Connected {
            operation_settlement.settle_failure(
                &operation,
                EasyConError::new(
                    ErrorDomain::Controller,
                    ErrorCode::DeviceDisconnected,
                    "controller is not connected",
                ),
            );
            return;
        }
        if self.lease_owner.is_some() {
            operation_settlement.settle_failure(
                &operation,
                resource_busy_error("controller write lease is owned"),
            );
            return;
        }
        // The sequence offset is relative to lane ownership, not to a later scheduling pass.
        // Capture it before publishing the lease snapshot so a concurrent clock advance cannot
        // move every absolute target forward.
        let origin_ns = self.clock.now_ns();
        self.set_lease_owner(Some(LeaseOwner::Sequence(operation.id())));
        let waiting = WaitingSequence {
            operation,
            sequence,
            operation_settlement,
            origin_ns,
        };
        if self.pending_reports.is_empty() {
            self.schedule_sequence(waiting);
        } else {
            self.waiting_sequence = Some(waiting);
        }
    }

    fn activate_waiting_sequence(&mut self) {
        if !self.pending_reports.is_empty() {
            return;
        }
        let Some(waiting) = self.waiting_sequence.take() else {
            return;
        };
        if waiting.operation.snapshot().cancellation_reason.is_some() {
            self.schedule_cancel_cleanup_with_settlement(
                waiting.operation,
                true,
                Some(waiting.operation_settlement),
            );
        } else {
            self.schedule_sequence(waiting);
        }
    }

    fn schedule_sequence(&mut self, waiting: WaitingSequence) {
        let origin_ns = waiting.origin_ns;
        let mut planned_report = self.desired_report;
        let mut groups = waiting
            .sequence
            .steps()
            .chunk_by(|left, right| left.offset_ns == right.offset_ns);
        let group_count = groups.clone().count();
        let mut previous_due = self
            .last_report_acceptance_ns
            .map(|last| last.saturating_add(self.options.minimum_report_interval_ns));
        for (index, group) in groups.by_ref().enumerate() {
            for step in group {
                step.action.apply(&mut planned_report);
            }
            let Some(target_ns) = origin_ns.checked_add(group[0].offset_ns) else {
                self.pending_reports
                    .retain(|pending| pending.operation.id() != waiting.operation.id());
                self.schedule_failure_cleanup_with_settlement(
                    waiting.operation,
                    EasyConError::new(
                        ErrorDomain::Validation,
                        ErrorCode::InvalidArgument,
                        "sequence target overflowed monotonic time",
                    ),
                    true,
                    Some(waiting.operation_settlement),
                );
                return;
            };
            let due_ns = previous_due.map_or(target_ns, |previous| previous.max(target_ns));
            if due_ns > target_ns {
                self.publish_timing_deviation(waiting.operation.id(), target_ns, due_ns);
            }
            previous_due = Some(due_ns.saturating_add(self.options.minimum_report_interval_ns));
            let deadline_id = self.clock.register_deadline(due_ns);
            self.pending_reports.push_back(ScheduledReport {
                target_ns,
                due_ns,
                deadline_id,
                operation: waiting.operation.clone(),
                settlement_owner: None,
                operation_settlement: Some(waiting.operation_settlement.clone()),
                automation_generation: None,
                report: planned_report,
                mutation: None,
                direct_timing: None,
                kind: WriteKind::Report,
                completion: ReportCompletion::Sequence {
                    final_report: index + 1 == group_count,
                },
            });
        }
    }

    fn handle_ack(
        &mut self,
        operation: Operation,
        command: Arc<[u8]>,
        expected_reply: u8,
        protocol_timeout_ns: u64,
    ) {
        if !self.prepare_request_operation(&operation) {
            return;
        }
        let result = self.exchange_ack(
            &operation,
            &command,
            expected_reply,
            protocol_timeout_ns,
            None,
        );
        self.finish_request_exchange(operation, result);
    }

    fn handle_amiibo_select(
        &mut self,
        operation: Operation,
        slot: u8,
        options: AmiiboSelectOptions,
    ) {
        if !self.prepare_request_operation(&operation) {
            return;
        }
        let result = self.exchange_ack(
            &operation,
            &select_command(slot),
            AMIIBO_ACK,
            options.ack_timeout_ns,
            options.operation_deadline_ns,
        );
        self.finish_amiibo_exchange(operation, result, AMIIBO_DEFAULT_RESET_TIMEOUT_NS);
    }

    fn handle_amiibo_save(
        &mut self,
        operation: Operation,
        slot: u8,
        data: Arc<[u8]>,
        options: AmiiboSaveOptions,
    ) {
        if !self.prepare_request_operation(&operation) {
            return;
        }
        let mut completed_chunks = 0_usize;
        for (chunk_index, chunk) in data.chunks(AMIIBO_CHUNK_SIZE).enumerate() {
            let offset = chunk_index
                .checked_mul(AMIIBO_CHUNK_SIZE)
                .expect("validated Amiibo offset cannot overflow");
            let header = save_header(slot, offset, chunk.len());
            let mut retries = 0_u8;
            loop {
                let result = self.exchange_ack(
                    &operation,
                    &header,
                    AMIIBO_ACK,
                    options.ack_timeout_ns,
                    options.operation_deadline_ns,
                );
                let result = match result {
                    Ok(()) => self.exchange_ack(
                        &operation,
                        chunk,
                        AMIIBO_ACK,
                        options.ack_timeout_ns,
                        options.operation_deadline_ns,
                    ),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) => break,
                    Err(error)
                        if is_retryable_amiibo_error(&error)
                            && retries < options.maximum_chunk_retries =>
                    {
                        retries = retries.saturating_add(1);
                        self.publish_amiibo_event(
                            "controller.amiibo.save.retry",
                            &operation,
                            format!("slot={slot}, offset={offset}, retry={retries}, cause={error}"),
                            true,
                        );
                        if let Err(reset_error) = self.exchange_ack(
                            &operation,
                            &reset_command(),
                            AMIIBO_RESET_REPLY,
                            options.reset_timeout_ns,
                            options.operation_deadline_ns,
                        ) {
                            self.finish_amiibo_save_failure(
                                operation,
                                reset_error,
                                completed_chunks,
                                options.reset_timeout_ns,
                            );
                            return;
                        }
                    }
                    Err(error) => {
                        self.finish_amiibo_save_failure(
                            operation,
                            error,
                            completed_chunks,
                            options.reset_timeout_ns,
                        );
                        return;
                    }
                }
            }
            completed_chunks = completed_chunks
                .checked_add(1)
                .expect("validated Amiibo chunk count cannot overflow");
            self.publish_amiibo_event(
                "controller.amiibo.save.chunk_accepted",
                &operation,
                format!("slot={slot}, offset={offset}, length={}", chunk.len()),
                false,
            );
        }
        if operation.succeed(OperationValue::Unit) == TransitionOutcome::Invalid
            && operation.snapshot().state == OperationState::Cancelling
        {
            operation.finish_cancelled();
        }
    }

    fn prepare_request_operation(&mut self, operation: &Operation) -> bool {
        if self.finish_if_cancelled_before_start(operation) {
            return false;
        }
        if !start_or_finish_cancelled(operation) {
            return false;
        }
        if self.state() != ControllerState::Connected {
            fail_or_finish_cancelled(
                operation,
                EasyConError::new(
                    ErrorDomain::Controller,
                    ErrorCode::DeviceDisconnected,
                    "controller is not connected",
                ),
            );
            return false;
        }
        if self.lease_owner.is_some() || self.waiting_sequence.is_some() {
            fail_or_finish_cancelled(
                operation,
                resource_busy_error("controller write lease is owned"),
            );
            return false;
        }
        true
    }

    fn exchange_ack(
        &mut self,
        operation: &Operation,
        command: &[u8],
        expected_reply: u8,
        protocol_timeout_ns: u64,
        operation_deadline_ns: Option<u64>,
    ) -> Result<(), TransportError> {
        if self.cancel_or_deadline(operation, operation_deadline_ns) {
            return Err(cancelled_transport_error());
        }
        let generation = self.next_ack_generation;
        self.next_ack_generation = self
            .next_ack_generation
            .checked_add(1)
            .expect("ACK generation exhausted");
        let now = self.clock.now_ns();
        self.write_payload(Some(operation), WriteKind::Command, now, None, command)?;
        if self.cancel_or_deadline(operation, operation_deadline_ns) {
            return Err(cancelled_transport_error());
        }
        let protocol_deadline_ns = self.clock.now_ns().saturating_add(protocol_timeout_ns);
        let deadline_ns = operation_deadline_ns.map_or(protocol_deadline_ns, |deadline| {
            deadline.min(protocol_deadline_ns)
        });
        loop {
            let request = AckRequest {
                operation_id: operation.id(),
                generation,
                expected_reply,
                deadline_ns,
                cancellation: operation.cancellation_token(),
                resource_cancellation: self.resource_cancellation.clone(),
            };
            match self.transport.wait_for_ack(request) {
                Ok(frame) if frame.generation < generation => {
                    let _ = self.runtime.publish(
                        EventDraft::ordinary(
                            EventKind::Warning,
                            "controller.ack.late_ignored",
                            Severity::Warning,
                        )
                        .with_resource(self.resource_id)
                        .with_operation(operation.id()),
                    );
                }
                Ok(frame) if frame.generation == generation && frame.byte == expected_reply => {
                    if self.cancel_or_deadline(operation, operation_deadline_ns) {
                        return Err(cancelled_transport_error());
                    }
                    return Ok(());
                }
                Ok(_) => {
                    return Err(TransportError::new(
                        TransportErrorKind::Protocol,
                        "ACK generation or reply byte did not match",
                    ));
                }
                Err(error) => {
                    if self.cancel_or_deadline(operation, operation_deadline_ns) {
                        return Err(cancelled_transport_error());
                    }
                    return Err(error);
                }
            }
        }
    }

    fn finish_request_exchange(
        &mut self,
        operation: Operation,
        result: Result<(), TransportError>,
    ) {
        match result {
            Ok(()) => {
                if operation.succeed(OperationValue::Unit) == TransitionOutcome::Invalid
                    && operation.snapshot().state == OperationState::Cancelling
                {
                    operation.finish_cancelled();
                }
            }
            Err(error) if error.kind() == TransportErrorKind::Cancelled => {
                self.finish_cancelled_request(operation);
            }
            Err(error) => {
                if error.kind() == TransportErrorKind::Disconnected {
                    self.handle_transport_disconnect(Some(operation.id()), &error);
                }
                self.fail_request_exchange(operation, map_transport_error(error));
            }
        }
    }

    fn finish_amiibo_exchange(
        &mut self,
        operation: Operation,
        result: Result<(), TransportError>,
        reset_timeout_ns: u64,
    ) {
        if let Err(error) = &result
            && error.kind() == TransportErrorKind::Cancelled
            && let Err(cleanup_error) =
                self.exchange_amiibo_cleanup_reset(operation.id(), reset_timeout_ns)
        {
            self.handle_amiibo_cleanup_failure(operation.id(), &cleanup_error);
        }
        self.finish_request_exchange(operation, result);
    }

    fn finish_amiibo_save_failure(
        &mut self,
        operation: Operation,
        error: TransportError,
        completed_chunks: usize,
        reset_timeout_ns: u64,
    ) {
        self.publish_amiibo_event(
            if error.kind() == TransportErrorKind::Cancelled {
                "controller.amiibo.save.cancelled"
            } else {
                "controller.amiibo.save.partial_failure"
            },
            &operation,
            format!("completed_chunks={completed_chunks}, cause={error}"),
            true,
        );
        if error.kind() == TransportErrorKind::Cancelled {
            if let Err(cleanup_error) =
                self.exchange_amiibo_cleanup_reset(operation.id(), reset_timeout_ns)
            {
                self.handle_amiibo_cleanup_failure(operation.id(), &cleanup_error);
            }
            self.finish_cancelled_request(operation);
            return;
        }
        if error.kind() == TransportErrorKind::Disconnected {
            self.handle_transport_disconnect(Some(operation.id()), &error);
        } else if let Err(cleanup_error) =
            self.exchange_amiibo_cleanup_reset(operation.id(), reset_timeout_ns)
        {
            self.handle_amiibo_cleanup_failure(operation.id(), &cleanup_error);
        }
        let mapped = map_transport_error(error);
        self.fail_request_exchange(
            operation,
            EasyConError::new(
                mapped.domain(),
                mapped.code(),
                format!(
                    "Amiibo save failed after {completed_chunks} complete chunks: {}",
                    mapped.message()
                ),
            ),
        );
    }

    fn exchange_amiibo_cleanup_reset(
        &mut self,
        operation_id: OperationId,
        reset_timeout_ns: u64,
    ) -> Result<(), TransportError> {
        if self.resource_cancellation.is_cancelled() || self.state() != ControllerState::Connected {
            return Err(cancelled_transport_error());
        }
        let generation = self.next_ack_generation;
        self.next_ack_generation = self
            .next_ack_generation
            .checked_add(1)
            .expect("ACK generation exhausted");
        let now = self.clock.now_ns();
        self.write_payload(None, WriteKind::Command, now, None, &reset_command())?;
        let deadline_ns = self.clock.now_ns().saturating_add(reset_timeout_ns);
        loop {
            match self.transport.wait_for_ack(AckRequest {
                operation_id,
                generation,
                expected_reply: AMIIBO_RESET_REPLY,
                deadline_ns,
                cancellation: easycon_runtime::CancellationToken::root(),
                resource_cancellation: self.resource_cancellation.clone(),
            }) {
                Ok(frame) if frame.generation < generation => {
                    let _ = self.runtime.publish(
                        EventDraft::ordinary(
                            EventKind::Warning,
                            "controller.ack.late_ignored",
                            Severity::Warning,
                        )
                        .with_resource(self.resource_id)
                        .with_operation(operation_id),
                    );
                }
                Ok(frame) if frame.generation == generation && frame.byte == AMIIBO_RESET_REPLY => {
                    return Ok(());
                }
                Ok(_) => {
                    return Err(TransportError::new(
                        TransportErrorKind::Protocol,
                        "Amiibo cleanup reset reply did not match",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn invalidate_amiibo_stream(&mut self, operation_id: OperationId, error: &TransportError) {
        self.desired_report.reset();
        self.update_desired_snapshot();
        self.set_state(
            ControllerState::Disconnected,
            "controller.disconnected",
            None,
        );
        let _ = self.runtime.publish(
            EventDraft::critical(
                EventKind::Warning,
                "controller.amiibo.cleanup.stream_closed",
                Severity::Warning,
            )
            .with_resource(self.resource_id)
            .with_operation(operation_id)
            .with_detail(error.to_string()),
        );
        self.fail_pending_disconnected();
    }

    fn handle_amiibo_cleanup_failure(&mut self, operation_id: OperationId, error: &TransportError) {
        if self.resource_cancellation.is_cancelled() {
            let _ = self.runtime.publish(
                EventDraft::critical(
                    EventKind::Warning,
                    "controller.amiibo.cleanup.deferred_to_close",
                    Severity::Warning,
                )
                .with_resource(self.resource_id)
                .with_operation(operation_id)
                .with_detail(error.to_string()),
            );
        } else {
            self.invalidate_amiibo_stream(operation_id, error);
        }
    }

    fn publish_amiibo_event(
        &self,
        code: &'static str,
        operation: &Operation,
        detail: String,
        warning: bool,
    ) {
        let (kind, severity) = if warning {
            (EventKind::Warning, Severity::Warning)
        } else {
            (EventKind::Data, Severity::Info)
        };
        let _ = self.runtime.publish(
            EventDraft::ordinary(kind, code, severity)
                .with_resource(self.resource_id)
                .with_operation(operation.id())
                .with_detail(detail),
        );
    }

    fn handle_transport_disconnect(
        &mut self,
        operation_id: Option<OperationId>,
        error: &TransportError,
    ) {
        self.desired_report.reset();
        self.update_desired_snapshot();
        self.set_state(
            ControllerState::Disconnected,
            "controller.disconnected",
            None,
        );
        self.publish_neutralization_warning(operation_id, error);
        self.fail_pending_disconnected();
    }

    fn acquire_automation_lease(&mut self, record: Arc<AcquireRecord>) {
        record.resolve_visible_signal();
        if let Some(error) = self.permanent_failure.get() {
            let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Failure(error));
            return;
        }
        match self.state() {
            ControllerState::Disconnecting | ControllerState::Closed => {
                let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Closed);
                return;
            }
            ControllerState::Disconnected | ControllerState::Connecting => {
                let _ = record.resolve_with(|| {
                    AutomationLeaseAcquireOutcome::Failure(EasyConError::new(
                        ErrorDomain::Controller,
                        ErrorCode::DeviceDisconnected,
                        "controller is not connected",
                    ))
                });
                return;
            }
            ControllerState::Connected => {}
        }
        if self.resource_cancellation.is_cancelled() {
            let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Closed);
            return;
        }
        if self.lease_owner.is_some()
            || self.waiting_sequence.is_some()
            || !self.pending_reports.is_empty()
            || self.automation_cleanup.is_some()
        {
            let _ = record.resolve_with(|| {
                AutomationLeaseAcquireOutcome::Failure(resource_busy_error(
                    "controller write lease is owned",
                ))
            });
            return;
        }
        let lease_id = self.next_lease_generation;
        let Some(next_lease_generation) = lease_id.checked_add(1) else {
            let _ = record.resolve_with(|| {
                AutomationLeaseAcquireOutcome::Failure(EasyConError::new(
                    ErrorDomain::Internal,
                    ErrorCode::Internal,
                    "controller Automation lease generations are exhausted",
                ))
            });
            return;
        };
        if lease_id == 0 {
            let _ = record.resolve_with(|| {
                AutomationLeaseAcquireOutcome::Failure(EasyConError::new(
                    ErrorDomain::Internal,
                    ErrorCode::Internal,
                    "controller Automation lease generation must be non-zero",
                ))
            });
            return;
        }
        let generation = Arc::new(LeaseGeneration {
            id: lease_id,
            sender: self.sender.clone(),
            release_interrupts: self.release_interrupts.clone(),
            permanent_failure: self.permanent_failure.clone(),
            gate: Mutex::new(LeaseGenerationGate::Open {
                actions: Vec::new(),
                reservations: 0,
            }),
            cleanup_failure: Mutex::new(None),
        });
        let controller_id = self.resource_id;
        let controller_identity = self.controller_identity.clone();
        let _ = record.resolve_with(|| {
            self.next_lease_generation = next_lease_generation;
            self.set_lease_owner(Some(LeaseOwner::Automation(lease_id)));
            AutomationLeaseAcquireOutcome::Granted(AutomationLease {
                controller_id,
                controller_identity,
                generation,
            })
        });
    }

    fn begin_automation_lease_release(
        &mut self,
        generation: Arc<LeaseGeneration>,
        completed: Arc<ReleaseRecord>,
    ) {
        if let Some(error) = self.permanent_failure.get() {
            self.release_automation_lease(generation.id);
            completed.complete(Err(error));
            return;
        }
        self.settle_sealed_generation_actions(&generation);
        if let Some(cleanup) = &self.automation_cleanup {
            if cleanup.generation.id == generation.id {
                return;
            }
            completed.complete(Err(EasyConError::new(
                ErrorDomain::Internal,
                ErrorCode::Internal,
                "controller already owns another Automation release cleanup",
            )));
            return;
        }
        if self.lease_owner != Some(LeaseOwner::Automation(generation.id)) {
            if self.resource_cancellation.is_cancelled() {
                completed.request_close_interrupt();
                completed.complete(Ok(
                    AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(
                        closed_lane_error(),
                    ),
                ));
            } else {
                completed.complete(Err(invalid_automation_lease_error()));
            }
            return;
        }
        if let Some(error) = generation.cleanup_failure() {
            self.release_automation_lease(generation.id);
            completed.complete(Err(error));
            return;
        }
        self.automation_cleanup = Some(self.new_automation_cleanup(generation, completed));
    }

    /// The release command is the lane-side seal boundary. It must drain reports that reached
    /// the lane before the caller sealed their generation; relying on a later generic wake can
    /// leave such a report running behind a manually advanced pacing deadline.
    fn settle_sealed_generation_actions(&mut self, generation: &Arc<LeaseGeneration>) {
        let mut retained = VecDeque::with_capacity(self.pending_reports.len());
        while let Some(mut pending) = self.pending_reports.pop_front() {
            let belongs_to_generation = pending
                .automation_generation
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, generation));
            if !belongs_to_generation || pending.operation.snapshot().state.is_terminal() {
                retained.push_back(pending);
                continue;
            }
            let _ = pending
                .operation
                .request_cancel(CancellationReason::ParentClose);
            settle_report_cancellation(
                pending.settlement_owner.take(),
                pending.operation_settlement.take(),
                &pending.operation,
            );
        }
        self.pending_reports = retained;
    }

    fn fail_command_after_cleanup_failure(&mut self, command: LaneCommand, error: EasyConError) {
        match command {
            LaneCommand::Direct {
                operation,
                settlement_owner,
                operation_settlement,
                lease_access,
                ..
            } => {
                if let LeaseAccess::Automation(generation) = lease_access {
                    generation.record_cleanup_failure(error.clone());
                    self.release_automation_lease(generation.id);
                }
                settle_report_strict_failure(
                    settlement_owner,
                    operation_settlement,
                    &operation,
                    error,
                );
            }
            LaneCommand::Sequence {
                operation: _,
                operation_settlement,
                ..
            } => {
                operation_settlement.settle_strict_failure(error);
            }
            LaneCommand::Connect { operation, .. }
            | LaneCommand::Ack { operation, .. }
            | LaneCommand::AmiiboSave { operation, .. }
            | LaneCommand::AmiiboSelect { operation, .. } => {
                fail_strict_cleanup(&operation, error);
            }
            LaneCommand::AcquireAutomationLease { record } => {
                let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Failure(error));
            }
            LaneCommand::BeginAutomationLeaseRelease {
                generation,
                completed,
            } => {
                generation.record_cleanup_failure(error.clone());
                self.release_automation_lease(generation.id);
                completed.complete(Err(error));
            }
            LaneCommand::Wake | LaneCommand::Close { .. } => {}
        }
    }

    fn new_automation_cleanup(
        &self,
        generation: Arc<LeaseGeneration>,
        completed: Arc<ReleaseRecord>,
    ) -> AutomationCleanup {
        let now = self.clock.now_ns();
        let target_ns = self.last_report_acceptance_ns.map_or(now, |last| {
            last.saturating_add(self.options.minimum_report_interval_ns)
                .max(now)
        });
        AutomationCleanup {
            generation,
            completed,
            target_ns,
            deadline_id: self.clock.register_deadline(target_ns),
        }
    }

    fn dispatch_automation_cleanup(&mut self) {
        let now = self.clock.now_ns();
        let Some(cleanup) = self.automation_cleanup.as_ref() else {
            return;
        };
        if !cleanup.generation.actions_settled() || cleanup.target_ns > now {
            return;
        }
        let cleanup = self
            .automation_cleanup
            .take()
            .expect("Automation cleanup was present above");
        if let Some(error) = cleanup.generation.cleanup_failure() {
            self.release_automation_lease(cleanup.generation.id);
            let close_consumed = cleanup.completed.complete(Err(error));
            if close_consumed {
                self.close_consumed_release = true;
            }
            return;
        }
        self.desired_report.reset();
        self.update_desired_snapshot();
        let (cancellation, _) = cleanup.completed.begin_neutral();
        let settlement = release_write_settlement(cleanup.completed.clone());
        let bytes = SwitchReport::NEUTRAL.encode();
        let result = if self.state() == ControllerState::Connected {
            self.write_payload_with_cancellation(
                None,
                WriteKind::Neutralize,
                now,
                None,
                PayloadWrite {
                    bytes: &bytes,
                    cancellation_override: Some(cancellation),
                    settlement: &settlement,
                },
            )
        } else {
            Err(TransportError::new(
                TransportErrorKind::Disconnected,
                "controller disconnected before Automation release neutralization",
            ))
        };
        let close_consumed = match result {
            Ok(()) => {
                let accepted_at_ns = settlement
                    .accepted_at_ns()
                    .expect("successful release neutral retains its backend acceptance timestamp");
                self.clock.record_dispatch(cleanup.deadline_id, now);
                self.last_report_acceptance_ns = Some(accepted_at_ns);
                self.record_report_acceptance(accepted_at_ns, None, bytes);
                self.release_automation_lease(cleanup.generation.id);
                cleanup
                    .completed
                    .complete(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted))
            }
            Err(error) => {
                if self.state() == ControllerState::Connected {
                    self.set_state(
                        ControllerState::Disconnected,
                        "controller.disconnected",
                        None,
                    );
                }
                self.publish_neutralization_warning(None, &error);
                self.release_automation_lease(cleanup.generation.id);
                if error.is_cleanup_failure() {
                    cleanup.completed.complete(Err(map_transport_error(error)))
                } else {
                    cleanup.completed.complete(Ok(
                        AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(
                            map_transport_error(error),
                        ),
                    ))
                }
            }
        };
        if close_consumed {
            self.close_consumed_release = true;
        }
    }

    fn finish_if_cancelled_before_start(&self, operation: &Operation) -> bool {
        if self.resource_cancellation.is_cancelled()
            || operation.cancellation_token().is_cancelled()
        {
            let _ = operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
        }
        if operation.snapshot().state == OperationState::Cancelling {
            operation.finish_cancelled();
            true
        } else {
            false
        }
    }

    fn finish_cancelled_request(&mut self, operation: Operation) {
        if operation.snapshot().state != OperationState::Cancelling {
            operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
        }
        let Some(operation) = self.defer_request_for_resource_close(operation) else {
            return;
        };
        operation.finish_cancelled();
    }

    fn fail_request_exchange(&mut self, operation: Operation, error: EasyConError) {
        let Some(operation) = self.defer_request_for_resource_close(operation) else {
            return;
        };
        fail_or_finish_cancelled(&operation, error);
    }

    fn defer_request_for_resource_close(&mut self, operation: Operation) -> Option<Operation> {
        if !self.resource_cancellation.is_cancelled() {
            return Some(operation);
        }
        if !operation.snapshot().state.is_terminal()
            && operation.snapshot().state != OperationState::Cancelling
        {
            operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
        }
        if operation.snapshot().state == OperationState::Cancelling {
            self.close_deferred_operations.push(operation);
            None
        } else {
            Some(operation)
        }
    }

    fn release_automation_lease(&mut self, lease_id: u64) {
        if self.lease_owner == Some(LeaseOwner::Automation(lease_id)) {
            self.set_lease_owner(None);
        }
    }

    fn observe_cancellation(&mut self) {
        if self.resource_cancellation.is_cancelled() {
            for operation in self
                .pending_reports
                .iter()
                .map(|pending| &pending.operation)
                .chain(
                    self.waiting_sequence
                        .iter()
                        .map(|waiting| &waiting.operation),
                )
                .chain(
                    self.deferred_commands
                        .iter()
                        .filter_map(LaneCommand::operation),
                )
            {
                let _ = operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
            }
        }
        while let Some(index) = self.pending_reports.iter().position(|report| {
            (report.settlement_owner.is_some() || report.operation_settlement.is_some())
                && report
                    .automation_generation
                    .as_ref()
                    .is_some_and(|generation| generation.is_sealed())
                && report.operation.snapshot().cancellation_reason.is_some()
        }) {
            let mut cancelled = self
                .pending_reports
                .remove(index)
                .expect("automation cancellation index came from the same queue");
            settle_report_cancellation(
                cancelled.settlement_owner.take(),
                cancelled.operation_settlement.take(),
                &cancelled.operation,
            );
        }
        if let Some(index) = self.deferred_commands.iter().position(|command| {
            command
                .operation()
                .is_some_and(|operation| operation.snapshot().state == OperationState::Cancelling)
        }) {
            let command = self
                .deferred_commands
                .remove(index)
                .expect("deferred command index came from the same queue");
            if let Some(operation) = command.into_operation() {
                operation.finish_cancelled();
            }
        }
        if self.pending_reports.is_empty()
            && self
                .waiting_sequence
                .as_ref()
                .is_some_and(|waiting| waiting.operation.snapshot().cancellation_reason.is_some())
        {
            let waiting = self
                .waiting_sequence
                .take()
                .expect("waiting sequence checked above");
            self.schedule_cancel_cleanup_with_settlement(
                waiting.operation,
                true,
                Some(waiting.operation_settlement),
            );
            return;
        }
        let Some(index) = self.pending_reports.iter().position(|report| {
            report.kind == WriteKind::Report
                && (report.operation.snapshot().state == OperationState::Cancelling
                    || (report.operation_settlement.is_some()
                        && report.operation.snapshot().cancellation_reason.is_some()))
        }) else {
            return;
        };
        let mut cancelled = self
            .pending_reports
            .remove(index)
            .expect("index came from pending report queue");
        let operation_id = cancelled.operation.id();
        let release_sequence = self.lease_owner == Some(LeaseOwner::Sequence(operation_id));
        self.pending_reports
            .retain(|pending| pending.operation.id() != operation_id);
        self.schedule_cancel_cleanup_with_settlement(
            cancelled.operation,
            release_sequence,
            cancelled.operation_settlement.take(),
        );
    }

    fn schedule_cancel_cleanup(&mut self, operation: Operation, release_sequence: bool) {
        self.schedule_cancel_cleanup_with_settlement(operation, release_sequence, None);
    }

    fn schedule_cancel_cleanup_with_settlement(
        &mut self,
        operation: Operation,
        release_sequence: bool,
        operation_settlement: Option<Arc<OperationSettlementRecord>>,
    ) {
        self.rebuild_pending_after_neutral();
        let now = self.clock.now_ns();
        let due_ns = self.last_report_acceptance_ns.map_or(now, |last| {
            last.saturating_add(self.options.minimum_report_interval_ns)
                .max(now)
        });
        let deadline_id = self.clock.register_deadline(due_ns);
        self.pending_reports.push_front(ScheduledReport {
            target_ns: now,
            due_ns,
            deadline_id,
            operation,
            settlement_owner: None,
            operation_settlement,
            automation_generation: None,
            report: SwitchReport::NEUTRAL,
            mutation: None,
            direct_timing: None,
            kind: WriteKind::Neutralize,
            completion: ReportCompletion::Cancelled { release_sequence },
        });
    }

    fn schedule_failure_cleanup(
        &mut self,
        operation: Operation,
        error: EasyConError,
        release_sequence: bool,
    ) {
        self.schedule_failure_cleanup_with_settlement(operation, error, release_sequence, None);
    }

    fn schedule_failure_cleanup_with_settlement(
        &mut self,
        operation: Operation,
        error: EasyConError,
        release_sequence: bool,
        operation_settlement: Option<Arc<OperationSettlementRecord>>,
    ) {
        self.rebuild_pending_after_neutral();
        let now = self.clock.now_ns();
        let due_ns = self.last_report_acceptance_ns.map_or(now, |last| {
            last.saturating_add(self.options.minimum_report_interval_ns)
                .max(now)
        });
        let deadline_id = self.clock.register_deadline(due_ns);
        self.pending_reports.push_front(ScheduledReport {
            target_ns: now,
            due_ns,
            deadline_id,
            operation,
            settlement_owner: None,
            operation_settlement,
            automation_generation: None,
            report: SwitchReport::NEUTRAL,
            mutation: None,
            direct_timing: None,
            kind: WriteKind::Neutralize,
            completion: ReportCompletion::Failed {
                error,
                release_sequence,
            },
        });
    }

    fn rebuild_pending_after_neutral(&mut self) {
        let mut desired = SwitchReport::NEUTRAL;
        for pending in &mut self.pending_reports {
            if let Some(action) = pending.mutation {
                action.apply(&mut desired);
                pending.report = desired;
            }
        }
        self.desired_report = desired;
        self.update_desired_snapshot();
    }

    fn dispatch_due_reports(&mut self) {
        loop {
            let now = self.clock.now_ns();
            let Some(mut pending) = self.pending_reports.pop_front() else {
                return;
            };
            if pending.due_ns > now {
                self.pending_reports.push_front(pending);
                return;
            }
            if pending.kind == WriteKind::Report
                && pending.settlement_owner.is_some()
                && pending.operation.snapshot().cancellation_reason.is_some()
            {
                let owner = pending
                    .settlement_owner
                    .take()
                    .expect("owned Automation report retains its settlement owner");
                settle_owned_cancellation(owner, &pending.operation);
                continue;
            }
            if pending.kind == WriteKind::Report
                && pending.operation_settlement.is_some()
                && pending.operation.snapshot().cancellation_reason.is_some()
            {
                self.pending_reports.push_front(pending);
                self.observe_cancellation();
                continue;
            }
            if pending.kind == WriteKind::Report
                && pending.operation.snapshot().state == OperationState::Cancelling
            {
                self.pending_reports.push_front(pending);
                self.observe_cancellation();
                continue;
            }
            let earliest = self.last_report_acceptance_ns.map_or(now, |last| {
                last.saturating_add(self.options.minimum_report_interval_ns)
            });
            if earliest > now {
                pending.due_ns = earliest;
                pending.deadline_id = self.clock.register_deadline(earliest);
                self.publish_timing_deviation(pending.operation.id(), pending.target_ns, earliest);
                self.pending_reports.push_front(pending);
                return;
            }

            let bytes = pending.report.encode();
            let owned_settlement = pending.settlement_owner.is_some();
            let operation_settlement = pending.operation_settlement.clone();
            let completes_operation_effect = matches!(
                &pending.completion,
                ReportCompletion::Direct | ReportCompletion::Sequence { final_report: true }
            ) && pending.kind == WriteKind::Report;
            let settlement = self.action_write_settlement(
                pending.settlement_owner.take(),
                operation_settlement.clone(),
                pending.operation.clone(),
                completes_operation_effect,
            );
            let write_result = self.write_payload_with_cancellation(
                Some(&pending.operation),
                pending.kind,
                now,
                pending.direct_timing,
                PayloadWrite {
                    bytes: &bytes,
                    cancellation_override: None,
                    settlement: &settlement,
                },
            );
            if completes_operation_effect && settlement.full_accepted_outcome() {
                // The backend reserved this record at the physical final-byte boundary. Claim
                // the corresponding Runtime winner here, after the backend has published and
                // every settlement/transport lock is released. A WriteSettlement observer is
                // intentionally not the only path to this claim: its panic is isolated.
                let _ = operation_settlement
                    .as_ref()
                    .expect("effect-completing report retains its operation settlement")
                    .claim_reserved_full_accepted();
            }
            if operation_settlement
                .as_ref()
                .is_some_and(|record| record.claimed_full())
            {
                let accepted_at_ns = settlement
                    .accepted_at_ns()
                    .expect("gate-claimed report retains its backend acceptance timestamp");
                self.clock.record_dispatch(pending.deadline_id, now);
                self.last_report_acceptance_ns = Some(accepted_at_ns);
                self.record_report_acceptance(accepted_at_ns, Some(pending.operation.id()), bytes);
                if matches!(&pending.completion, ReportCompletion::Sequence { .. }) {
                    self.desired_report = pending.report;
                    self.update_desired_snapshot();
                }
                if matches!(
                    &pending.completion,
                    ReportCompletion::Sequence { final_report: true }
                ) {
                    self.release_sequence(pending.operation.id());
                }
                let _ = operation_settlement
                    .as_ref()
                    .expect("gate-claimed report retains its settlement record")
                    .finish_success();
                continue;
            }
            match write_result {
                Ok(()) => {
                    let accepted_at_ns = settlement
                        .accepted_at_ns()
                        .expect("successful report write retains its backend acceptance timestamp");
                    self.clock.record_dispatch(pending.deadline_id, now);
                    self.last_report_acceptance_ns = Some(accepted_at_ns);
                    self.record_report_acceptance(
                        accepted_at_ns,
                        Some(pending.operation.id()),
                        bytes,
                    );
                    if matches!(&pending.completion, ReportCompletion::Sequence { .. }) {
                        self.desired_report = pending.report;
                        self.update_desired_snapshot();
                    }
                    if owned_settlement {
                        continue;
                    }
                    if operation_settlement.is_some_and(|record| !record.is_pending()) {
                        if matches!(
                            &pending.completion,
                            ReportCompletion::Sequence { final_report: true }
                        ) {
                            self.release_sequence(pending.operation.id());
                        }
                        continue;
                    }
                    if pending.kind == WriteKind::Report
                        && pending.operation.snapshot().state == OperationState::Cancelling
                    {
                        self.cancel_after_accepted_report(pending);
                        continue;
                    }
                    match pending.completion {
                        ReportCompletion::Direct => {
                            if pending.operation.succeed(OperationValue::Unit)
                                == TransitionOutcome::Invalid
                                && pending.operation.snapshot().state == OperationState::Cancelling
                            {
                                self.schedule_cancel_cleanup(pending.operation, false);
                            }
                        }
                        ReportCompletion::Sequence {
                            final_report: false,
                        } => {}
                        ReportCompletion::Sequence { final_report: true } => {
                            let operation_id = pending.operation.id();
                            let outcome = pending
                                .operation
                                .succeed_after_cleanup(OperationValue::Unit, || {
                                    self.release_sequence(operation_id)
                                });
                            if outcome == TransitionOutcome::Invalid
                                && pending.operation.snapshot().state == OperationState::Cancelling
                            {
                                self.schedule_cancel_cleanup(pending.operation, true);
                            }
                        }
                        ReportCompletion::Cancelled { release_sequence } => {
                            if release_sequence {
                                self.release_sequence(pending.operation.id());
                            }
                            if let Some(settlement) = pending.operation_settlement.take() {
                                settlement.settle_cancellation(&pending.operation);
                            } else {
                                pending.operation.finish_cancelled();
                            }
                        }
                        ReportCompletion::Failed {
                            error,
                            release_sequence,
                        } => {
                            let operation_id = pending.operation.id();
                            if let Some(settlement) = pending.operation_settlement.take() {
                                if release_sequence {
                                    self.release_sequence(operation_id);
                                }
                                settlement.settle_failure(&pending.operation, error);
                                continue;
                            }
                            let outcome = if release_sequence {
                                pending.operation.fail_after_cleanup(error, || {
                                    self.release_sequence(operation_id);
                                })
                            } else {
                                pending.operation.fail(error)
                            };
                            if outcome == TransitionOutcome::Invalid
                                && pending.operation.snapshot().state == OperationState::Cancelling
                            {
                                if release_sequence {
                                    self.release_sequence(operation_id);
                                }
                                pending.operation.finish_cancelled();
                            }
                        }
                    }
                }
                Err(error) => {
                    self.handle_report_write_failure(pending, error, owned_settlement);
                }
            }
        }
    }

    fn cancel_after_accepted_report(&mut self, mut pending: ScheduledReport) {
        let operation_id = pending.operation.id();
        let release_sequence = self.lease_owner == Some(LeaseOwner::Sequence(operation_id));
        self.pending_reports
            .retain(|queued| queued.operation.id() != operation_id);
        self.schedule_cancel_cleanup_with_settlement(
            pending.operation,
            release_sequence,
            pending.operation_settlement.take(),
        );
    }

    fn handle_report_write_failure(
        &mut self,
        mut pending: ScheduledReport,
        error: TransportError,
        owned_settlement: bool,
    ) {
        let operation_id = pending.operation.id();
        let release_sequence = self.lease_owner == Some(LeaseOwner::Sequence(operation_id));
        let automation_release_pending = pending
            .automation_generation
            .as_ref()
            .is_some_and(|generation| generation.is_sealed());
        let operation_settlement = pending.operation_settlement.take();
        if error.is_cleanup_failure() {
            let cleanup_failure = self.permanent_failure.get().unwrap_or_else(|| {
                self.seal_after_cleanup_failure(map_transport_error(error.clone()))
            });
            if let Some(generation) = pending.automation_generation.as_ref() {
                generation.record_cleanup_failure(cleanup_failure.clone());
                self.release_automation_lease(generation.id);
            }
            self.pending_reports
                .retain(|queued| queued.operation.id() != operation_id);
            if !owned_settlement {
                if matches!(&pending.completion, ReportCompletion::Sequence { .. }) {
                    self.release_sequence(operation_id);
                }
                if let Some(settlement) = operation_settlement {
                    settlement.settle_strict_failure(cleanup_failure);
                } else {
                    fail_strict_cleanup(&pending.operation, cleanup_failure);
                }
            }
            return;
        }
        self.pending_reports
            .retain(|queued| queued.operation.id() != operation_id);
        self.desired_report.reset();
        self.update_desired_snapshot();

        // The backend settlement hook consumed the exclusive owner only after the stream was
        // settled. The lane still owns report/lease cleanup, but must not claim a second winner.
        if owned_settlement {
            if automation_release_pending {
                return;
            }
            if error.kind() == TransportErrorKind::Cancelled
                || pending.operation.snapshot().cancellation_reason.is_some()
            {
                self.schedule_cancel_cleanup(pending.operation, false);
            } else {
                self.schedule_failure_cleanup(pending.operation, map_transport_error(error), false);
            }
            return;
        }

        if let Some(owner) = pending.settlement_owner.take() {
            if error.kind() == TransportErrorKind::Cancelled
                || pending.operation.snapshot().cancellation_reason.is_some()
            {
                settle_owned_cancellation(owner, &pending.operation);
            } else {
                settle_owned_failure(
                    owner,
                    &pending.operation,
                    map_transport_error(error.clone()),
                );
            }
            if automation_release_pending {
                return;
            }
            if error.kind() == TransportErrorKind::Cancelled {
                self.schedule_cancel_cleanup(pending.operation, false);
            } else {
                self.schedule_failure_cleanup(pending.operation, map_transport_error(error), false);
            }
            return;
        }

        if pending.kind == WriteKind::Report
            && let Some(settlement) = operation_settlement
        {
            if automation_release_pending {
                if error.kind() == TransportErrorKind::Cancelled
                    || pending.operation.snapshot().cancellation_reason.is_some()
                {
                    settlement.settle_cancellation(&pending.operation);
                } else {
                    settlement.settle_failure(&pending.operation, map_transport_error(error));
                }
                return;
            }
            if error.kind() == TransportErrorKind::Cancelled
                || pending.operation.snapshot().cancellation_reason.is_some()
            {
                self.schedule_cancel_cleanup_with_settlement(
                    pending.operation,
                    release_sequence,
                    Some(settlement),
                );
            } else {
                self.schedule_failure_cleanup_with_settlement(
                    pending.operation,
                    map_transport_error(error),
                    release_sequence,
                    Some(settlement),
                );
            }
            return;
        }

        if error.kind() == TransportErrorKind::Cancelled
            && pending.operation.snapshot().state != OperationState::Cancelling
        {
            pending
                .operation
                .request_cancel(easycon_runtime::CancellationReason::ParentClose);
        }
        let cancelling = pending.operation.snapshot().state == OperationState::Cancelling;
        if pending.kind == WriteKind::Neutralize || error.kind() == TransportErrorKind::Disconnected
        {
            if error.kind() == TransportErrorKind::Disconnected {
                self.set_state(
                    ControllerState::Disconnected,
                    "controller.disconnected",
                    None,
                );
                if let Err(cleanup_failure) =
                    self.close_transport_with_cleanup_failure(Some(&error))
                {
                    self.publish_neutralization_warning(Some(operation_id), &error);
                    if release_sequence {
                        self.release_sequence(operation_id);
                    }
                    if let Some(settlement) = operation_settlement {
                        settlement.settle_strict_failure(cleanup_failure);
                    } else {
                        fail_strict_cleanup(&pending.operation, cleanup_failure);
                    }
                    self.fail_pending_disconnected();
                    return;
                }
            }
            self.publish_neutralization_warning(Some(operation_id), &error);
            if release_sequence {
                self.release_sequence(operation_id);
            }
            if self.state() != ControllerState::Disconnected {
                self.rebuild_pending_after_neutral();
            }
            match pending.completion {
                ReportCompletion::Cancelled { .. } => {
                    if let Some(settlement) = operation_settlement {
                        settlement.settle_cancellation(&pending.operation);
                    } else {
                        pending.operation.finish_cancelled();
                    }
                }
                ReportCompletion::Failed { error, .. } => {
                    if let Some(settlement) = operation_settlement {
                        settlement.settle_failure(&pending.operation, error);
                    } else if cancelling {
                        pending.operation.finish_cancelled();
                    } else {
                        fail_or_finish_cancelled(&pending.operation, error);
                    }
                }
                ReportCompletion::Direct | ReportCompletion::Sequence { .. } => {
                    if let Some(settlement) = operation_settlement {
                        settlement.settle_failure(&pending.operation, map_transport_error(error));
                    } else if cancelling {
                        pending.operation.finish_cancelled();
                    } else {
                        fail_or_finish_cancelled(&pending.operation, map_transport_error(error));
                    }
                }
            }
            if self.state() == ControllerState::Disconnected {
                self.fail_pending_disconnected();
            }
            return;
        }

        if cancelling {
            self.schedule_cancel_cleanup(pending.operation, release_sequence);
            return;
        }

        self.schedule_failure_cleanup(
            pending.operation,
            map_transport_error(error),
            release_sequence,
        );
    }

    fn action_write_settlement(
        &self,
        owner: Option<OperationSettlementOwner>,
        operation_settlement: Option<Arc<OperationSettlementRecord>>,
        operation: Operation,
        completes_operation_effect: bool,
    ) -> WriteSettlement {
        if let Some(owner) = owner {
            return WriteSettlement::tracked(Some(Box::new(move |outcome| match outcome {
                WriteSettlementOutcome::FullAccepted => settle_owned_success(owner),
                WriteSettlementOutcome::NotDeliveredStreamSettled(error) => {
                    settle_owned_failure_or_cancellation(owner, &operation, error);
                }
                WriteSettlementOutcome::CleanupFailed(error) => {
                    settle_owned_strict_failure(owner, error);
                }
            })));
        }
        let Some(operation_settlement) = operation_settlement else {
            return WriteSettlement::tracked(None);
        };
        if !completes_operation_effect {
            return WriteSettlement::tracked(None);
        }
        let reservation = Arc::clone(&operation_settlement);
        WriteSettlement::tracked_with_full_reservation(
            Some(Box::new(move || reservation.reserve_full_accepted())),
            None,
        )
    }

    fn write_payload(
        &mut self,
        operation: Option<&Operation>,
        kind: WriteKind,
        timestamp_ns: u64,
        direct_timing: Option<DirectWriteTiming>,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        let settlement = WriteSettlement::tracked(None);
        self.write_payload_with_cancellation(
            operation,
            kind,
            timestamp_ns,
            direct_timing,
            PayloadWrite {
                bytes,
                cancellation_override: None,
                settlement: &settlement,
            },
        )
    }

    fn write_payload_with_cancellation(
        &mut self,
        operation: Option<&Operation>,
        kind: WriteKind,
        timestamp_ns: u64,
        direct_timing: Option<DirectWriteTiming>,
        payload: PayloadWrite<'_>,
    ) -> Result<(), TransportError> {
        let PayloadWrite {
            bytes,
            cancellation_override,
            settlement,
        } = payload;
        if !settlement.mark_dispatched() {
            return Err(TransportError::new(
                TransportErrorKind::Io,
                "logical report settlement gate was not dispatchable",
            ));
        }
        let operation_id = operation.map(Operation::id);
        let sequence = self.next_write_sequence;
        self.next_write_sequence = self
            .next_write_sequence
            .checked_add(1)
            .expect("controller write sequence exhausted");
        let context = WriteContext {
            resource_id: self.resource_id,
            operation_id,
            sequence,
            timestamp_ns,
            direct_timing,
            total_len: bytes.len(),
            kind,
        };
        let cancellation = cancellation_override.unwrap_or_else(|| {
            if matches!(kind, WriteKind::Report | WriteKind::Command) {
                operation.map_or_else(easycon_runtime::CancellationToken::root, |operation| {
                    operation.cancellation_token()
                })
            } else {
                easycon_runtime::CancellationToken::root()
            }
        });
        let resource_cancellation = if kind == WriteKind::Neutralize {
            easycon_runtime::CancellationToken::root()
        } else {
            self.resource_cancellation.clone()
        };
        let deadline_ns = self
            .clock
            .now_ns()
            .saturating_add(self.options.write_timeout_ns);
        let mut written = 0;
        while written < bytes.len() {
            let accepted = self.transport.write(WriteRequest {
                context,
                bytes: &bytes[written..],
                deadline_ns,
                cancellation: cancellation.clone(),
                resource_cancellation: resource_cancellation.clone(),
                settlement: settlement.clone(),
            });
            let accepted = match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    if settlement.full_accepted_outcome() {
                        return Ok(());
                    }
                    let settled = self.settle_failed_write(
                        settlement,
                        error.clone(),
                        written != 0 || !error.stream_reusable(),
                    );
                    return Err(settled.err().unwrap_or(error));
                }
            };
            if accepted == 0 || accepted > bytes.len() - written {
                let error = TransportError::new(
                    TransportErrorKind::Io,
                    "transport returned an invalid partial-write length",
                );
                let settled = self.settle_failed_write(settlement, error.clone(), true);
                return Err(settled.err().unwrap_or(error));
            }
            written += accepted;
        }
        if !settlement.full_accepted_outcome() {
            let error = TransportError::new(
                TransportErrorKind::Io,
                "transport returned without final-byte settlement",
            );
            let settled = self.settle_failed_write(settlement, error.clone(), true);
            return Err(settled.err().unwrap_or(error));
        }
        Ok(())
    }

    fn settle_failed_write(
        &mut self,
        settlement: &WriteSettlement,
        error: TransportError,
        close_stream: bool,
    ) -> Result<(), TransportError> {
        if !settlement.begin_non_full() {
            return Ok(());
        }
        // A transport error alone does not prove that no late completion can occur. Only an
        // adapter-proven zero-effect reusable stream may settle without a close.
        if close_stream && let Err(cleanup) = self.close_settled_stream() {
            let cleanup_failure = TransportError::cleanup_failure(&error, &cleanup);
            let first_failure =
                self.seal_after_cleanup_failure(map_transport_error(cleanup_failure.clone()));
            let _ = settlement.cleanup_failed(first_failure);
            return Err(cleanup_failure);
        }
        let _ = settlement.not_delivered_stream_settled(map_transport_error(error));
        Ok(())
    }

    fn close_settled_stream(&mut self) -> Result<(), TransportError> {
        let result = self.checked_transport_close();
        if self.state() == ControllerState::Connected {
            self.set_state(
                ControllerState::Disconnected,
                "controller.disconnected",
                None,
            );
        }
        result
    }

    fn checked_transport_close(&mut self) -> Result<(), TransportError> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.transport.close_checked()
        })) {
            Ok(result) => result,
            Err(payload) => {
                // A foreign panic payload can panic again when dropped. It is intentionally
                // isolated at this transport boundary after the lane has retained its error.
                std::mem::forget(payload);
                Err(TransportError::new(
                    TransportErrorKind::Io,
                    "Controller transport close panicked",
                ))
            }
        }
    }

    fn close_transport_with_cleanup_failure(
        &mut self,
        primary: Option<&TransportError>,
    ) -> Result<(), EasyConError> {
        self.checked_transport_close().map_err(|close_error| {
            let failure = primary.map_or(close_error.clone(), |primary| {
                TransportError::cleanup_failure(primary, &close_error)
            });
            self.seal_after_cleanup_failure(map_transport_error(failure))
        })
    }

    fn record_report_acceptance(
        &self,
        timestamp_ns: u64,
        operation_id: Option<OperationId>,
        bytes: [u8; 8],
    ) {
        let mut snapshot = self
            .snapshot
            .lock()
            .expect("controller snapshot lock poisoned");
        snapshot.accepted_report_count = snapshot
            .accepted_report_count
            .checked_add(1)
            .expect("accepted report counter exhausted");
        snapshot.last_report_timestamp_ns = Some(timestamp_ns);
        drop(snapshot);
        let mut event = EventDraft::ordinary(
            EventKind::Data,
            "controller.report.transport_accepted",
            Severity::Info,
        )
        .with_resource(self.resource_id)
        .with_detail(format!("bytes={bytes:02x?}; hardware_execution=false"));
        if let Some(operation_id) = operation_id {
            event = event.with_operation(operation_id);
        }
        let _ = self.runtime.publish(event);
    }

    fn fail_pending_disconnected(&mut self) {
        let pending_reports: Vec<_> = self.pending_reports.drain(..).collect();
        let waiting = self.waiting_sequence.take();
        let close_failure = self.close_transport_with_cleanup_failure(None).err();
        for mut pending in pending_reports {
            let operation_id = pending.operation.id();
            if self.lease_owner == Some(LeaseOwner::Sequence(operation_id)) {
                self.set_lease_owner(None);
            }
            if let Some(error) = close_failure.as_ref() {
                settle_report_strict_failure(
                    pending.settlement_owner.take(),
                    pending.operation_settlement.take(),
                    &pending.operation,
                    error.clone(),
                );
                continue;
            }
            let error = EasyConError::new(
                ErrorDomain::Io,
                ErrorCode::DeviceDisconnected,
                "controller disconnected before report acceptance",
            );
            if let Some(owner) = pending.settlement_owner.take() {
                settle_owned_failure(owner, &pending.operation, error);
                continue;
            }
            if let Some(settlement) = pending.operation_settlement.take() {
                settlement.settle_failure(&pending.operation, error);
                continue;
            }
            if pending.operation.snapshot().state == OperationState::Cancelling {
                pending.operation.finish_cancelled();
            } else {
                fail_or_finish_cancelled(&pending.operation, error);
            }
        }
        if let Some(waiting) = waiting {
            self.release_sequence(waiting.operation.id());
            if let Some(error) = close_failure {
                waiting.operation_settlement.settle_strict_failure(error);
            } else {
                waiting.operation_settlement.settle_failure(
                    &waiting.operation,
                    EasyConError::new(
                        ErrorDomain::Io,
                        ErrorCode::DeviceDisconnected,
                        "controller disconnected before sequence dispatch",
                    ),
                );
            }
        }
    }

    fn collect_command_during_close(
        &mut self,
        command: LaneCommand,
        operations: &mut Vec<Operation>,
        report_settlements: &mut Vec<CloseOperationSettlement>,
        close_waiters: &mut Vec<SyncSender<()>>,
        automation_cleanup: &mut Option<AutomationCleanup>,
    ) {
        match command {
            LaneCommand::Direct {
                operation,
                settlement_owner,
                operation_settlement,
                ..
            } => {
                if !operation.snapshot().state.is_terminal() {
                    let _ =
                        operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
                }
                if let Some(owner) = settlement_owner {
                    report_settlements.push(CloseOperationSettlement::Owner { owner, operation });
                } else if let Some(settlement) = operation_settlement {
                    report_settlements.push(CloseOperationSettlement::Record {
                        record: settlement,
                        operation,
                    });
                } else {
                    operations.push(operation);
                }
            }
            LaneCommand::Sequence {
                operation,
                operation_settlement,
                ..
            } => {
                if !operation.snapshot().state.is_terminal() {
                    operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
                }
                report_settlements.push(CloseOperationSettlement::Record {
                    record: operation_settlement,
                    operation,
                });
            }
            LaneCommand::Connect { operation, .. }
            | LaneCommand::Ack { operation, .. }
            | LaneCommand::AmiiboSave { operation, .. }
            | LaneCommand::AmiiboSelect { operation, .. } => {
                if !operation.snapshot().state.is_terminal() {
                    operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
                }
                operations.push(operation);
            }
            LaneCommand::AcquireAutomationLease { record } => {
                let _ = record.resolve_with(|| AutomationLeaseAcquireOutcome::Closed);
            }
            LaneCommand::BeginAutomationLeaseRelease {
                generation,
                completed,
            } => {
                completed.request_close_interrupt();
                if automation_cleanup.is_none()
                    && self.lease_owner == Some(LeaseOwner::Automation(generation.id))
                {
                    *automation_cleanup = Some(self.new_automation_cleanup(generation, completed));
                } else if automation_cleanup
                    .as_ref()
                    .is_none_or(|cleanup| cleanup.generation.id != generation.id)
                {
                    completed.complete(Ok(
                        AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(
                            closed_lane_error(),
                        ),
                    ));
                }
            }
            LaneCommand::Close { completed } => close_waiters.push(completed),
            LaneCommand::Wake => {}
        }
    }

    fn wait_for_close_target(
        &mut self,
        target_ns: u64,
        operations: &mut Vec<Operation>,
        report_settlements: &mut Vec<CloseOperationSettlement>,
        close_waiters: &mut Vec<SyncSender<()>>,
        automation_cleanup: &mut Option<AutomationCleanup>,
    ) {
        while self.clock.now_ns() < target_ns {
            let command = match self.clock.real_wait_duration(target_ns) {
                Some(duration) => match self.receiver.recv_timeout(duration) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => {
                        std::thread::sleep(duration);
                        None
                    }
                },
                None => Some(
                    self.receiver
                        .recv()
                        .expect("controller clock hook keeps the close wait channel open"),
                ),
            };
            if let Some(command) = command {
                self.collect_command_during_close(
                    command,
                    operations,
                    report_settlements,
                    close_waiters,
                    automation_cleanup,
                );
            }
        }
    }

    fn settle_automation_cleanup_during_close(
        &mut self,
        cleanup: AutomationCleanup,
        was_connected: bool,
        operations: &mut Vec<Operation>,
        report_settlements: &mut Vec<CloseOperationSettlement>,
        close_waiters: &mut Vec<SyncSender<()>>,
        automation_cleanup: &mut Option<AutomationCleanup>,
    ) {
        cleanup.completed.request_close_interrupt();
        if let Some(error) = cleanup.generation.cleanup_failure() {
            self.release_automation_lease(cleanup.generation.id);
            cleanup.completed.complete(Err(error));
            self.close_consumed_release = true;
            return;
        }
        let now = self.clock.now_ns();
        let paced_target = self.last_report_acceptance_ns.map_or(now, |last| {
            last.saturating_add(self.options.minimum_report_interval_ns)
                .max(now)
        });
        let target_ns = cleanup.target_ns.max(paced_target);
        self.wait_for_close_target(
            target_ns,
            operations,
            report_settlements,
            close_waiters,
            automation_cleanup,
        );
        self.desired_report.reset();
        self.update_desired_snapshot();
        let dispatch_ns = self.clock.now_ns();
        let (cancellation, _) = cleanup.completed.begin_neutral();
        let settlement = release_write_settlement(cleanup.completed.clone());
        let bytes = SwitchReport::NEUTRAL.encode();
        let result = if was_connected {
            self.write_payload_with_cancellation(
                None,
                WriteKind::Neutralize,
                dispatch_ns,
                None,
                PayloadWrite {
                    bytes: &bytes,
                    cancellation_override: Some(cancellation),
                    settlement: &settlement,
                },
            )
        } else {
            Err(TransportError::new(
                TransportErrorKind::Disconnected,
                "controller disconnected before Automation release neutralization",
            ))
        };
        match result {
            Ok(()) => {
                let accepted_at_ns = settlement
                    .accepted_at_ns()
                    .expect("successful neutral write retains its backend acceptance timestamp");
                self.clock.record_dispatch(cleanup.deadline_id, dispatch_ns);
                self.last_report_acceptance_ns = Some(accepted_at_ns);
                self.record_report_acceptance(accepted_at_ns, None, bytes);
                self.release_automation_lease(cleanup.generation.id);
                cleanup
                    .completed
                    .complete(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted));
            }
            Err(error) => {
                self.publish_neutralization_warning(None, &error);
                self.release_automation_lease(cleanup.generation.id);
                if error.is_cleanup_failure() {
                    cleanup.completed.complete(Err(map_transport_error(error)));
                } else {
                    cleanup.completed.complete(Ok(
                        AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(
                            map_transport_error(error),
                        ),
                    ));
                }
            }
        }
        self.close_consumed_release = true;
    }

    fn close_lane(&mut self, completed: Option<SyncSender<()>>) {
        self.close_consumed_release |= self.release_interrupts.request_close_interrupts();
        let was_connected = matches!(
            self.state(),
            ControllerState::Connected | ControllerState::Disconnecting
        );
        if was_connected {
            self.set_state(
                ControllerState::Disconnecting,
                "controller.disconnecting",
                None,
            );
        }
        let mut automation_cleanup = self.automation_cleanup.take();
        let pending_reports: Vec<_> = self.pending_reports.drain(..).collect();
        let mut operations = Vec::new();
        let mut report_settlements = Vec::new();
        for mut pending in pending_reports {
            if !pending.operation.snapshot().state.is_terminal() {
                let _ = pending
                    .operation
                    .request_cancel(easycon_runtime::CancellationReason::ParentClose);
            }
            if let Some(owner) = pending.settlement_owner.take() {
                report_settlements.push(CloseOperationSettlement::Owner {
                    owner,
                    operation: pending.operation,
                });
            } else if let Some(settlement) = pending.operation_settlement.take() {
                report_settlements.push(CloseOperationSettlement::Record {
                    record: settlement,
                    operation: pending.operation,
                });
            } else {
                operations.push(pending.operation);
            }
        }
        let mut close_waiters = Vec::new();
        let mut close_deferred_operations = std::mem::take(&mut self.close_deferred_operations);
        let queued: Vec<_> = self
            .deferred_commands
            .drain(..)
            .chain(self.receiver.try_iter())
            .collect();
        for command in queued {
            self.collect_command_during_close(
                command,
                &mut operations,
                &mut report_settlements,
                &mut close_waiters,
                &mut automation_cleanup,
            );
        }
        if let Some(waiting) = self.waiting_sequence.take() {
            if !waiting.operation.snapshot().state.is_terminal() {
                let _ = waiting
                    .operation
                    .request_cancel(easycon_runtime::CancellationReason::ParentClose);
            }
            report_settlements.push(CloseOperationSettlement::Record {
                record: waiting.operation_settlement,
                operation: waiting.operation,
            });
        }
        operations.sort_by_key(|operation| operation.id());
        operations.dedup_by_key(|operation| operation.id());
        for operation in &operations {
            if !operation.snapshot().state.is_terminal() {
                operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
            }
        }
        self.desired_report.reset();
        self.update_desired_snapshot();
        if let Some(cleanup) = automation_cleanup.take() {
            self.settle_automation_cleanup_during_close(
                cleanup,
                was_connected,
                &mut operations,
                &mut report_settlements,
                &mut close_waiters,
                &mut automation_cleanup,
            );
        } else if was_connected && !self.close_consumed_release {
            let bytes = SwitchReport::NEUTRAL.encode();
            let now = self.clock.now_ns();
            let target_ns = self.last_report_acceptance_ns.map_or(now, |last| {
                last.saturating_add(self.options.minimum_report_interval_ns)
                    .max(now)
            });
            let deadline_id = self.clock.register_deadline(target_ns);
            self.wait_for_close_target(
                target_ns,
                &mut operations,
                &mut report_settlements,
                &mut close_waiters,
                &mut automation_cleanup,
            );
            if let Some(cleanup) = automation_cleanup.take() {
                self.settle_automation_cleanup_during_close(
                    cleanup,
                    was_connected,
                    &mut operations,
                    &mut report_settlements,
                    &mut close_waiters,
                    &mut automation_cleanup,
                );
            } else {
                let dispatch_ns = self.clock.now_ns();
                let settlement = WriteSettlement::tracked(None);
                match self.write_payload_with_cancellation(
                    None,
                    WriteKind::Neutralize,
                    dispatch_ns,
                    None,
                    PayloadWrite {
                        bytes: &bytes,
                        cancellation_override: None,
                        settlement: &settlement,
                    },
                ) {
                    Ok(()) => {
                        let accepted_at_ns = settlement.accepted_at_ns().expect(
                            "successful close neutral retains its backend acceptance timestamp",
                        );
                        self.clock.record_dispatch(deadline_id, dispatch_ns);
                        self.last_report_acceptance_ns = Some(accepted_at_ns);
                        self.record_report_acceptance(accepted_at_ns, None, bytes);
                    }
                    Err(error) => {
                        let _ = self.runtime.publish(
                            EventDraft::critical(
                                EventKind::Warning,
                                "controller.neutralization.not_delivered",
                                Severity::Warning,
                            )
                            .with_resource(self.resource_id)
                            .with_detail(error.to_string()),
                        );
                    }
                }
            }
        }
        self.set_lease_owner(None);
        operations.sort_by_key(|operation| operation.id());
        operations.dedup_by_key(|operation| operation.id());
        let close_failure = self.close_transport_with_cleanup_failure(None).err();
        if let Some(error) = close_failure {
            for settlement in report_settlements {
                settlement.settle_strict_failure(error.clone());
            }
            for operation in operations {
                fail_strict_cleanup(&operation, error.clone());
            }
            close_deferred_operations.sort_by_key(Operation::id);
            close_deferred_operations.dedup_by_key(|operation| operation.id());
            for operation in close_deferred_operations {
                fail_strict_cleanup(&operation, error.clone());
            }
        } else {
            for settlement in report_settlements {
                settlement.settle_cancellation();
            }
            for operation in operations {
                if operation.snapshot().state == OperationState::Cancelling {
                    operation.finish_cancelled();
                }
            }
            close_deferred_operations.sort_by_key(Operation::id);
            close_deferred_operations.dedup_by_key(|operation| operation.id());
            for operation in close_deferred_operations {
                if operation.snapshot().state == OperationState::Cancelling {
                    operation.finish_cancelled();
                }
            }
        }
        self.release_interrupts.settle_stream_after_close();
        self.set_state(ControllerState::Closed, "controller.closed", None);
        if let Some(completed) = completed {
            let _ = completed.send(());
        }
        for completed in close_waiters {
            let _ = completed.send(());
        }
    }

    fn state(&self) -> ControllerState {
        self.snapshot
            .lock()
            .expect("controller snapshot lock poisoned")
            .state
    }

    fn record_permanent_cleanup_failure(&self, error: EasyConError) -> EasyConError {
        let first_failure = self.permanent_failure.record(error);
        self.release_interrupts
            .record_cleanup_failure(first_failure.clone());
        first_failure
    }

    fn seal_after_cleanup_failure(&mut self, error: EasyConError) -> EasyConError {
        let first_failure = self.record_permanent_cleanup_failure(error);
        self.desired_report.reset();
        self.update_desired_snapshot();

        let pending_reports: Vec<_> = self.pending_reports.drain(..).collect();
        for pending in pending_reports {
            self.fail_scheduled_after_cleanup_failure(pending, &first_failure);
        }
        if let Some(waiting) = self.waiting_sequence.take() {
            self.release_sequence(waiting.operation.id());
            waiting
                .operation_settlement
                .settle_strict_failure(first_failure.clone());
        }
        if let Some(cleanup) = self.automation_cleanup.take() {
            cleanup
                .generation
                .record_cleanup_failure(first_failure.clone());
            self.release_automation_lease(cleanup.generation.id);
            cleanup.completed.complete(Err(first_failure.clone()));
        }
        self.set_lease_owner(None);
        first_failure
    }

    fn fail_scheduled_after_cleanup_failure(
        &mut self,
        mut pending: ScheduledReport,
        error: &EasyConError,
    ) {
        if let Some(generation) = pending.automation_generation.as_ref() {
            generation.record_cleanup_failure(error.clone());
            self.release_automation_lease(generation.id);
        }
        if let Some(owner) = pending.settlement_owner.take() {
            settle_owned_strict_failure(owner, error.clone());
            return;
        }
        if let Some(settlement) = pending.operation_settlement.take() {
            if matches!(&pending.completion, ReportCompletion::Sequence { .. }) {
                self.release_sequence(pending.operation.id());
            }
            settlement.settle_strict_failure(error.clone());
            return;
        }
        if matches!(&pending.completion, ReportCompletion::Sequence { .. }) {
            self.release_sequence(pending.operation.id());
        }
        fail_strict_cleanup(&pending.operation, error.clone());
    }

    fn set_state(
        &self,
        state: ControllerState,
        code: &'static str,
        operation_id: Option<OperationId>,
    ) {
        self.snapshot
            .lock()
            .expect("controller snapshot lock poisoned")
            .state = state;
        let mut event = EventDraft::critical(EventKind::State, code, Severity::Info)
            .with_resource(self.resource_id);
        if let Some(operation_id) = operation_id {
            event = event.with_operation(operation_id);
        }
        let _ = self.runtime.publish(event);
    }

    fn update_desired_snapshot(&self) {
        self.snapshot
            .lock()
            .expect("controller snapshot lock poisoned")
            .desired_report = self.desired_report;
    }

    fn set_lease_owner(&mut self, owner: Option<LeaseOwner>) {
        if self.lease_owner == owner {
            return;
        }
        self.lease_owner = owner;
        let state = match owner {
            None => ControllerLeaseState::Available,
            Some(LeaseOwner::Sequence(operation)) => ControllerLeaseState::Sequence(operation),
            Some(LeaseOwner::Automation(lease)) => ControllerLeaseState::Automation(lease),
        };
        self.snapshot
            .lock()
            .expect("controller snapshot lock poisoned")
            .lease = state;
        self.publish_event_isolated(
            EventDraft::critical(
                EventKind::State,
                if owner.is_some() {
                    "controller.lease.acquired"
                } else {
                    "controller.lease.released"
                },
                Severity::Info,
            )
            .with_resource(self.resource_id),
        );
    }

    fn publish_event_isolated(&self, draft: EventDraft) {
        if let Err(payload) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.runtime.publish(draft)))
        {
            // Runtime events timestamp through the public Clock. The lane state was already
            // committed before publication, so a foreign Clock panic cannot roll it back or
            // poison admission/close. The payload itself is isolated for the same reason.
            std::mem::forget(payload);
        }
    }

    fn release_sequence(&mut self, operation_id: OperationId) {
        if self.lease_owner == Some(LeaseOwner::Sequence(operation_id)) {
            self.set_lease_owner(None);
        }
    }

    fn publish_timing_deviation(
        &self,
        operation_id: OperationId,
        target_ns: u64,
        dispatch_not_before_ns: u64,
    ) {
        let _ = self.runtime.publish(
            EventDraft::ordinary(
                EventKind::TimingDeviation,
                "controller.report.delayed",
                Severity::Warning,
            )
            .with_resource(self.resource_id)
            .with_operation(operation_id)
            .with_detail(format!(
                "target_ns={target_ns}, dispatch_not_before_ns={dispatch_not_before_ns}"
            )),
        );
    }

    fn publish_neutralization_warning(
        &self,
        operation_id: Option<OperationId>,
        error: &TransportError,
    ) {
        let mut event = EventDraft::critical(
            EventKind::Warning,
            "controller.neutralization.not_delivered",
            Severity::Warning,
        )
        .with_resource(self.resource_id)
        .with_detail(error.to_string());
        if let Some(operation_id) = operation_id {
            event = event.with_operation(operation_id);
        }
        let _ = self.runtime.publish(event);
    }
}

fn map_transport_error(error: TransportError) -> EasyConError {
    let (domain, code) = match error.kind() {
        TransportErrorKind::Timeout => (ErrorDomain::Controller, ErrorCode::ProtocolTimeout),
        TransportErrorKind::WriteTimeout => (ErrorDomain::Io, ErrorCode::Transport),
        TransportErrorKind::Cancelled => (ErrorDomain::Runtime, ErrorCode::Cancelled),
        TransportErrorKind::Disconnected => (ErrorDomain::Io, ErrorCode::DeviceDisconnected),
        TransportErrorKind::Io => (ErrorDomain::Io, ErrorCode::Transport),
        TransportErrorKind::Protocol => (ErrorDomain::Controller, ErrorCode::ProtocolError),
    };
    EasyConError::new(domain, code, error.message())
}

fn invalid_automation_lease_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Controller,
        ErrorCode::InvalidArgument,
        "Automation lease is not valid for this controller generation",
    )
}

fn settle_owned_success(owner: OperationSettlementOwner) {
    finish_owned_candidate(
        owner,
        SettlementEvidence::EffectAccepted,
        TerminalCandidate::Success(OperationValue::Unit),
    );
}

/// Controller lanes must never block on a competing deferred Runtime transaction. A winner
/// already claimed elsewhere will finish through its own retained claim; this lane only finishes
/// the transaction it successfully claimed itself.
fn finish_owned_candidate(
    owner: OperationSettlementOwner,
    evidence: SettlementEvidence,
    candidate: TerminalCandidate,
) {
    if let ClaimOutcome::Claimed(claim) = owner.claim(evidence, candidate) {
        let _ = claim.finish(Ok(()));
    }
}

fn settle_report_cancellation(
    settlement_owner: Option<OperationSettlementOwner>,
    operation_settlement: Option<Arc<OperationSettlementRecord>>,
    operation: &Operation,
) {
    if let Some(owner) = settlement_owner {
        settle_owned_cancellation(owner, operation);
    } else if let Some(settlement) = operation_settlement {
        settlement.settle_cancellation(operation);
    }
}

fn settle_report_failure(
    settlement_owner: Option<OperationSettlementOwner>,
    operation_settlement: Option<Arc<OperationSettlementRecord>>,
    operation: &Operation,
    error: EasyConError,
) {
    if let Some(owner) = settlement_owner {
        settle_owned_failure(owner, operation, error);
    } else if let Some(settlement) = operation_settlement {
        settlement.settle_failure(operation, error);
    }
}

fn settle_report_strict_failure(
    settlement_owner: Option<OperationSettlementOwner>,
    operation_settlement: Option<Arc<OperationSettlementRecord>>,
    operation: &Operation,
    error: EasyConError,
) {
    if let Some(owner) = settlement_owner {
        settle_owned_strict_failure(owner, error);
    } else if let Some(settlement) = operation_settlement {
        settlement.settle_strict_failure(error);
    } else {
        let _ = operation.fail_strict_cleanup(error);
    }
}

fn settle_owned_cancellation(owner: OperationSettlementOwner, operation: &Operation) {
    if operation.snapshot().cancellation_reason.is_none() {
        let _ = operation.request_cancel(CancellationReason::ParentClose);
    }
    finish_owned_candidate(
        owner,
        SettlementEvidence::NotDelivered,
        TerminalCandidate::Cancellation,
    );
}

fn settle_owned_failure(
    owner: OperationSettlementOwner,
    operation: &Operation,
    error: EasyConError,
) {
    if operation.snapshot().cancellation_reason.is_some() {
        settle_owned_cancellation(owner, operation);
        return;
    }
    finish_owned_candidate(
        owner,
        SettlementEvidence::ExecutionFailed,
        TerminalCandidate::Failure(error),
    );
}

fn settle_owned_strict_failure(owner: OperationSettlementOwner, error: EasyConError) {
    if let ClaimOutcome::Claimed(claim) = owner.claim(
        SettlementEvidence::ExecutionFailed,
        TerminalCandidate::Failure(error.clone()),
    ) {
        let _ = claim.finish(Err(error));
    }
}

fn settle_owned_failure_or_cancellation(
    owner: OperationSettlementOwner,
    operation: &Operation,
    error: EasyConError,
) {
    if operation.snapshot().cancellation_reason.is_some() {
        settle_owned_cancellation(owner, operation);
    } else {
        settle_owned_failure(owner, operation, error);
    }
}

fn is_retryable_amiibo_error(error: &TransportError) -> bool {
    matches!(
        error.kind(),
        TransportErrorKind::Timeout | TransportErrorKind::Protocol
    )
}

fn cancelled_transport_error() -> TransportError {
    TransportError::new(
        TransportErrorKind::Cancelled,
        "Controller operation cancelled during ACK exchange",
    )
}

fn fail_closed_lane(operation: &Operation) {
    fail_or_finish_cancelled(operation, closed_lane_error());
}

fn fail_or_finish_cancelled(operation: &Operation, error: EasyConError) {
    if operation.fail(error) == TransitionOutcome::Invalid
        && operation.snapshot().state == OperationState::Cancelling
    {
        operation.finish_cancelled();
    }
}

fn fail_strict_cleanup(operation: &Operation, error: EasyConError) {
    let _ = operation.fail_strict_cleanup(error);
}

fn start_or_finish_cancelled(operation: &Operation) -> bool {
    match operation.start() {
        TransitionOutcome::Applied => true,
        TransitionOutcome::Invalid if operation.snapshot().state == OperationState::Cancelling => {
            operation.finish_cancelled();
            false
        }
        TransitionOutcome::Unchanged
        | TransitionOutcome::AlreadyTerminal
        | TransitionOutcome::Invalid
        | TransitionOutcome::CleanupFailed => false,
    }
}

fn closed_lane_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Controller,
        ErrorCode::DeviceDisconnected,
        "controller lane is closed",
    )
}

fn controller_ownership_lost_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Internal,
        ErrorCode::Internal,
        "controller lane ownership and transport settlement evidence are unavailable",
    )
}

fn resource_busy_error(message: &'static str) -> EasyConError {
    EasyConError::new(ErrorDomain::Controller, ErrorCode::ResourceBusy, message)
}

fn acquire_clock_panic_error() -> AutomationLeaseAcquireOutcome {
    AutomationLeaseAcquireOutcome::Failure(EasyConError::new(
        ErrorDomain::Internal,
        ErrorCode::Internal,
        "Runtime Clock panicked while resolving an Automation lease acquisition",
    ))
}

fn acquire_resolution_panic_error() -> AutomationLeaseAcquireOutcome {
    AutomationLeaseAcquireOutcome::Failure(EasyConError::new(
        ErrorDomain::Internal,
        ErrorCode::Internal,
        "Automation lease grant resolution panicked",
    ))
}

fn automation_admission_panic_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Internal,
        ErrorCode::Internal,
        "Automation lease action admission panicked",
    )
}

fn controller_clock_now(clock: &Arc<dyn Clock>) -> Result<u64, EasyConError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| clock.now_ns())) {
        Ok(timestamp_ns) => Ok(timestamp_ns),
        Err(payload) => {
            // A Controller admission has not committed any lane-visible work yet, so a Clock
            // panic becomes a typed admission failure instead of poisoning a gate.
            std::mem::forget(payload);
            Err(EasyConError::new(
                ErrorDomain::Internal,
                ErrorCode::Internal,
                "Runtime Clock panicked while admitting a Controller action",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier, Mutex, mpsc};
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::Duration;

    use easycon_model::{Button, EasyConError, ErrorCode, ErrorDomain};
    use easycon_runtime::{
        CancellationReason, CancellationToken, Clock, ClockChangeRegistration, CloseOutcome,
        DeadlineId, Operation, OperationState, OperationValue, Runtime, SettlementEvidence,
        SettlementOwnerMode, TerminalCandidate, TransitionOutcome, VirtualClock, WaitResult,
        WaitTimeout,
    };

    use crate::AckFrame;

    use super::{
        AckRequest, AcquireRecord, AcquireStatus, AutomationLeaseAcquireOutcome,
        AutomationLeaseReleaseOutcome, ConnectOptions, ControllerAction, ControllerCleanupFailure,
        ControllerLeaseState, ControllerOptions, ControllerSession, ControllerTransport,
        HandshakeRequest, LeaseAccess, LeaseGeneration, LeaseGenerationGate,
        OperationSettlementRecord, OperationSettlementState, ReleaseInterruptRegistry,
        ReleasePhase, ReleaseRecord, TransportError, TransportErrorKind, WriteRequest,
        WriteSettlement, WriteSettlementOutcome,
    };

    const CHANNEL_WAIT: Duration = Duration::from_secs(2);

    struct ChannelWake(mpsc::SyncSender<()>);

    impl Wake for ChannelWake {
        fn wake(self: Arc<Self>) {
            let _ = self.0.try_send(());
        }

        fn wake_by_ref(self: &Arc<Self>) {
            let _ = self.0.try_send(());
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        try_block_on(future).expect("future did not wake before the bounded test deadline")
    }

    fn try_block_on<F: Future>(future: F) -> Result<F::Output, ()> {
        let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
        let waker = Waker::from(Arc::new(ChannelWake(wake_sender)));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return Ok(output),
                Poll::Pending => wake_receiver.recv_timeout(CHANNEL_WAIT).map_err(|_| ())?,
            }
        }
    }

    fn wait_terminal(operation: &Operation) {
        assert!(matches!(
            operation.wait(WaitTimeout::For(CHANNEL_WAIT)),
            WaitResult::Completed(_)
        ));
    }

    struct ProbeClock {
        inner: Arc<VirtualClock>,
        action: Mutex<Option<ProbeClockAction>>,
        controller_lane: Arc<Mutex<Option<std::thread::ThreadId>>>,
    }

    struct ProbeClockAction {
        predicate: Arc<dyn Fn() -> bool + Send + Sync>,
        action: Box<dyn FnOnce() + Send>,
    }

    impl ProbeClock {
        fn new() -> Self {
            Self {
                inner: Arc::new(VirtualClock::default()),
                action: Mutex::new(None),
                controller_lane: Arc::new(Mutex::new(None)),
            }
        }

        fn arm(&self, action: impl FnOnce() + Send + 'static) {
            self.arm_when(|| true, action);
        }

        fn arm_when(
            &self,
            predicate: impl Fn() -> bool + Send + Sync + 'static,
            action: impl FnOnce() + Send + 'static,
        ) {
            *self.action.lock().expect("probe clock action lock") = Some(ProbeClockAction {
                predicate: Arc::new(predicate),
                action: Box::new(action),
            });
        }

        fn is_controller_lane(&self) -> bool {
            self.controller_lane
                .lock()
                .expect("probe controller lane lock")
                .is_some_and(|thread| thread == std::thread::current().id())
        }

        fn advance_to(&self, target_ns: u64) {
            self.inner.advance_to(target_ns);
        }
    }

    impl Clock for ProbeClock {
        fn now_ns(&self) -> u64 {
            let predicate = self
                .action
                .lock()
                .expect("probe clock action lock")
                .as_ref()
                .map(|armed| Arc::clone(&armed.predicate));
            let action = if predicate.is_some_and(|predicate| predicate()) {
                self.action
                    .lock()
                    .expect("probe clock action lock")
                    .take()
                    .map(|armed| armed.action)
            } else {
                None
            };
            if let Some(action) = action {
                action();
            }
            self.inner.now_ns()
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.inner.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> DeadlineId {
            self.inner.register_deadline(target_ns)
        }

        fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
            self.inner.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.inner.real_wait_duration(target_ns)
        }
    }

    struct ImmediateTransport {
        controller_lane: Arc<Mutex<Option<std::thread::ThreadId>>>,
    }

    impl ControllerTransport for ImmediateTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            *self
                .controller_lane
                .lock()
                .expect("probe controller lane lock") = Some(std::thread::current().id());
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            if !request
                .settlement
                .full_accepted_at(request.context.timestamp_ns)
            {
                return Err(TransportError::new(
                    TransportErrorKind::Io,
                    "test transport rejected final-byte settlement",
                ));
            }
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "test transport has no ACK script",
            ))
        }

        fn close(&mut self) {}
    }

    fn connected_probe_controller() -> (Arc<ProbeClock>, Runtime, ControllerSession) {
        let clock = Arc::new(ProbeClock::new());
        let runtime = Runtime::new(clock.clone());
        let controller = ControllerSession::new(
            &runtime,
            Box::new(ImmediateTransport {
                controller_lane: Arc::clone(&clock.controller_lane),
            }),
            ControllerOptions {
                minimum_report_interval_ns: 10,
                ..ControllerOptions::default()
            },
        )
        .expect("controller");
        let connect = controller
            .connect(ConnectOptions::default())
            .expect("connect operation");
        wait_terminal(&connect);
        assert_eq!(connect.snapshot().state, OperationState::Succeeded);
        (clock, runtime, controller)
    }

    // conformance: controller.lease.close-record-transition
    #[test]
    fn release_completion_consumes_close_requested_before_the_backend_gate() {
        let record = Arc::new(ReleaseRecord::new());
        let (_cancellation, close_owned) = record.begin_neutral();
        assert!(!close_owned);

        // Close gets the record before the backend reaches physical full acceptance. That
        // ordering consumes this same neutral instead of requiring an independent final close.
        let barrier = Arc::new(Barrier::new(2));
        let close_record = record.clone();
        let close_barrier = barrier.clone();
        let (close_sender, close_receiver) = mpsc::sync_channel(1);
        let close_thread = std::thread::spawn(move || {
            close_barrier.wait();
            assert!(close_record.request_close_interrupt());
            close_sender.send(()).expect("close observer remains alive");
        });
        barrier.wait();
        close_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("close intent registered before completion");

        assert!(record.complete(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)));
        close_thread.join().expect("close thread");
        let state = record.state.lock().expect("release record lock");
        assert!(matches!(
            state.phase,
            ReleasePhase::Committed {
                close_consumed: true,
                outcome: Ok(AutomationLeaseReleaseOutcome::NeutralAccepted),
            }
        ));
    }

    // conformance: controller.lease.close-all-records-traversed
    #[test]
    fn registry_close_transitions_every_record_after_a_prior_record_was_consumed() {
        let registry = ReleaseInterruptRegistry::default();
        let prior = Arc::new(ReleaseRecord::new());
        let live = Arc::new(ReleaseRecord::new());
        registry.track(&prior);
        registry.track(&live);

        assert!(!prior.complete(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)));
        assert!(registry.request_close_interrupts());

        let prior_state = prior.state.lock().expect("prior release record lock");
        assert!(matches!(
            prior_state.phase,
            ReleasePhase::Committed {
                close_consumed: false,
                outcome: Ok(AutomationLeaseReleaseOutcome::NeutralAccepted),
            }
        ));
        drop(prior_state);

        let live_state = live.state.lock().expect("live release record lock");
        assert!(matches!(
            live_state.phase,
            ReleasePhase::Undispatched {
                close_acquired: true,
            }
        ));
    }

    #[test]
    fn ownership_loss_keeps_release_registry_nonterminal_after_close_observation() {
        let registry = ReleaseInterruptRegistry::default();
        let record = Arc::new(ReleaseRecord::new());
        registry.track(&record);

        registry.record_ownership_loss();
        registry.settle_stream_after_close();

        let record_state = record.state.lock().expect("release record lock");
        assert!(matches!(
            record_state.phase,
            ReleasePhase::Undispatched {
                close_acquired: false,
            }
        ));
        drop(record_state);

        let registry_state = registry.state.lock().expect("release registry lock");
        assert!(matches!(
            registry_state.cleanup_projection.as_ref(),
            Some(super::ReleaseCleanupProjection::OwnershipLost)
        ));
        assert_eq!(registry_state.retained_ownership_loss_records.len(), 1);
    }

    // conformance: controller.report.gate-claim-strict-cleanup
    #[test]
    fn strict_failure_while_full_claim_is_in_flight_finishes_the_same_claim() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (operation, owner) = runtime
            .create_operation_with_settlement_owner(
                None,
                SettlementOwnerMode::Transferable,
                || Ok(()),
                |_| {},
            )
            .expect("owned operation");
        assert_eq!(operation.start(), TransitionOutcome::Applied);
        let record = OperationSettlementRecord::new(owner);
        let owner = {
            let mut state = record.state.lock().expect("settlement record lock");
            match std::mem::replace(
                &mut *state,
                OperationSettlementState::ClaimingFull {
                    strict_failure: None,
                },
            ) {
                OperationSettlementState::Pending(owner) => owner,
                _ => panic!("fresh record must retain its pending owner"),
            }
        };
        let error = EasyConError::new(
            ErrorDomain::Io,
            ErrorCode::Transport,
            "scripted strict cleanup failure",
        );
        record.settle_strict_failure(error.clone());
        let outcome = owner.claim(
            SettlementEvidence::EffectAccepted,
            TerminalCandidate::Success(OperationValue::Unit),
        );
        assert!(!record.complete_full_claim(outcome));
        let snapshot = operation.snapshot();
        assert_eq!(snapshot.state, OperationState::Failed);
        assert_eq!(snapshot.error, Some(error));
        assert!(record.is_finished());
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn full_acceptance_reserves_an_action_before_the_runtime_claim_hook_runs() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (operation, owner) = runtime
            .create_operation_with_settlement_owner(
                None,
                SettlementOwnerMode::Transferable,
                || Ok(()),
                |_| {},
            )
            .expect("owned operation");
        assert_eq!(operation.start(), TransitionOutcome::Applied);
        let record = Arc::new(OperationSettlementRecord::new(owner));
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let (resume_sender, resume_receiver) = mpsc::sync_channel(1);
        let reservation_record = Arc::clone(&record);
        let hook_record = Arc::clone(&record);
        let settlement = WriteSettlement::tracked_with_full_reservation(
            Some(Box::new(move || reservation_record.reserve_full_accepted())),
            Some(Box::new(move |outcome| {
                if outcome == WriteSettlementOutcome::FullAccepted {
                    entered_sender
                        .send(())
                        .expect("test observes the full-acceptance hook");
                    resume_receiver
                        .recv_timeout(CHANNEL_WAIT)
                        .expect("test releases the full-acceptance hook");
                    let _ = hook_record.claim_reserved_full_accepted();
                }
            })),
        );
        assert!(settlement.mark_dispatched());

        let completion = settlement.clone();
        let (completed_sender, completed_receiver) = mpsc::sync_channel(1);
        let completion_thread = std::thread::spawn(move || {
            assert!(completion.full_accepted_at(17));
            completed_sender
                .send(())
                .expect("full-acceptance completion observer remains alive");
        });
        entered_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("physical full acceptance reaches the hook barrier");

        let _ = operation.request_cancel(CancellationReason::ParentClose);
        record.settle_cancellation(&operation);

        resume_sender
            .send(())
            .expect("full-acceptance hook remains available");
        completed_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("full-acceptance completion returns after the hook");
        completion_thread.join().expect("full-acceptance thread");

        assert!(
            record.finish_success(),
            "a physical final-byte acceptance must retain the action owner through the hook"
        );
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn final_acceptance_can_claim_an_action_after_an_isolated_hook_panic() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (operation, owner) = runtime
            .create_operation_with_settlement_owner(
                None,
                SettlementOwnerMode::Transferable,
                || Ok(()),
                |_| {},
            )
            .expect("owned operation");
        assert_eq!(operation.start(), TransitionOutcome::Applied);
        let record = Arc::new(OperationSettlementRecord::new(owner));

        // The physical-acceptance reservation intentionally recovers the record lock. This
        // simulates an isolated observer fault before the lane performs its post-publish claim.
        let poisoned_record = Arc::clone(&record);
        let poisoner = std::thread::spawn(move || {
            let _state = poisoned_record
                .state
                .lock()
                .expect("settlement record poison setup lock");
            panic!("scripted observer panic poisons the action record");
        });
        assert!(poisoner.join().is_err(), "the scripted observer must panic");

        let reservation_record = Arc::clone(&record);
        let settlement = WriteSettlement::tracked_with_full_reservation(
            Some(Box::new(move || reservation_record.reserve_full_accepted())),
            Some(Box::new(|outcome| {
                if outcome == WriteSettlementOutcome::FullAccepted {
                    panic!("scripted final-acceptance observer panic");
                }
            })),
        );
        assert!(settlement.mark_dispatched());
        assert!(settlement.full_accepted_at(7));

        assert!(
            record.claim_reserved_full_accepted(),
            "the lane must recover the reserved owner after an isolated observer panic"
        );
        assert!(record.finish_success());
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn full_acceptance_reserves_a_release_before_close_observes_the_hook_window() {
        let record = Arc::new(ReleaseRecord::new());
        let (_cancellation, close_owned) = record.begin_neutral();
        assert!(!close_owned);
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let (resume_sender, resume_receiver) = mpsc::sync_channel(1);
        let reservation_record = Arc::clone(&record);
        let settlement = WriteSettlement::tracked_with_full_reservation(
            Some(Box::new(move || reservation_record.reserve_full_accepted())),
            Some(Box::new(move |outcome| {
                if outcome == WriteSettlementOutcome::FullAccepted {
                    entered_sender
                        .send(())
                        .expect("test observes the release full-acceptance hook");
                    resume_receiver
                        .recv_timeout(CHANNEL_WAIT)
                        .expect("test releases the release full-acceptance hook");
                }
            })),
        );
        assert!(settlement.mark_dispatched());

        let completion = settlement.clone();
        let (completed_sender, completed_receiver) = mpsc::sync_channel(1);
        let completion_thread = std::thread::spawn(move || {
            assert!(completion.full_accepted_at(19));
            completed_sender
                .send(())
                .expect("release full-acceptance completion observer remains alive");
        });
        entered_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("physical release acceptance reaches the hook barrier");

        let close_consumed = record.request_close_interrupt();

        resume_sender
            .send(())
            .expect("release full-acceptance hook remains available");
        completed_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("release full-acceptance completion returns after the hook");
        completion_thread
            .join()
            .expect("release full-acceptance thread");

        assert!(
            !close_consumed,
            "a close after physical full acceptance needs its own final neutral"
        );
        assert!(
            !record.complete(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)),
            "the accepted release cannot consume a later close"
        );
    }

    #[test]
    fn acquire_grant_clock_reentry_never_observes_the_record_mutex_held() {
        let (clock, runtime, controller) = connected_probe_controller();
        let cancellation = CancellationToken::root();
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let (resume_sender, resume_receiver) = mpsc::sync_channel(1);
        let predicate_clock = Arc::clone(&clock);
        let predicate_controller = controller.clone();
        clock.arm_when(
            move || {
                predicate_clock.is_controller_lane()
                    && matches!(
                        predicate_controller.snapshot().lease,
                        ControllerLeaseState::Automation(_)
                    )
            },
            move || {
                entered_sender
                    .send(())
                    .expect("grant Clock callback observer remains alive");
                resume_receiver
                    .recv_timeout(CHANNEL_WAIT)
                    .expect("grant Clock callback remains bounded");
            },
        );

        let acquire = controller.acquire_automation_lease(&cancellation, None);
        let (outcome_sender, outcome_receiver) = mpsc::sync_channel(1);
        let acquire_thread = std::thread::spawn(move || {
            outcome_sender
                .send(block_on(acquire))
                .expect("acquire outcome observer remains alive");
        });
        entered_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("grant Clock callback reaches the deterministic barrier");

        let record = controller
            .inner
            .pending_acquires
            .lock()
            .expect("pending acquire probe lock")
            .iter()
            .find_map(std::sync::Weak::upgrade);
        let record_lock_free = record
            .as_ref()
            .is_some_and(|record| record.state.try_lock().is_ok());
        if record_lock_free {
            cancellation.cancel();
        }
        resume_sender
            .send(())
            .expect("grant Clock callback remains paused until cancellation reentry completes");
        let lease = match outcome_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("acquire future settles after the Clock callback resumes")
        {
            AutomationLeaseAcquireOutcome::Granted(lease) => lease,
            AutomationLeaseAcquireOutcome::Cancelled => panic!("grant was cancelled"),
            AutomationLeaseAcquireOutcome::Deadline => panic!("grant reached deadline"),
            AutomationLeaseAcquireOutcome::Closed => panic!("controller closed before grant"),
            AutomationLeaseAcquireOutcome::Failure(error) => {
                panic!("grant failed after unlocked Clock reentry: {error}")
            }
        };
        acquire_thread.join().expect("acquire thread");
        let release = lease.neutralize_and_release();
        clock.advance_to(100);
        assert!(matches!(
            block_on(release),
            Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
        ));
        // A release that settled before resource close leaves the independent final neutral
        // paced from its acceptance at 100.
        clock.advance_to(110);
        controller.close();
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        assert!(
            record.is_some(),
            "active acquire record remains registered at the Clock barrier"
        );
        assert!(
            record_lock_free,
            "Clock reentry must not observe an AcquireRecord mutex held by grant resolution"
        );
    }

    #[test]
    fn acquire_clock_reentry_cancellation_wins_before_grant_reservation() {
        let (clock, runtime, controller) = connected_probe_controller();
        let cancellation = CancellationToken::root();
        let reentrant_cancellation = cancellation.clone();
        clock.arm(move || reentrant_cancellation.cancel());

        assert!(matches!(
            block_on(controller.acquire_automation_lease(&cancellation, Some(100))),
            AutomationLeaseAcquireOutcome::Cancelled
        ));

        controller.close();
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn acquire_grant_clock_panic_isolated_without_poisoning_close() {
        let (clock, runtime, controller) = connected_probe_controller();
        let cancellation = CancellationToken::root();
        let predicate_clock = Arc::clone(&clock);
        let predicate_controller = controller.clone();
        let fired = Arc::new(AtomicBool::new(false));
        let observed_fired = Arc::clone(&fired);
        clock.arm_when(
            move || {
                predicate_clock.is_controller_lane()
                    && matches!(
                        predicate_controller.snapshot().lease,
                        ControllerLeaseState::Automation(_)
                    )
            },
            move || {
                observed_fired.store(true, Ordering::Release);
                panic!("scripted Automation grant Clock panic");
            },
        );

        let acquire = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            block_on(controller.acquire_automation_lease(&cancellation, None))
        }));
        let lease = match acquire {
            Ok(AutomationLeaseAcquireOutcome::Granted(lease)) => lease,
            Ok(AutomationLeaseAcquireOutcome::Cancelled) => panic!("grant was cancelled"),
            Ok(AutomationLeaseAcquireOutcome::Deadline) => panic!("grant reached deadline"),
            Ok(AutomationLeaseAcquireOutcome::Closed) => panic!("controller closed before grant"),
            Ok(AutomationLeaseAcquireOutcome::Failure(error)) => {
                panic!("grant failed after isolated Clock panic: {error}")
            }
            Err(_) => panic!("Clock panic escaped Automation grant resolution"),
        };
        assert!(
            fired.load(Ordering::Acquire),
            "the grant-state publication must consume the targeted Clock panic"
        );
        let release = lease.neutralize_and_release();
        clock.advance_to(100);
        assert!(matches!(
            block_on(release),
            Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
        ));
        clock.advance_to(110);
        controller.close();
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn automation_admission_clock_panic_does_not_poison_close() {
        let (clock, runtime, controller) = connected_probe_controller();
        let cancellation = CancellationToken::root();
        let lease = std::mem::ManuallyDrop::new(
            match block_on(controller.acquire_automation_lease(&cancellation, None)) {
                AutomationLeaseAcquireOutcome::Granted(lease) => lease,
                AutomationLeaseAcquireOutcome::Cancelled => panic!("test acquire was cancelled"),
                AutomationLeaseAcquireOutcome::Deadline => {
                    panic!("test acquire reached its deadline")
                }
                AutomationLeaseAcquireOutcome::Closed => panic!("test acquire was closed"),
                AutomationLeaseAcquireOutcome::Failure(error) => {
                    panic!("test acquire failed: {error}")
                }
            },
        );
        clock.arm(|| {
            panic!("scripted Automation admission Clock panic");
        });

        let direct = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            controller.direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        }));
        let close = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| controller.close()));
        let runtime_close =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.close()));
        if direct.is_ok() {
            drop(std::mem::ManuallyDrop::into_inner(lease));
        }

        assert!(
            direct.is_ok(),
            "Clock panic must not unwind through Automation admission"
        );
        assert!(
            close.is_ok(),
            "Controller close must recover after Clock panic"
        );
        assert!(
            runtime_close.is_ok(),
            "Runtime close must remain bounded after Automation Clock panic"
        );
    }

    #[test]
    fn automation_admission_clock_reentry_does_not_hold_generation_or_admission_gates() {
        let (clock, runtime, controller) = connected_probe_controller();
        let cancellation = CancellationToken::root();
        let lease = match block_on(controller.acquire_automation_lease(&cancellation, None)) {
            AutomationLeaseAcquireOutcome::Granted(lease) => lease,
            AutomationLeaseAcquireOutcome::Cancelled => panic!("test acquire was cancelled"),
            AutomationLeaseAcquireOutcome::Deadline => panic!("test acquire reached its deadline"),
            AutomationLeaseAcquireOutcome::Closed => panic!("test acquire was closed"),
            AutomationLeaseAcquireOutcome::Failure(error) => panic!("test acquire failed: {error}"),
        };
        let nested_controller = controller.clone();
        let nested_generation = lease.generation.clone();
        let (nested_sender, nested_receiver) = mpsc::sync_channel(1);
        clock.arm(move || {
            let same_generation = nested_controller.submit_direct(
                ControllerAction::ButtonDown(Button::B),
                LeaseAccess::Automation(nested_generation),
            );
            let ordinary = nested_controller.direct(ControllerAction::ButtonDown(Button::X));
            nested_sender
                .send((same_generation, ordinary))
                .expect("Clock reentry observer remains alive");
        });

        let outer = controller
            .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
            .expect("outer Automation direct admission");
        let (same_generation, ordinary) = nested_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("Clock reentry completes without holding an admission gate");
        let same_generation = same_generation.expect("same-generation reentry admission");
        let ordinary = ordinary.expect("ordinary reentry admission");
        clock.advance_to(100);
        let same_wait = same_generation.wait(WaitTimeout::For(CHANNEL_WAIT));
        clock.advance_to(110);
        let outer_wait = outer.wait(WaitTimeout::For(CHANNEL_WAIT));
        let ordinary_wait = ordinary.wait(WaitTimeout::For(CHANNEL_WAIT));
        let release = lease.neutralize_and_release();
        clock.advance_to(120);
        let release_wait = try_block_on(release);
        assert!(matches!(same_wait, WaitResult::Completed(_)));
        assert!(matches!(outer_wait, WaitResult::Completed(_)));
        assert!(matches!(ordinary_wait, WaitResult::Completed(_)));
        assert!(matches!(
            release_wait,
            Ok(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted))
        ));
        let closing_controller = controller.clone();
        let (close_sender, close_receiver) = mpsc::sync_channel(1);
        let close_thread = std::thread::spawn(move || {
            closing_controller.close();
            close_sender.send(()).expect("close observer remains alive");
        });
        clock.advance_to(130);
        close_receiver
            .recv_timeout(CHANNEL_WAIT)
            .expect("Controller close remains bounded after Clock reentry");
        close_thread.join().expect("Controller close thread");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    fn resolve_acquire_at_same_observation_point(
        cancelled: bool,
        deadline_reached: bool,
        closing: bool,
    ) -> AutomationLeaseAcquireOutcome {
        let clock = Arc::new(VirtualClock::default());
        if deadline_reached {
            clock.advance_to(1);
        }
        let cancellation = CancellationToken::root();
        if cancelled {
            cancellation.cancel();
        }
        let closing = Arc::new(AtomicBool::new(closing));
        let record = AcquireRecord::new(cancellation, Some(1), clock, closing, None);
        assert!(record.resolve_with(|| {
            AutomationLeaseAcquireOutcome::Failure(super::resource_busy_error("test failure"))
        }));
        let mut state = record.state.lock().expect("lease acquire record lock");
        let AcquireStatus::Ready(outcome) = &mut state.status else {
            panic!("resolution must publish exactly one outcome");
        };
        outcome
            .take()
            .expect("outcome is consumed once by this test")
    }

    // conformance: controller.lease.acquire-same-point-priority
    #[test]
    fn acquire_record_uses_stable_same_point_priority() {
        assert!(matches!(
            resolve_acquire_at_same_observation_point(true, true, true),
            AutomationLeaseAcquireOutcome::Cancelled
        ));
        assert!(matches!(
            resolve_acquire_at_same_observation_point(false, true, true),
            AutomationLeaseAcquireOutcome::Deadline
        ));
        assert!(matches!(
            resolve_acquire_at_same_observation_point(false, false, true),
            AutomationLeaseAcquireOutcome::Closed
        ));
        assert!(matches!(
            resolve_acquire_at_same_observation_point(false, false, false),
            AutomationLeaseAcquireOutcome::Failure(_)
        ));
    }

    // conformance: controller.lease.acquire-abandonment
    #[test]
    fn abandoned_acquire_record_rejects_a_late_grant_transition() {
        let record = AcquireRecord::new(
            CancellationToken::root(),
            None,
            Arc::new(VirtualClock::default()),
            Arc::new(AtomicBool::new(false)),
            None,
        );
        record.abandon();
        assert!(!record.resolve_with(|| {
            panic!("an abandoned acquire must not invoke a late grant transition")
        }));
        assert!(matches!(
            record
                .state
                .lock()
                .expect("lease acquire record lock")
                .status,
            AcquireStatus::Abandoned
        ));
    }

    // conformance: controller.lease.generation-stale-sealed
    #[test]
    fn sealed_generation_rejects_stale_action_admission() {
        let (sender, receiver) = mpsc::channel();
        let generation = Arc::new(LeaseGeneration {
            id: 7,
            sender,
            release_interrupts: Arc::new(ReleaseInterruptRegistry::default()),
            permanent_failure: Arc::new(ControllerCleanupFailure::default()),
            gate: std::sync::Mutex::new(LeaseGenerationGate::Open {
                actions: Vec::new(),
                reservations: 0,
            }),
            cleanup_failure: std::sync::Mutex::new(None),
        });
        let record = generation.request_cleanup();
        let error = match generation
            .admit(|| panic!("a sealed generation must reject before submitting an action"))
        {
            Err(error) => error,
            Ok(_) => panic!("stale generation action must be rejected"),
        };
        assert_eq!(error.domain(), easycon_model::ErrorDomain::Controller);
        assert_eq!(error.code(), easycon_model::ErrorCode::InvalidArgument);
        drop(record);
        drop(receiver);
    }
}
