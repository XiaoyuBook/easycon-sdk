use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::ThreadId;
use std::time::Duration;

use easycon_controller::{
    AckFrame, AckRequest, ControllerTransport, HANDSHAKE_REPLY, HANDSHAKE_REQUEST,
    HandshakeRequest, TransportError, TransportErrorKind, WriteContext, WriteRequest,
};
use easycon_runtime::{Clock, ClockChangeRegistration, VirtualClock};

/// Scripted result for one expected baud attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HandshakeOutcome {
    /// Hello succeeds after deterministic virtual elapsed time.
    Success { elapsed_ns: u64 },
    /// The protocol attempt times out.
    Timeout { elapsed_ns: u64 },
    /// A specific non-timeout failure occurs.
    Error {
        kind: TransportErrorKind,
        message: Arc<str>,
    },
}

/// Scripted ACK matcher input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AckOutcome {
    /// Delivers one generation-tagged byte.
    Frame {
        generation: u64,
        byte: u8,
        elapsed_ns: u64,
    },
    /// Exceeds the protocol deadline.
    Timeout { elapsed_ns: u64 },
    /// Returns a transport failure.
    Error {
        kind: TransportErrorKind,
        message: Arc<str>,
    },
    /// Blocks until its deadline, operation/resource cancellation, or transport close wakes it.
    BlockUntilCancelled,
}

/// Recorded source-exact handshake attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandshakeAttemptRecord {
    /// Baud supplied by the controller state machine.
    pub baud_rate: u32,
    /// Request bytes seen by transport.
    pub request_bytes: [u8; 3],
    /// Expected reply matcher.
    pub expected_reply: u8,
    /// Absolute protocol deadline.
    pub deadline_ns: u64,
}

/// One complete logical write accepted after any number of partial writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedWrite {
    /// Context shared by every partial call.
    pub context: WriteContext,
    /// Reassembled accepted bytes.
    pub bytes: Vec<u8>,
    /// OS thread that called every transport write.
    pub writer_thread: ThreadId,
    /// Number of partial calls used for this payload.
    pub partial_calls: usize,
}

/// Cloneable handle and transport implementation with shared deterministic state.
#[derive(Clone)]
pub struct FakeControllerTransport {
    clock: Arc<VirtualClock>,
    shared: Arc<FakeShared>,
    _clock_hook: Arc<ClockChangeRegistration>,
}

struct FakeShared {
    state: Mutex<FakeState>,
    changed: Condvar,
}

struct FakeState {
    handshakes: VecDeque<HandshakeScript>,
    attempts: Vec<HandshakeAttemptRecord>,
    accepted_writes: Vec<AcceptedWrite>,
    partial: Option<PartialWrite>,
    maximum_write_chunk: usize,
    write_calls: usize,
    fail_write_call: Option<(usize, TransportError)>,
    invalid_write_count_call: Option<(usize, usize)>,
    next_write_delay_ns: Option<u64>,
    write_block: Option<WriteBlockMode>,
    write_waiting: bool,
    write_release: bool,
    write_block_observers: Vec<mpsc::Sender<()>>,
    write_interrupt_observers: Vec<mpsc::Sender<()>>,
    close_failure: Option<CloseFailure>,
    ack_outcomes: VecDeque<AckOutcome>,
    ack_waiting: bool,
    open: bool,
    closed: bool,
}

struct HandshakeScript {
    baud_rate: u32,
    outcome: HandshakeOutcome,
}

