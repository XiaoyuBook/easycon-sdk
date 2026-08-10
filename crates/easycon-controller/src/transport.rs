use std::fmt;
use std::sync::{Arc, Mutex};

use easycon_model::{EasyConError, OperationId, ResourceId};
use easycon_runtime::CancellationToken;

/// Source-exact automatic connection order.
pub const AUTO_BAUD_RATES: [u32; 2] = [115_200, 9_600];
/// Source-exact hello request.
pub const HANDSHAKE_REQUEST: [u8; 3] = [0xA5, 0xA5, 0x81];
/// Source-exact successful hello reply.
pub const HANDSHAKE_REPLY: u8 = 0x80;

/// One bounded handshake attempt.
pub struct HandshakeRequest {
    /// Related connect operation.
    pub operation_id: OperationId,
    /// Attempted baud rate.
    pub baud_rate: u32,
    /// Source-exact hello request bytes.
    pub request_bytes: [u8; 3],
    /// Source-exact expected reply byte.
    pub expected_reply: u8,
    /// Absolute protocol timeout on the Runtime clock.
    pub deadline_ns: u64,
    /// Cancellation propagated from the operation and Runtime root.
    pub cancellation: CancellationToken,
    /// Resource cancellation used to interrupt close while the lane is blocked.
    pub resource_cancellation: CancellationToken,
}

/// One generation-tagged protocol reply byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AckFrame {
    /// Request generation assigned by the single matcher.
    pub generation: u64,
    /// Reply byte.
    pub byte: u8,
}

/// One bounded generation-aware ACK wait.
pub struct AckRequest {
    /// Related operation.
    pub operation_id: OperationId,
    /// Current request generation.
    pub generation: u64,
    /// Required reply byte.
    pub expected_reply: u8,
    /// Absolute protocol deadline.
    pub deadline_ns: u64,
    /// Operation cancellation.
    pub cancellation: CancellationToken,
    /// Resource close cancellation.
    pub resource_cancellation: CancellationToken,
}

/// Semantic purpose of a transport write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteKind {
    /// Normal desired-state report.
    Report,
    /// Safety report sent during cancellation or close.
    Neutralize,
    /// Request/response command bytes.
    Command,
}

/// Controller-side monotonic stages captured before one direct report enters transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectWriteTiming {
    /// Time sampled immediately before the command is enqueued to the single-writer lane.
    pub command_admitted_ns: u64,
    /// Time sampled when the lane receives and begins dispatching the direct command.
    pub lane_wake_ns: u64,
}

/// Stable context repeated across partial writes for one logical payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteContext {
    /// Controller resource that owns the single writer.
    pub resource_id: ResourceId,
    /// Related operation, absent for final resource close cleanup.
    pub operation_id: Option<OperationId>,
    /// Strictly increasing logical write sequence.
    pub sequence: u64,
    /// Runtime-clock timestamp chosen for this logical dispatch.
    pub timestamp_ns: u64,
    /// Admission and lane-wake stages for a direct report; absent for other write kinds.
    pub direct_timing: Option<DirectWriteTiming>,
    /// Complete logical payload length.
    pub total_len: usize,
    /// Write purpose.
    pub kind: WriteKind,
}

/// The outcome consumed by one logical report's backend settlement gate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WriteSettlementOutcome {
    /// The backend accepted the complete logical report.
    FullAccepted,
    /// The backend did not accept the report and its stream is settled.
    NotDeliveredStreamSettled(EasyConError),
    /// Stream cleanup itself failed after the backend completion was consumed.
    CleanupFailed(EasyConError),
}

type WriteSettlementHook = Box<dyn FnOnce(WriteSettlementOutcome) + Send + 'static>;
// This reservation is intentionally narrower than a hook: it may only move the owning
// Controller record into its local full-acceptance state. It runs while the settlement gate is
// locked so cancellation/close cannot claim the record between physical acceptance and the
// deferred Runtime/release work. It must not call Runtime, Clock, publish, wake, or user code.
type WriteSettlementFullReservation = Box<dyn FnOnce() -> bool + Send + 'static>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteSettlementPhase {
    NotDispatched,
    Outstanding,
    FullAcceptedReserved,
    FullAcceptanceRejected,
    SettlingNonFull,
    Settled,
}

