use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use easycon_controller::{
    AckFrame, AckRequest, ControllerTransport, HANDSHAKE_REPLY, HANDSHAKE_REQUEST,
    HandshakeRequest, TransportError, TransportErrorKind, WriteKind, WriteRequest,
};
use easycon_model::OperationId;
use easycon_runtime::Clock;

/// One complete direct-report software-path timing sample on a shared monotonic clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectLatencySample {
    /// Direct operation associated with the report.
    pub operation_id: OperationId,
    /// Controller lane logical-write sequence.
    pub write_sequence: u64,
    /// Timestamp immediately before channel admission.
    pub command_admitted_ns: u64,
    /// Timestamp when the single-writer lane received the command.
    pub lane_wake_ns: u64,
    /// Timestamp when the lane dispatched the report.
    pub lane_dispatch_ns: u64,
    /// Timestamp sampled inside `ControllerTransport::write` before any acceptance work.
    pub transport_write_entered_ns: u64,
    /// Timestamp when the in-memory transport accepted the complete report.
    pub transport_accepted_ns: u64,
}

impl DirectLatencySample {
    /// Returns whether every segment is monotonic and belongs to one complete transport write.
    #[must_use]
    pub const fn is_monotonic(self) -> bool {
        self.command_admitted_ns <= self.lane_wake_ns
            && self.lane_wake_ns <= self.lane_dispatch_ns
            && self.lane_dispatch_ns <= self.transport_write_entered_ns
            && self.transport_write_entered_ns <= self.transport_accepted_ns
    }

    /// Returns the Phase 2A target interval from command admission to transport entry.
    #[must_use]
    pub const fn admitted_to_write_entered_ns(self) -> u64 {
        self.transport_write_entered_ns
            .saturating_sub(self.command_admitted_ns)
    }
}

/// Cloneable observation side of the non-blocking latency transport.
#[derive(Clone)]
pub struct DirectLatencyRecorder {
    shared: Arc<Shared>,
}

impl DirectLatencyRecorder {
    /// Returns all measured direct reports without dropping or filtering samples.
    #[must_use]
    pub fn samples(&self) -> Vec<DirectLatencySample> {
        self.shared
            .state
            .lock()
            .expect("latency transport state")
            .samples
            .clone()
    }

    /// Clears an explicit warm-up population before the measured population begins.
    pub fn clear(&self) {
        self.shared
            .state
            .lock()
            .expect("latency transport state")
            .samples
            .clear();
    }

    /// Waits for an exact lower bound without polling or sleeping.
    #[must_use]
    pub fn wait_for_samples(&self, count: usize, timeout: Duration) -> bool {
        let state = self.shared.state.lock().expect("latency transport state");
        let (state, _result) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| state.samples.len() < count)
            .expect("latency transport sample wait");
        state.samples.len() >= count
    }

    /// Returns whether deterministic close reached the transport.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared
            .state
            .lock()
            .expect("latency transport state")
            .closed
    }
}

/// Non-blocking memory transport used only by Phase 2A latency measurement assets.
pub struct DirectLatencyTransport {
    clock: Arc<dyn Clock>,
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    connected: bool,
    closed: bool,
    samples: Vec<DirectLatencySample>,
}

impl DirectLatencyTransport {
    /// Creates a transport and its observation handle on the Controller Runtime clock.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> (Self, DirectLatencyRecorder) {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        });
        (
            Self {
                clock,
                shared: shared.clone(),
            },
            DirectLatencyRecorder { shared },
        )
    }
}

impl ControllerTransport for DirectLatencyTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        if request.request_bytes != HANDSHAKE_REQUEST || request.expected_reply != HANDSHAKE_REPLY {
            return Err(protocol_error(
                "latency transport handshake was not source-exact",
            ));
        }
        if request.cancellation.is_cancelled() || request.resource_cancellation.is_cancelled() {
            return Err(TransportError::new(
                TransportErrorKind::Cancelled,
                "latency transport handshake cancelled",
            ));
        }
        let mut state = self.shared.state.lock().expect("latency transport state");
        state.connected = true;
        state.closed = false;
        Ok(())
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        let entered_at_ns = self.clock.now_ns();
        if request.bytes.is_empty() || request.bytes.len() != request.context.total_len {
            return Err(protocol_error(
                "latency transport requires one complete logical write",
            ));
        }
        if request.cancellation.is_cancelled() || request.resource_cancellation.is_cancelled() {
            return Err(TransportError::new(
                TransportErrorKind::Cancelled,
                "latency transport write cancelled",
            ));
        }
        let mut state = self.shared.state.lock().expect("latency transport state");
        if !state.connected || state.closed {
            return Err(TransportError::new(
                TransportErrorKind::Disconnected,
                "latency transport is closed",
            ));
        }
        if request.context.kind == WriteKind::Report {
            let timing = request.context.direct_timing.ok_or_else(|| {
                protocol_error("latency report did not carry direct timing stages")
            })?;
            let operation_id = request
                .context
                .operation_id
                .ok_or_else(|| protocol_error("latency report did not carry an operation ID"))?;
            let accepted_at_ns = self.clock.now_ns();
            state.samples.push(DirectLatencySample {
                operation_id,
                write_sequence: request.context.sequence,
                command_admitted_ns: timing.command_admitted_ns,
                lane_wake_ns: timing.lane_wake_ns,
                lane_dispatch_ns: request.context.timestamp_ns,
                transport_write_entered_ns: entered_at_ns,
                transport_accepted_ns: accepted_at_ns,
            });
            self.shared.changed.notify_all();
        }
        Ok(request.bytes.len())
    }

    fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
        Err(protocol_error(
            "latency transport does not script ACK commands",
        ))
    }

    fn close(&mut self) {
        let mut state = self.shared.state.lock().expect("latency transport state");
        state.connected = false;
        state.closed = true;
        drop(state);
        self.shared.changed.notify_all();
    }
}

fn protocol_error(message: &'static str) -> TransportError {
    TransportError::new(TransportErrorKind::Protocol, message)
}