struct PartialWrite {
    context: WriteContext,
    bytes: Vec<u8>,
    writer_thread: ThreadId,
    calls: usize,
    block_after_acceptance: Option<WriteBlockMode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteBlockMode {
    BeforeAcceptance,
    AfterFinalCompletionBeforeSettlement,
    AfterAcceptance,
    AfterAcceptanceThenCancelled,
}

enum CloseFailure {
    Error(TransportError),
    Panic,
    PanicWithDroppingPayload,
}

struct PanicOnDropPayload;

impl Drop for PanicOnDropPayload {
    fn drop(&mut self) {
        panic!("scripted fake close panic payload dropped");
    }
}

impl FakeControllerTransport {
    /// Creates an empty deterministic fake using the Runtime virtual clock.
    #[must_use]
    pub fn new(clock: Arc<VirtualClock>) -> Self {
        let shared = Arc::new(FakeShared {
            state: Mutex::new(FakeState {
                handshakes: VecDeque::new(),
                attempts: Vec::new(),
                accepted_writes: Vec::new(),
                partial: None,
                maximum_write_chunk: usize::MAX,
                write_calls: 0,
                fail_write_call: None,
                invalid_write_count_call: None,
                next_write_delay_ns: None,
                write_block: None,
                write_waiting: false,
                write_release: false,
                write_block_observers: Vec::new(),
                write_interrupt_observers: Vec::new(),
                close_failure: None,
                ack_outcomes: VecDeque::new(),
                ack_waiting: false,
                open: false,
                closed: false,
            }),
            changed: Condvar::new(),
        });
        let clock_wake = Arc::downgrade(&shared);
        let clock_hook = clock.on_change(Arc::new(move || {
            if let Some(shared) = clock_wake.upgrade() {
                shared.changed.notify_all();
            }
        }));
        Self {
            clock,
            shared,
            _clock_hook: Arc::new(clock_hook),
        }
    }

    /// Appends one expected baud and outcome.
    pub fn push_handshake(&self, baud_rate: u32, outcome: HandshakeOutcome) {
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .handshakes
            .push_back(HandshakeScript { baud_rate, outcome });
    }

    /// Limits each `write` call to a non-zero prefix.
    pub fn set_maximum_write_chunk(&self, maximum: usize) {
        assert!(maximum != 0, "partial write chunk must be non-zero");
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .maximum_write_chunk = maximum;
    }

    /// Fails one one-based partial write call.
    pub fn fail_write_call(&self, call: usize, error: TransportError) {
        assert!(call != 0, "write call index is one-based");
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .fail_write_call = Some((call, error));
    }

    /// Returns an invalid byte count from one one-based partial write call.
    pub fn return_invalid_write_count(&self, call: usize, accepted: usize) {
        assert!(call != 0, "write call index is one-based");
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .invalid_write_count_call = Some((call, accepted));
    }

    /// Advances virtual time before the next logical write accepts its first byte.
    pub fn delay_next_write_by(&self, elapsed_ns: u64) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.next_write_delay_ns.is_none(),
            "a write delay is already scripted"
        );
        state.next_write_delay_ns = Some(elapsed_ns);
    }

    /// Blocks the next logical write before any byte is accepted until cancellation/close.
    pub fn block_next_write_before_acceptance(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.write_block.is_none(),
            "a write block is already scripted"
        );
        state.write_block = Some(WriteBlockMode::BeforeAcceptance);
    }

    /// Blocks after the next logical payload is accepted but before `write` returns.
    pub fn block_next_write_after_acceptance(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.write_block.is_none(),
            "a write block is already scripted"
        );
        state.write_block = Some(WriteBlockMode::AfterAcceptance);
    }

    /// Blocks after the fake has reserved its final physical byte but before it publishes the
    /// logical-report settlement hook. This is a deterministic adapter-boundary probe.
    pub fn block_next_write_after_final_completion_before_settlement(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.write_block.is_none(),
            "a write block is already scripted"
        );
        state.write_block = Some(WriteBlockMode::AfterFinalCompletionBeforeSettlement);
    }

    /// Blocks after final byte acceptance, then reports a late cancellation to its caller.
    pub fn block_next_write_after_acceptance_then_cancelled(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.write_block.is_none(),
            "a write block is already scripted"
        );
        state.write_block = Some(WriteBlockMode::AfterAcceptanceThenCancelled);
    }

    /// Waits until the fake is blocked in a transport write.
    #[must_use]
    pub fn wait_until_write_blocked(&self, timeout: Duration) -> bool {
        let state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        let (state, _result) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| !state.write_waiting)
            .expect("fake transport lock poisoned while write waits");
        state.write_waiting
    }

    /// Releases a write blocked after byte acceptance.
    pub fn release_blocked_write(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        state.write_release = true;
        self.shared.changed.notify_all();
    }

    /// Returns a one-shot observation channel for the next blocked transport write.
    #[must_use]
    pub fn observe_next_write_blocked(&self) -> mpsc::Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .write_block_observers
            .push(sender);
        receiver
    }

    /// Returns a one-shot channel notified when a blocked write observes cancellation.
    #[must_use]
    pub fn observe_next_write_interrupted(&self) -> mpsc::Receiver<()> {
        let (sender, receiver) = mpsc::channel();
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .write_interrupt_observers
            .push(sender);
        receiver
    }

    /// Makes the next checked close return a typed transport failure after it closes the stream.
    pub fn fail_next_close(&self, error: TransportError) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.close_failure.is_none(),
            "a close failure is already scripted"
        );
        state.close_failure = Some(CloseFailure::Error(error));
    }

    /// Makes the next checked close panic after it closes the stream.
    pub fn panic_next_close(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.close_failure.is_none(),
            "a close failure is already scripted"
        );
        state.close_failure = Some(CloseFailure::Panic);
    }

    /// Makes the next checked close panic with a payload whose destructor also panics.
    pub fn panic_next_close_with_dropping_payload(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        assert!(
            state.close_failure.is_none(),
            "a close failure is already scripted"
        );
        state.close_failure = Some(CloseFailure::PanicWithDroppingPayload);
    }

    /// Appends one ACK result.
    pub fn push_ack(&self, outcome: AckOutcome) {
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .ack_outcomes
            .push_back(outcome);
    }

    /// Simulates an external disconnect before the next write.
    pub fn disconnect(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        state.open = false;
        state.closed = true;
        self.shared.changed.notify_all();
    }

    /// Returns recorded attempts in order.
    #[must_use]
    pub fn handshake_attempts(&self) -> Vec<HandshakeAttemptRecord> {
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .attempts
            .clone()
    }

    /// Returns complete accepted logical writes in order.
    #[must_use]
    pub fn accepted_writes(&self) -> Vec<AcceptedWrite> {
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .accepted_writes
            .clone()
    }

    /// Returns total transport partial-write calls.
    #[must_use]
    pub fn write_call_count(&self) -> usize {
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .write_calls
    }

    /// Returns whether close was requested.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .closed
    }

    /// Waits until at least `count` complete writes are accepted.
    #[must_use]
    pub fn wait_for_accepted_count(&self, count: usize, timeout: Duration) -> bool {
        let state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        let (state, _result) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| state.accepted_writes.len() < count)
            .expect("fake transport lock poisoned while waiting");
        state.accepted_writes.len() >= count
    }

    /// Waits until the fake is blocked inside an ACK read.
    #[must_use]
    pub fn wait_until_ack_blocked(&self, timeout: Duration) -> bool {
        let state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        let (state, _result) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| !state.ack_waiting)
            .expect("fake transport lock poisoned while waiting");
        state.ack_waiting
    }
}