struct WriteSettlementState {
    phase: WriteSettlementPhase,
    outcome: Option<WriteSettlementOutcome>,
    accepted_at_ns: Option<u64>,
    full_acceptance_reservation: Option<WriteSettlementFullReservation>,
    hook: Option<WriteSettlementHook>,
}

/// One logical report's backend settlement and acceptance gate.
///
/// The Controller creates a tracked handle before dispatch. A concrete backend must claim
/// `FullAccepted` while it consumes the completion that contains the final byte; a late caller
/// cancellation cannot replace that result after the claim.
#[derive(Clone)]
pub struct WriteSettlement {
    state: Arc<Mutex<WriteSettlementState>>,
}

impl WriteSettlement {
    /// Creates an untracked handle for direct adapter contract tests.
    #[must_use]
    pub fn untracked() -> Self {
        Self {
            state: Arc::new(Mutex::new(WriteSettlementState {
                phase: WriteSettlementPhase::Outstanding,
                outcome: None,
                accepted_at_ns: None,
                full_acceptance_reservation: None,
                hook: None,
            })),
        }
    }

    pub(crate) fn tracked(hook: Option<WriteSettlementHook>) -> Self {
        Self::tracked_with_full_reservation(None, hook)
    }

    pub(crate) fn tracked_with_full_reservation(
        full_acceptance_reservation: Option<WriteSettlementFullReservation>,
        hook: Option<WriteSettlementHook>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(WriteSettlementState {
                phase: WriteSettlementPhase::NotDispatched,
                outcome: None,
                accepted_at_ns: None,
                full_acceptance_reservation,
                hook,
            })),
        }
    }

    pub(crate) fn mark_dispatched(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("write settlement gate lock poisoned");
        if state.phase != WriteSettlementPhase::NotDispatched {
            return false;
        }
        state.phase = WriteSettlementPhase::Outstanding;
        true
    }

    /// Reserves complete backend acceptance at the final-byte completion boundary.
    ///
    /// This short transaction is intentionally free of Clock, Runtime, user callbacks, wakeups,
    /// and telemetry. Backends call it while their unique physical completion owner still holds the
    /// final byte count; cancellation and resource close may then record intent but cannot take
    /// the logical-report owner. Call [`Self::publish_reserved_full_acceptance`] only after every
    /// backend and settlement lock has been released.
    pub fn reserve_full_acceptance(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("write settlement gate lock poisoned");
        if state.phase != WriteSettlementPhase::Outstanding {
            return false;
        }
        if let Some(reservation) = state.full_acceptance_reservation.take()
            && !reservation()
        {
            // A failed typed reservation is a physical-completion contract failure, not a second
            // chance for a duplicate callback to manufacture FullAccepted.
            state.phase = WriteSettlementPhase::FullAcceptanceRejected;
            return false;
        }
        state.phase = WriteSettlementPhase::FullAcceptedReserved;
        true
    }

    /// Publishes a previously reserved physical final-byte acceptance.
    ///
    /// The terminal settlement outcome is committed before the optional hook is invoked. The hook
    /// therefore runs outside the settlement mutex and cannot make a duplicate/reentrant publish
    /// replace the already frozen acceptance.
    pub fn publish_reserved_full_acceptance(&self, accepted_at_ns: u64) -> bool {
        let hook = {
            let mut state = self
                .state
                .lock()
                .expect("write settlement gate lock poisoned");
            if state.phase != WriteSettlementPhase::FullAcceptedReserved {
                return false;
            }
            state.accepted_at_ns = Some(accepted_at_ns);
            state.phase = WriteSettlementPhase::Settled;
            state.outcome = Some(WriteSettlementOutcome::FullAccepted);
            state.hook.take()
        };
        Self::invoke_hook(hook, WriteSettlementOutcome::FullAccepted);
        true
    }

    /// Claims and publishes complete backend acceptance at the final-byte completion boundary.
    ///
    /// Direct synthetic transports may use this convenience method only when no public work is
    /// observable between their physical completion and publication. Byte-I/O backends reserve
    /// first and publish after sampling their Clock outside backend locks.
    pub fn full_accepted_at(&self, accepted_at_ns: u64) -> bool {
        self.reserve_full_acceptance() && self.publish_reserved_full_acceptance(accepted_at_ns)
    }

    pub(crate) fn begin_non_full(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .expect("write settlement gate lock poisoned");
        if !matches!(
            state.phase,
            WriteSettlementPhase::Outstanding | WriteSettlementPhase::FullAcceptanceRejected
        ) {
            return false;
        }
        state.phase = WriteSettlementPhase::SettlingNonFull;
        true
    }

    pub(crate) fn not_delivered_stream_settled(&self, error: EasyConError) -> bool {
        self.complete(
            WriteSettlementOutcome::NotDeliveredStreamSettled(error),
            WriteSettlementPhase::SettlingNonFull,
            None,
        )
    }

    pub(crate) fn cleanup_failed(&self, error: EasyConError) -> bool {
        self.complete(
            WriteSettlementOutcome::CleanupFailed(error),
            WriteSettlementPhase::SettlingNonFull,
            None,
        )
    }

    #[must_use]
    pub fn is_full_accepted(&self) -> bool {
        let state = self
            .state
            .lock()
            .expect("write settlement gate lock poisoned");
        state.phase == WriteSettlementPhase::FullAcceptedReserved
            || matches!(&state.outcome, Some(WriteSettlementOutcome::FullAccepted))
    }

    pub(crate) fn full_accepted_outcome(&self) -> bool {
        self.is_full_accepted()
    }

    #[must_use]
    pub(crate) fn accepted_at_ns(&self) -> Option<u64> {
        self.state
            .lock()
            .expect("write settlement gate lock poisoned")
            .accepted_at_ns
    }

    #[must_use]
    pub fn same_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    fn complete(
        &self,
        outcome: WriteSettlementOutcome,
        expected: WriteSettlementPhase,
        accepted_at_ns: Option<u64>,
    ) -> bool {
        let hook = {
            let mut state = self
                .state
                .lock()
                .expect("write settlement gate lock poisoned");
            if state.phase != expected {
                return false;
            }
            state.phase = WriteSettlementPhase::Settled;
            state.outcome = Some(outcome.clone());
            state.accepted_at_ns = accepted_at_ns;
            state.hook.take()
        };
        Self::invoke_hook(hook, outcome);
        true
    }

    fn invoke_hook(hook: Option<WriteSettlementHook>, outcome: WriteSettlementOutcome) {
        if let Some(hook) = hook {
            // A transport callback is outside the settlement transaction. Its panic cannot undo
            // the committed outcome, and a foreign panic payload may itself panic when dropped.
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook(outcome);
            })) {
                std::mem::forget(payload);
            }
        }
    }
}

