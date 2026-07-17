use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

use easycon_model::{
    Button, EasyConError, ErrorCode, ErrorDomain, Hat, OperationId, ResourceId, StickPosition,
};
use easycon_runtime::{
    Clock, DeadlineId, EventDraft, EventKind, ManagedResource, Operation, OperationState,
    OperationValue, ResourceRegistration, Runtime, Severity, TaskRegistration, TransitionOutcome,
};

use crate::protocol::SwitchReport;
use crate::transport::{
    AUTO_BAUD_RATES, ControllerTransport, HANDSHAKE_REPLY, HANDSHAKE_REQUEST, HandshakeRequest,
    TransportError, TransportErrorKind, WriteContext, WriteKind,
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
}

impl Default for ControllerSnapshot {
    fn default() -> Self {
        Self {
            state: ControllerState::Disconnected,
            desired_report: SwitchReport::NEUTRAL,
            accepted_report_count: 0,
            last_report_timestamp_ns: None,
        }
    }
}

/// Runtime options for one controller lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControllerOptions {
    /// Minimum interval between complete report acceptances.
    pub minimum_report_interval_ns: u64,
}

impl Default for ControllerOptions {
    fn default() -> Self {
        Self {
            minimum_report_interval_ns: 30_000_000,
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

struct ControllerInner {
    runtime: Runtime,
    resource_id: OnceLock<ResourceId>,
    sender: Sender<LaneCommand>,
    snapshot: Arc<Mutex<ControllerSnapshot>>,
    worker: Mutex<WorkerState>,
    worker_ready: Condvar,
    close_gate: Mutex<()>,
    registration: Mutex<Option<ResourceRegistration>>,
}

enum WorkerState {
    Starting,
    Running(JoinHandle<()>),
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
        action: ControllerAction,
    },
    Wake,
    Close {
        completed: SyncSender<()>,
    },
}

struct ScheduledReport {
    target_ns: u64,
    due_ns: u64,
    deadline_id: DeadlineId,
    operation: Operation,
    report: SwitchReport,
    kind: WriteKind,
}

struct ControllerLane {
    runtime: Runtime,
    resource_id: ResourceId,
    clock: Arc<dyn Clock>,
    options: ControllerOptions,
    transport: Box<dyn ControllerTransport>,
    receiver: Receiver<LaneCommand>,
    snapshot: Arc<Mutex<ControllerSnapshot>>,
    task: Option<TaskRegistration>,
    desired_report: SwitchReport,
    pending_reports: VecDeque<ScheduledReport>,
    last_dispatch_ns: Option<u64>,
    next_write_sequence: u64,
}

impl ControllerSession {
    /// Creates and registers a controller without opening transport or hardware.
    pub fn new(
        runtime: Runtime,
        transport: Box<dyn ControllerTransport>,
        options: ControllerOptions,
    ) -> Result<Self, EasyConError> {
        if options.minimum_report_interval_ns == 0 {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "minimum report interval must be non-zero",
            ));
        }
        let (sender, receiver) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(ControllerSnapshot::default()));
        let inner = Arc::new(ControllerInner {
            runtime: runtime.clone(),
            resource_id: OnceLock::new(),
            sender: sender.clone(),
            snapshot: snapshot.clone(),
            worker: Mutex::new(WorkerState::Starting),
            worker_ready: Condvar::new(),
            close_gate: Mutex::new(()),
            registration: Mutex::new(None),
        });
        let managed: Arc<dyn ManagedResource> = inner.clone();
        let (registration, task) = match runtime.register_resource_with_task(managed) {
            Ok(registrations) => registrations,
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
        let wake_sender = sender;
        clock.on_change(Arc::new(move || {
            let _ = wake_sender.send(LaneCommand::Wake);
        }));
        let lane = ControllerLane {
            runtime,
            resource_id,
            clock,
            options,
            transport,
            receiver,
            snapshot,
            task: Some(task),
            desired_report: SwitchReport::NEUTRAL,
            pending_reports: VecDeque::new(),
            last_dispatch_ns: None,
            next_write_sequence: 1,
        };
        let worker = std::thread::Builder::new()
            .name(format!("easycon-controller-{}", resource_id.get()))
            .spawn(move || lane.run())
            .map_err(|error| {
                let mut state = inner
                    .worker
                    .lock()
                    .expect("controller worker lock poisoned");
                *state = WorkerState::Failed;
                drop(state);
                inner.worker_ready.notify_all();
                EasyConError::new(
                    ErrorDomain::Runtime,
                    ErrorCode::Internal,
                    format!("failed to start controller lane: {error}"),
                )
            })?;
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
        let operation = self
            .inner
            .runtime
            .create_operation(options.operation_deadline_ns)?;
        self.attach_wake(&operation);
        if self
            .inner
            .sender
            .send(LaneCommand::Connect {
                operation: operation.clone(),
                options,
            })
            .is_err()
        {
            fail_closed_lane(&operation);
        }
        Ok(operation)
    }