impl ControllerTransport for FakeControllerTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        if request.request_bytes != HANDSHAKE_REQUEST || request.expected_reply != HANDSHAKE_REPLY {
            return Err(TransportError::new(
                TransportErrorKind::Protocol,
                "handshake bytes do not match the source-exact protocol",
            ));
        }
        let script = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("fake transport lock poisoned");
            state.closed = false;
            state.attempts.push(HandshakeAttemptRecord {
                baud_rate: request.baud_rate,
                request_bytes: request.request_bytes,
                expected_reply: request.expected_reply,
                deadline_ns: request.deadline_ns,
            });
            state.handshakes.pop_front().ok_or_else(|| {
                TransportError::new(
                    TransportErrorKind::Protocol,
                    "no scripted handshake outcome",
                )
            })?
        };
        if script.baud_rate != request.baud_rate {
            return Err(TransportError::new(
                TransportErrorKind::Protocol,
                "handshake baud order differed from the script",
            ));
        }
        if request.cancellation.is_cancelled() || request.resource_cancellation.is_cancelled() {
            return Err(TransportError::new(
                TransportErrorKind::Cancelled,
                "handshake cancelled",
            ));
        }

        let elapsed_ns = match &script.outcome {
            HandshakeOutcome::Success { elapsed_ns } | HandshakeOutcome::Timeout { elapsed_ns } => {
                *elapsed_ns
            }
            HandshakeOutcome::Error { .. } => 0,
        };
        let target = self.clock.now_ns().saturating_add(elapsed_ns);
        self.clock.advance_to(target.min(request.deadline_ns));
        if request.cancellation.is_cancelled() || request.resource_cancellation.is_cancelled() {
            return Err(TransportError::new(
                TransportErrorKind::Cancelled,
                "handshake cancelled",
            ));
        }
        if target >= request.deadline_ns {
            return Err(TransportError::new(
                TransportErrorKind::Timeout,
                "handshake protocol timeout",
            ));
        }

        match script.outcome {
            HandshakeOutcome::Success { .. } => {
                self.shared
                    .state
                    .lock()
                    .expect("fake transport lock poisoned")
                    .open = true;
                Ok(())
            }
            HandshakeOutcome::Timeout { .. } => Err(TransportError::new(
                TransportErrorKind::Timeout,
                "scripted handshake timeout",
            )),
            HandshakeOutcome::Error { kind, message } => Err(TransportError::new(kind, message)),
        }
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        let context = request.context;
        let bytes = request.bytes;
        let operation_cancel = request.cancellation.clone();
        let resource_cancel = request.resource_cancellation.clone();
        let shared = self.shared.clone();
        let _operation_wake = request
            .cancellation
            .on_cancel_scoped(move || shared.changed.notify_all());
        let shared = self.shared.clone();
        let _resource_wake = request
            .resource_cancellation
            .on_cancel_scoped(move || shared.changed.notify_all());
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        if state.closed || !state.open {
            state.partial = None;
            return Err(TransportError::new(
                TransportErrorKind::Disconnected,
                "fake transport is disconnected",
            ));
        }
        if operation_cancel.is_cancelled() || resource_cancel.is_cancelled() {
            let zero_effect = state.partial.is_none();
            state.partial = None;
            return Err(if zero_effect {
                TransportError::reusable_stream(
                    TransportErrorKind::Cancelled,
                    "write cancelled before transport acceptance",
                )
            } else {
                TransportError::new(
                    TransportErrorKind::Cancelled,
                    "write cancelled after a partial prefix",
                )
            });
        }
        if self.clock.now_ns() >= request.deadline_ns {
            let zero_effect = state.partial.is_none();
            state.partial = None;
            return Err(if zero_effect {
                TransportError::reusable_stream(
                    TransportErrorKind::WriteTimeout,
                    "write I/O deadline elapsed before transport acceptance",
                )
            } else {
                TransportError::new(
                    TransportErrorKind::WriteTimeout,
                    "write I/O deadline elapsed after a partial prefix",
                )
            });
        }
        let (block, delay_ns) = if state.partial.is_none() {
            (
                state.write_block.take(),
                state.next_write_delay_ns.take().unwrap_or(0),
            )
        } else {
            (None, 0)
        };
        if block == Some(WriteBlockMode::BeforeAcceptance) {
            state.write_waiting = true;
            state.write_release = false;
            for observer in std::mem::take(&mut state.write_block_observers) {
                let _ = observer.send(());
            }
            self.shared.changed.notify_all();
            while !operation_cancel.is_cancelled()
                && !resource_cancel.is_cancelled()
                && !state.closed
                && self.clock.now_ns() < request.deadline_ns
            {
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .expect("fake transport lock poisoned while write waits");
            }
            state.write_waiting = false;
            self.shared.changed.notify_all();
            if operation_cancel.is_cancelled() || resource_cancel.is_cancelled() {
                state.partial = None;
                return Err(TransportError::reusable_stream(
                    TransportErrorKind::Cancelled,
                    "write cancelled before transport acceptance",
                ));
            }
            if state.closed {
                state.partial = None;
                return Err(TransportError::new(
                    TransportErrorKind::Disconnected,
                    "transport closed during write",
                ));
            }
            if self.clock.now_ns() >= request.deadline_ns {
                state.partial = None;
                return Err(TransportError::reusable_stream(
                    TransportErrorKind::WriteTimeout,
                    "write I/O deadline elapsed before transport acceptance",
                ));
            }
        }
        if delay_ns != 0 {
            drop(state);
            let delay_target = self.clock.now_ns().saturating_add(delay_ns);
            self.clock.advance_to(delay_target.min(request.deadline_ns));
            state = self
                .shared
                .state
                .lock()
                .expect("fake transport lock poisoned after write delay");
            if state.closed || !state.open {
                state.partial = None;
                return Err(TransportError::new(
                    TransportErrorKind::Disconnected,
                    "fake transport disconnected during write delay",
                ));
            }
            if operation_cancel.is_cancelled() || resource_cancel.is_cancelled() {
                state.partial = None;
                return Err(TransportError::reusable_stream(
                    TransportErrorKind::Cancelled,
                    "write cancelled during transport delay before acceptance",
                ));
            }
            if self.clock.now_ns() >= request.deadline_ns {
                state.partial = None;
                return Err(TransportError::reusable_stream(
                    TransportErrorKind::WriteTimeout,
                    "write I/O deadline elapsed during transport delay before acceptance",
                ));
            }
        }
        state.write_calls = state
            .write_calls
            .checked_add(1)
            .expect("write call overflow");
        if state
            .fail_write_call
            .as_ref()
            .is_some_and(|(call, _)| *call == state.write_calls)
        {
            let (_, error) = state
                .fail_write_call
                .take()
                .expect("write failure checked above");
            state.partial = None;
            return Err(error);
        }
        if state
            .invalid_write_count_call
            .as_ref()
            .is_some_and(|(call, _)| *call == state.write_calls)
        {
            let (_, accepted) = state
                .invalid_write_count_call
                .take()
                .expect("invalid write count checked above");
            return Ok(accepted);
        }
        let accepted = state.maximum_write_chunk.min(bytes.len());
        let thread = std::thread::current().id();
        if state.partial.is_none() {
            state.partial = Some(PartialWrite {
                context,
                bytes: Vec::with_capacity(context.total_len),
                writer_thread: thread,
                calls: 0,
                block_after_acceptance: match block {
                    Some(
                        mode @ (WriteBlockMode::AfterAcceptance
                        | WriteBlockMode::AfterAcceptanceThenCancelled
                        | WriteBlockMode::AfterFinalCompletionBeforeSettlement),
                    ) => Some(mode),
                    Some(WriteBlockMode::BeforeAcceptance) | None => None,
                },
            });
        }
        let partial = state.partial.as_mut().expect("partial initialized above");
        if partial.context != context || partial.writer_thread != thread {
            return Err(TransportError::new(
                TransportErrorKind::Protocol,
                "partial writes changed context or writer thread",
            ));
        }
        partial.bytes.extend_from_slice(&bytes[..accepted]);
        partial.calls = partial.calls.checked_add(1).expect("partial call overflow");
        let mut block_after_acceptance = None;
        let mut completed_write = None;
        if partial.bytes.len() == context.total_len {
            let complete = state.partial.take().expect("complete partial exists");
            block_after_acceptance = complete.block_after_acceptance;
            completed_write = Some(AcceptedWrite {
                context: complete.context,
                bytes: complete.bytes,
                writer_thread: complete.writer_thread,
                partial_calls: complete.calls,
            });
        } else if partial.bytes.len() > context.total_len {
            return Err(TransportError::new(
                TransportErrorKind::Protocol,
                "partial writes exceeded the logical payload length",
            ));
        }
        if let Some(completed_write) = completed_write {
            drop(state);
            if !request.settlement.reserve_full_acceptance() {
                return Err(TransportError::new(
                    TransportErrorKind::Io,
                    "fake backend rejected final-byte settlement reservation",
                ));
            }
            state = self
                .shared
                .state
                .lock()
                .expect("fake transport lock poisoned after final-byte reservation");
            if block_after_acceptance == Some(WriteBlockMode::AfterFinalCompletionBeforeSettlement)
            {
                state.write_waiting = true;
                state.write_release = false;
                for observer in std::mem::take(&mut state.write_block_observers) {
                    let _ = observer.send(());
                }
                self.shared.changed.notify_all();
                while !state.write_release {
                    state = self
                        .shared
                        .changed
                        .wait(state)
                        .expect("fake transport lock poisoned at final completion boundary");
                }
                state.write_waiting = false;
                state.write_release = false;
                self.shared.changed.notify_all();
            }
            drop(state);
            let accepted_at_ns = self.clock.now_ns();
            let claimed = request
                .settlement
                .publish_reserved_full_acceptance(accepted_at_ns);
            state = self
                .shared
                .state
                .lock()
                .expect("fake transport lock poisoned after final-byte settlement");
            if !claimed {
                return Err(TransportError::new(
                    TransportErrorKind::Io,
                    "fake backend rejected final-byte settlement gate",
                ));
            }
            state.accepted_writes.push(completed_write);
            self.shared.changed.notify_all();
        }
        if let Some(block_after_acceptance) = block_after_acceptance
            && block_after_acceptance != WriteBlockMode::AfterFinalCompletionBeforeSettlement
        {
            state.write_waiting = true;
            state.write_release = false;
            for observer in std::mem::take(&mut state.write_block_observers) {
                let _ = observer.send(());
            }
            self.shared.changed.notify_all();
            while !state.write_release
                && !operation_cancel.is_cancelled()
                && !resource_cancel.is_cancelled()
                && !state.closed
                && self.clock.now_ns() < request.deadline_ns
            {
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .expect("fake transport lock poisoned after write acceptance");
            }
            state.write_waiting = false;
            state.write_release = false;
            self.shared.changed.notify_all();
            if operation_cancel.is_cancelled() || resource_cancel.is_cancelled() {
                for observer in std::mem::take(&mut state.write_interrupt_observers) {
                    let _ = observer.send(());
                }
            }
            if block_after_acceptance == WriteBlockMode::AfterAcceptanceThenCancelled {
                return Err(TransportError::new(
                    TransportErrorKind::Cancelled,
                    "backend completion reported cancellation after accepting the final byte",
                ));
            }
        }
        Ok(accepted)
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        let outcome = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .ack_outcomes
            .pop_front()
            .ok_or_else(|| {
                TransportError::new(TransportErrorKind::Protocol, "no scripted ACK outcome")
            })?;
        if request.cancellation.is_cancelled() || request.resource_cancellation.is_cancelled() {
            return Err(TransportError::new(
                TransportErrorKind::Cancelled,
                "ACK wait cancelled",
            ));
        }
        match outcome {
            AckOutcome::Frame {
                generation,
                byte,
                elapsed_ns,
            } => {
                let target = self.clock.now_ns().saturating_add(elapsed_ns);
                self.clock.advance_to(target.min(request.deadline_ns));
                if target >= request.deadline_ns {
                    Err(TransportError::new(
                        TransportErrorKind::Timeout,
                        "ACK protocol timeout",
                    ))
                } else {
                    Ok(AckFrame { generation, byte })
                }
            }
            AckOutcome::Timeout { elapsed_ns } => {
                let target = self.clock.now_ns().saturating_add(elapsed_ns);
                self.clock.advance_to(target.min(request.deadline_ns));
                Err(TransportError::new(
                    TransportErrorKind::Timeout,
                    "scripted ACK timeout",
                ))
            }
            AckOutcome::Error { kind, message } => Err(TransportError::new(kind, message)),
            AckOutcome::BlockUntilCancelled => {
                let operation_cancel = request.cancellation.clone();
                let resource_cancel = request.resource_cancellation.clone();
                let shared = self.shared.clone();
                let _operation_wake = request
                    .cancellation
                    .on_cancel_scoped(move || shared.changed.notify_all());
                let shared = self.shared.clone();
                let _resource_wake = request
                    .resource_cancellation
                    .on_cancel_scoped(move || shared.changed.notify_all());
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .expect("fake transport lock poisoned");
                state.ack_waiting = true;
                self.shared.changed.notify_all();
                while !operation_cancel.is_cancelled()
                    && !resource_cancel.is_cancelled()
                    && !state.closed
                    && self.clock.now_ns() < request.deadline_ns
                {
                    state = self
                        .shared
                        .changed
                        .wait(state)
                        .expect("fake transport lock poisoned while ACK waits");
                }
                state.ack_waiting = false;
                self.shared.changed.notify_all();
                if operation_cancel.is_cancelled() || resource_cancel.is_cancelled() {
                    Err(TransportError::new(
                        TransportErrorKind::Cancelled,
                        "ACK wait cancelled",
                    ))
                } else if state.closed {
                    Err(TransportError::new(
                        TransportErrorKind::Disconnected,
                        "transport closed during ACK wait",
                    ))
                } else {
                    Err(TransportError::new(
                        TransportErrorKind::Timeout,
                        "ACK protocol deadline elapsed",
                    ))
                }
            }
        }
    }

    fn close(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned");
        state.open = false;
        state.closed = true;
        state.partial = None;
        self.shared.changed.notify_all();
    }

    fn close_checked(&mut self) -> Result<(), TransportError> {
        let failure = self
            .shared
            .state
            .lock()
            .expect("fake transport lock poisoned")
            .close_failure
            .take();
        self.close();
        match failure {
            None => Ok(()),
            Some(CloseFailure::Error(error)) => Err(error),
            Some(CloseFailure::Panic) => panic!("scripted fake checked-close panic"),
            Some(CloseFailure::PanicWithDroppingPayload) => {
                std::panic::panic_any(PanicOnDropPayload)
            }
        }
    }
}