impl Default for WriteSettlement {
    fn default() -> Self {
        Self::untracked()
    }
}

/// One bounded, cancellable partial-write attempt.
pub struct WriteRequest<'a> {
    /// Stable context shared by every partial call for the logical payload.
    pub context: WriteContext,
    /// Remaining bytes; a successful call accepts a non-zero prefix.
    pub bytes: &'a [u8],
    /// Absolute I/O deadline on the Runtime clock.
    pub deadline_ns: u64,
    /// Operation cancellation for normal report or command work.
    pub cancellation: CancellationToken,
    /// Controller resource cancellation used by deterministic close.
    pub resource_cancellation: CancellationToken,
    /// Unique backend settlement gate for this complete logical payload.
    pub settlement: WriteSettlement,
}

/// Stable fake/system transport failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    /// One bounded handshake or ACK exchange timed out.
    Timeout,
    /// A bounded transport write failed to make progress before its I/O deadline.
    WriteTimeout,
    /// Cancellation interrupted a blocking exchange.
    Cancelled,
    /// The device or port disconnected.
    Disconnected,
    /// Operating-system I/O failed.
    Io,
    /// Bytes violated the expected protocol.
    Protocol,
}

/// Owned transport error that never exposes an OS or serial package type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError {
    kind: TransportErrorKind,
    message: Arc<str>,
    stream_reusable: bool,
    cleanup_failure: bool,
}