    /// Submits one direct desired-state mutation.
    pub fn direct(&self, action: ControllerAction) -> Result<Operation, EasyConError> {
        let operation = self.inner.runtime.create_operation(None)?;
        self.attach_wake(&operation);
        if self
            .inner
            .sender
            .send(LaneCommand::Direct {
                operation: operation.clone(),
                action,
            })
            .is_err()
        {
            fail_closed_lane(&operation);
        }
        Ok(operation)
    }

    /// Submits a neutral reset through the same writer lane.
    pub fn reset(&self) -> Result<Operation, EasyConError> {
        self.direct(ControllerAction::Reset)
    }

    /// Idempotently neutralizes, closes transport, and joins the writer.
    pub fn close(&self) {
        self.inner.close_internal();
    }

    fn attach_wake(&self, operation: &Operation) {
        let sender = self.inner.sender.clone();
        operation.on_cancel(move || {
            let _ = sender.send(LaneCommand::Wake);
        });
    }
}

impl ControllerInner {
    fn close_internal(&self) {
        let _close = self
            .close_gate
            .lock()
            .expect("controller close lock poisoned");
        let mut worker_state = self.worker.lock().expect("controller worker lock poisoned");
        while matches!(*worker_state, WorkerState::Starting) {
            worker_state = self
                .worker_ready
                .wait(worker_state)
                .expect("controller worker lock poisoned while starting");
        }
        let running = matches!(*worker_state, WorkerState::Running(_));
        drop(worker_state);
        if running {
            let (completed, receiver) = mpsc::sync_channel(0);
            if self.sender.send(LaneCommand::Close { completed }).is_ok() {
                let _ = receiver.recv();
            }
        }
        let worker = {
            let mut worker_state = self.worker.lock().expect("controller worker lock poisoned");
            match std::mem::replace(&mut *worker_state, WorkerState::Joined) {
                WorkerState::Running(worker) => Some(worker),
                WorkerState::Starting => unreachable!("worker construction barrier was crossed"),
                WorkerState::Failed | WorkerState::Joined => None,
            }
        };
        if let Some(worker) = worker {
            worker.join().expect("controller lane must not panic");
        }
        self.registration
            .lock()
            .expect("controller registration lock poisoned")
            .take();
    }
}

impl ManagedResource for ControllerInner {
    fn close(&self) {
        self.close_internal();
    }
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        self.close_internal();
    }
}