impl TransportError {
    /// Creates an owned transport error.
    #[must_use]
    pub fn new(kind: TransportErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            stream_reusable: false,
            cleanup_failure: false,
        }
    }

    /// Creates an error for a backend-proven zero-effect failure whose stream remains usable.
    #[must_use]
    pub fn reusable_stream(kind: TransportErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            stream_reusable: true,
            cleanup_failure: false,
        }
    }

    /// Creates a typed cleanup failure that retains the preceding write and close diagnostics.
    #[must_use]
    pub fn cleanup_failure(primary: &Self, cleanup: &Self) -> Self {
        Self {
            kind: TransportErrorKind::Io,
            message: format!("Controller cleanup failure after {primary}: {cleanup}").into(),
            stream_reusable: false,
            cleanup_failure: true,
        }
    }

    /// Returns the stable category.
    #[must_use]
    pub const fn kind(&self) -> TransportErrorKind {
        self.kind
    }

    /// Returns diagnostic text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns whether the concrete backend proved this failed attempt had no effect and left
    /// framing reusable. All other errors require Controller stream settlement before retry.
    #[must_use]
    pub const fn stream_reusable(&self) -> bool {
        self.stream_reusable
    }

    /// Returns whether this error was produced while settling a failed stream cleanup.
    #[must_use]
    pub const fn is_cleanup_failure(&self) -> bool {
        self.cleanup_failure
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for TransportError {}

/// Controller-owned transport boundary. System serial implementations remain leaf dependencies.
pub trait ControllerTransport: Send + 'static {
    /// Opens at one baud and completes the source-exact hello exchange by `deadline_ns`.
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError>;

    /// Accepts a non-zero prefix and must wake for cancellation or the supplied deadline.
    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError>;

    /// Waits for one generation-tagged ACK frame or a bounded/cancelled outcome.
    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError>;

    /// Interrupts blocking I/O and releases the transport. Calling repeatedly is harmless.
    fn close(&mut self);

    /// Closes a settled stream and reports a cleanup failure when one can be observed.
    ///
    /// Implementations that only expose an infallible close keep the default. The Controller
    /// isolates panics from this method and projects them through the owning cleanup record.
    fn close_checked(&mut self) -> Result<(), TransportError> {
        self.close();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier, Mutex};

    use super::WriteSettlement;

    #[test]
    fn settlement_hook_observes_physical_acceptance_without_holding_its_mutex() {
        let holder = Arc::new(Mutex::new(None::<WriteSettlement>));
        let observed_unlocked = Arc::new(AtomicBool::new(false));
        let hook_holder = holder.clone();
        let hook_observed = observed_unlocked.clone();
        let settlement = WriteSettlement::tracked(Some(Box::new(move |_| {
            let settlement = hook_holder
                .lock()
                .expect("test settlement holder")
                .as_ref()
                .expect("settlement installed before completion")
                .clone();
            hook_observed.store(settlement.state.try_lock().is_ok(), Ordering::Release);
            assert!(settlement.is_full_accepted());
        })));
        *holder.lock().expect("test settlement holder") = Some(settlement.clone());

        assert!(settlement.mark_dispatched());
        assert!(settlement.full_accepted_at(17));
        assert!(observed_unlocked.load(Ordering::Acquire));
    }

    #[test]
    fn settlement_hook_panic_isolated_without_rewriting_physical_acceptance() {
        let settlement = WriteSettlement::tracked(Some(Box::new(|_| {
            panic!("scripted settlement hook panic");
        })));
        assert!(settlement.mark_dispatched());

        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert!(settlement.full_accepted_at(23));
            }))
            .is_ok()
        );
        assert!(settlement.is_full_accepted());
        assert_eq!(settlement.accepted_at_ns(), Some(23));
    }

    // conformance: controller.transport.rejected-full-reservation-duplicates
    #[test]
    fn rejected_full_reservation_cannot_be_upgraded_by_a_duplicate_completion() {
        let hook_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_hook_calls = hook_calls.clone();
        let settlement = WriteSettlement::tracked_with_full_reservation(
            Some(Box::new(|| false)),
            Some(Box::new(move |_| {
                observed_hook_calls.fetch_add(1, Ordering::AcqRel);
            })),
        );
        assert!(settlement.mark_dispatched());

        let barrier = Arc::new(Barrier::new(2));
        let duplicate_settlement = settlement.clone();
        let duplicate_barrier = barrier.clone();
        let duplicate = std::thread::spawn(move || {
            duplicate_barrier.wait();
            duplicate_settlement.full_accepted_at(42)
        });
        barrier.wait();
        assert!(
            !settlement.full_accepted_at(41),
            "the injected reservation rejection must reject the physical completion"
        );
        assert!(
            !duplicate.join().expect("duplicate completion thread"),
            "a concurrent duplicate completion must not synthesize FullAccepted after rejection"
        );
        assert!(!settlement.is_full_accepted());
        assert_eq!(hook_calls.load(Ordering::Acquire), 0);
    }
}