impl ControllerLane {
    fn run(mut self) {
        loop {
            self.observe_cancellation();
            self.dispatch_due_reports();

            let command = match self.next_wait_duration() {
                Some(duration) => match self.receiver.recv_timeout(duration) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => Some(LaneCommand::Wake),
                    Err(RecvTimeoutError::Disconnected) => None,
                },
                None => self.receiver.recv().ok(),
            };
            let Some(command) = command else {
                self.close_lane(None);
                break;
            };
            match command {
                LaneCommand::Connect { operation, options } => {
                    self.handle_connect(operation, options);
                }
                LaneCommand::Direct { operation, action } => {
                    self.handle_direct(operation, action);
                }
                LaneCommand::Wake => {}
                LaneCommand::Close { completed } => {
                    self.close_lane(Some(completed));
                    break;
                }
            }
        }
        self.task.take();
    }

    fn next_wait_duration(&self) -> Option<std::time::Duration> {
        self.pending_reports
            .front()
            .and_then(|report| self.clock.real_wait_duration(report.due_ns))
    }

    fn handle_connect(&mut self, operation: Operation, options: ConnectOptions) {
        if operation.snapshot().state == OperationState::Cancelling {
            operation.finish_cancelled();
            return;
        }
        if operation.start() != TransitionOutcome::Applied {
            return;
        }
        if self.state() != ControllerState::Disconnected {
            operation.fail(EasyConError::new(
                ErrorDomain::Controller,
                ErrorCode::ResourceBusy,
                "controller is not disconnected",
            ));
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
                self.transport.close();
                self.set_state(
                    ControllerState::Disconnected,
                    "controller.disconnected",
                    None,
                );
                operation.finish_cancelled();
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
            };
            match self.transport.handshake(request) {
                Ok(()) if !self.cancel_or_deadline(&operation, options.operation_deadline_ns) => {
                    self.set_state(
                        ControllerState::Connected,
                        "controller.connected",
                        Some(operation.id()),
                    );
                    operation.succeed(OperationValue::Unit);
                    return;
                }
                Ok(()) => {
                    self.transport.close();
                    self.set_state(
                        ControllerState::Disconnected,
                        "controller.disconnected",
                        None,
                    );
                    operation.finish_cancelled();
                    return;
                }
                Err(error) if error.kind() == TransportErrorKind::Cancelled => {
                    if operation.snapshot().state != OperationState::Cancelling {
                        operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
                    }
                    self.transport.close();
                    self.set_state(
                        ControllerState::Disconnected,
                        "controller.disconnected",
                        None,
                    );
                    operation.finish_cancelled();
                    return;
                }
                Err(error) => {
                    last_error = error;
                    self.transport.close();
                }
            }
        }

        self.set_state(
            ControllerState::Disconnected,
            "controller.disconnected",
            None,
        );
        operation.fail(map_transport_error(last_error));
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

    fn handle_direct(&mut self, operation: Operation, action: ControllerAction) {
        if operation.snapshot().state == OperationState::Cancelling {
            operation.finish_cancelled();
            return;
        }
        if operation.start() != TransitionOutcome::Applied {
            return;
        }
        if self.state() != ControllerState::Connected {
            operation.fail(EasyConError::new(
                ErrorDomain::Controller,
                ErrorCode::DeviceDisconnected,
                "controller is not connected",
            ));
            return;
        }

        action.apply(&mut self.desired_report);
        self.update_desired_snapshot();
        let now = self.clock.now_ns();
        let target_ns = self.last_dispatch_ns.map_or(now, |last| {
            last.saturating_add(self.options.minimum_report_interval_ns)
                .max(now)
        });
        let deadline_id = self.clock.register_deadline(target_ns);
        self.pending_reports.push_back(ScheduledReport {
            target_ns,
            due_ns: target_ns,
            deadline_id,
            operation,
            report: self.desired_report,
            kind: WriteKind::Report,
        });
    }

    fn observe_cancellation(&mut self) {
        let Some(index) = self.pending_reports.iter().position(|report| {
            report.kind == WriteKind::Report
                && report.operation.snapshot().state == OperationState::Cancelling
        }) else {
            return;
        };
        let cancelled = self
            .pending_reports
            .remove(index)
            .expect("index came from pending report queue");
        self.desired_report.reset();
        self.update_desired_snapshot();
        let now = self.clock.now_ns();
        let due_ns = self.last_dispatch_ns.map_or(now, |last| {
            last.saturating_add(self.options.minimum_report_interval_ns)
                .max(now)
        });
        let deadline_id = self.clock.register_deadline(due_ns);
        self.pending_reports.push_front(ScheduledReport {
            target_ns: now,
            due_ns,
            deadline_id,
            operation: cancelled.operation,
            report: SwitchReport::NEUTRAL,
            kind: WriteKind::Neutralize,
        });
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
                && pending.operation.snapshot().state == OperationState::Cancelling
            {
                self.pending_reports.push_front(pending);
                self.observe_cancellation();
                continue;
            }
            let earliest = self.last_dispatch_ns.map_or(now, |last| {
                last.saturating_add(self.options.minimum_report_interval_ns)
            });
            if earliest > now {
                pending.due_ns = earliest;
                pending.deadline_id = self.clock.register_deadline(earliest);
                self.runtime.publish(
                    EventDraft::ordinary(
                        EventKind::TimingDeviation,
                        "controller.report.delayed",
                        Severity::Warning,
                    )
                    .with_resource(self.resource_id)
                    .with_operation(pending.operation.id())
                    .with_detail(format!(
                        "target_ns={}, dispatch_not_before_ns={earliest}",
                        pending.target_ns
                    )),
                );
                self.pending_reports.push_front(pending);
                return;
            }

            let bytes = pending.report.encode();
            match self.write_payload(Some(pending.operation.id()), pending.kind, now, &bytes) {
                Ok(()) => {
                    self.clock.record_dispatch(pending.deadline_id, now);
                    self.last_dispatch_ns = Some(now);
                    self.record_report_acceptance(
                        now,
                        pending.report,
                        pending.operation.id(),
                        bytes,
                    );
                    if pending.kind == WriteKind::Neutralize {
                        pending.operation.finish_cancelled();
                    } else {
                        pending.operation.succeed(OperationValue::Unit);
                    }
                }
                Err(error) => {
                    self.desired_report.reset();
                    self.update_desired_snapshot();
                    self.set_state(
                        ControllerState::Disconnected,
                        "controller.disconnected",
                        None,
                    );
                    self.runtime.publish(
                        EventDraft::critical(
                            EventKind::Warning,
                            "controller.neutralization.not_delivered",
                            Severity::Warning,
                        )
                        .with_resource(self.resource_id)
                        .with_operation(pending.operation.id())
                        .with_detail(error.to_string()),
                    );
                    if pending.operation.snapshot().state == OperationState::Cancelling {
                        pending.operation.finish_cancelled();
                    } else {
                        pending.operation.fail(map_transport_error(error));
                    }
                    self.fail_pending_disconnected();
                }
            }
        }
    }

    fn write_payload(
        &mut self,
        operation_id: Option<OperationId>,
        kind: WriteKind,
        timestamp_ns: u64,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
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
            total_len: bytes.len(),
            kind,
        };
        let mut written = 0;
        while written < bytes.len() {
            let accepted = self.transport.write(context, &bytes[written..])?;
            if accepted == 0 || accepted > bytes.len() - written {
                return Err(TransportError::new(
                    TransportErrorKind::Io,
                    "transport returned an invalid partial-write length",
                ));
            }
            written += accepted;
        }
        Ok(())
    }

    fn record_report_acceptance(
        &self,
        timestamp_ns: u64,
        report: SwitchReport,
        operation_id: OperationId,
        bytes: [u8; 8],
    ) {
        let mut snapshot = self
            .snapshot
            .lock()
            .expect("controller snapshot lock poisoned");
        snapshot.desired_report = report;
        snapshot.accepted_report_count = snapshot
            .accepted_report_count
            .checked_add(1)
            .expect("accepted report counter exhausted");
        snapshot.last_report_timestamp_ns = Some(timestamp_ns);
        drop(snapshot);
        self.runtime.publish(
            EventDraft::ordinary(
                EventKind::Data,
                "controller.report.transport_accepted",
                Severity::Info,
            )
            .with_resource(self.resource_id)
            .with_operation(operation_id)
            .with_detail(format!("bytes={bytes:02x?}; hardware_execution=false")),
        );
    }

    fn fail_pending_disconnected(&mut self) {
        for pending in self.pending_reports.drain(..) {
            if pending.operation.snapshot().state == OperationState::Cancelling {
                pending.operation.finish_cancelled();
            } else {
                pending.operation.fail(EasyConError::new(
                    ErrorDomain::Io,
                    ErrorCode::DeviceDisconnected,
                    "controller disconnected before report acceptance",
                ));
            }
        }
        self.transport.close();
    }

    fn close_lane(&mut self, completed: Option<SyncSender<()>>) {
        let was_connected = self.state() == ControllerState::Connected;
        if was_connected {
            self.set_state(
                ControllerState::Disconnecting,
                "controller.disconnecting",
                None,
            );
        }
        let operations: Vec<_> = self
            .pending_reports
            .drain(..)
            .map(|pending| pending.operation)
            .collect();
        for operation in &operations {
            if !operation.snapshot().state.is_terminal() {
                operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
            }
        }
        self.desired_report.reset();
        self.update_desired_snapshot();
        if was_connected {
            let bytes = SwitchReport::NEUTRAL.encode();
            let now = self.clock.now_ns();
            if let Err(error) = self.write_payload(None, WriteKind::Neutralize, now, &bytes) {
                self.runtime.publish(
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
        for operation in operations {
            if operation.snapshot().state == OperationState::Cancelling {
                operation.finish_cancelled();
            }
        }
        self.transport.close();
        self.set_state(ControllerState::Closed, "controller.closed", None);
        if let Some(completed) = completed {
            let _ = completed.send(());
        }
    }

    fn state(&self) -> ControllerState {
        self.snapshot
            .lock()
            .expect("controller snapshot lock poisoned")
            .state
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
        self.runtime.publish(event);
    }

    fn update_desired_snapshot(&self) {
        self.snapshot
            .lock()
            .expect("controller snapshot lock poisoned")
            .desired_report = self.desired_report;
    }
}

fn map_transport_error(error: TransportError) -> EasyConError {
    let (domain, code) = match error.kind() {
        TransportErrorKind::Timeout => (ErrorDomain::Controller, ErrorCode::ProtocolTimeout),
        TransportErrorKind::Cancelled => (ErrorDomain::Runtime, ErrorCode::Cancelled),
        TransportErrorKind::Disconnected => (ErrorDomain::Io, ErrorCode::DeviceDisconnected),
        TransportErrorKind::Io => (ErrorDomain::Io, ErrorCode::Transport),
        TransportErrorKind::Protocol => (ErrorDomain::Controller, ErrorCode::ProtocolError),
    };
    EasyConError::new(domain, code, error.message())
}

fn fail_closed_lane(operation: &Operation) {
    operation.fail(EasyConError::new(
        ErrorDomain::Controller,
        ErrorCode::DeviceDisconnected,
        "controller lane is closed",
    ));
}
