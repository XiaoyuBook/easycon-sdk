use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::sequence::PreciseSequence;
use crate::transport::{
    AUTO_BAUD_RATES, AckRequest, ControllerTransport, HANDSHAKE_REPLY, HANDSHAKE_REQUEST,
    HandshakeRequest, TransportError, TransportErrorKind, WriteContext, WriteKind,
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

/// Exclusive primitive reserved for the future Automation adapter.
pub struct AutomationLease {
    controller_id: ResourceId,
    lease_id: u64,
    sender: Sender<LaneCommand>,
    active: AtomicBool,
}

impl AutomationLease {
    /// Controller resource that issued this lease.
    #[must_use]
    pub const fn controller_id(&self) -> ResourceId {
        self.controller_id
    }

    /// Runtime-local lease generation.
    #[must_use]
    pub const fn lease_id(&self) -> u64 {
        self.lease_id
    }

    /// Explicitly releases the lease. Dropping it performs the same action.
    pub fn release(self) {
        self.release_inner();
    }

    fn release_inner(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            let _ = self.sender.send(LaneCommand::ReleaseAutomationLease {
                lease_id: self.lease_id,
            });
        }
    }
}

impl Drop for AutomationLease {
    fn drop(&mut self) {
        self.release_inner();
    }
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
    resource_cancellation: easycon_runtime::CancellationToken,
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
        lease_access: LeaseAccess,
    },
    Sequence {
        operation: Operation,
        sequence: PreciseSequence,
    },
    Ack {
        operation: Operation,
        command: Arc<[u8]>,
        expected_reply: u8,
        protocol_timeout_ns: u64,
    },
    AcquireAutomationLease {
        lease_id: u64,
        completed: SyncSender<Result<(), EasyConError>>,
    },
    ReleaseAutomationLease {
        lease_id: u64,
    },
    Wake,
    Close {
        completed: SyncSender<()>,
    },
}

#[derive(Clone, Copy)]
enum LeaseAccess {
    Direct,
    Automation(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeaseOwner {
    Sequence(OperationId),
    Automation(u64),
}

struct WaitingSequence {
    operation: Operation,
    sequence: PreciseSequence,
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
    report: SwitchReport,
    kind: WriteKind,
    completion: ReportCompletion,
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
    resource_cancellation: easycon_runtime::CancellationToken,
    lease_owner: Option<LeaseOwner>,
    waiting_sequence: Option<WaitingSequence>,
    next_ack_generation: u64,
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
        let resource_cancellation = runtime.child_cancellation_token();
        let inner = Arc::new(ControllerInner {
            runtime: runtime.clone(),
            resource_id: OnceLock::new(),
            sender: sender.clone(),
            snapshot: snapshot.clone(),
            worker: Mutex::new(WorkerState::Starting),
            worker_ready: Condvar::new(),
            close_gate: Mutex::new(()),
            registration: Mutex::new(None),
            resource_cancellation: resource_cancellation.clone(),
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
            resource_cancellation,
            lease_owner: None,
            waiting_sequence: None,
            next_ack_generation: 1,
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
        self.submit_direct(action, LeaseAccess::Direct)
    }

    /// Submits an action authorized by a matching Automation primitive lease.
    pub fn direct_with_lease(
        &self,
        lease: &AutomationLease,
        action: ControllerAction,
    ) -> Result<Operation, EasyConError> {
        if lease.controller_id != self.id() || !lease.active.load(Ordering::Acquire) {
            return Err(EasyConError::new(
                ErrorDomain::Controller,
                ErrorCode::InvalidArgument,
                "Automation lease does not belong to this active controller",
            ));
        }
        self.submit_direct(action, LeaseAccess::Automation(lease.lease_id))
    }

    /// Submits a validated precise sequence with an exclusive write lease.
    pub fn precise_sequence(&self, sequence: PreciseSequence) -> Result<Operation, EasyConError> {
        let operation = self.inner.runtime.create_operation(None)?;
        self.attach_wake(&operation);
        if self
            .inner
            .sender
            .send(LaneCommand::Sequence {
                operation: operation.clone(),
                sequence,
            })
            .is_err()
        {
            fail_closed_lane(&operation);
        }
        Ok(operation)
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
        let operation = self.inner.runtime.create_operation(None)?;
        self.attach_wake(&operation);
        if self
            .inner
            .sender
            .send(LaneCommand::Ack {
                operation: operation.clone(),
                command,
                expected_reply,
                protocol_timeout_ns,
            })
            .is_err()
        {
            fail_closed_lane(&operation);
        }
        Ok(operation)
    }

    /// Acquires the low-level Automation arbitration primitive without implementing ECS.
    pub fn acquire_automation_lease(&self) -> Result<AutomationLease, EasyConError> {
        static NEXT_LEASE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let lease_id = NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed);
        if lease_id == 0 {
            return Err(EasyConError::new(
                ErrorDomain::Internal,
                ErrorCode::Internal,
                "Automation lease ID space exhausted",
            ));
        }
        let (completed, receiver) = mpsc::sync_channel(0);
        self.inner
            .sender
            .send(LaneCommand::AcquireAutomationLease {
                lease_id,
                completed,
            })
            .map_err(|_| closed_lane_error())?;
        receiver.recv().map_err(|_| closed_lane_error())??;
        Ok(AutomationLease {
            controller_id: self.id(),
            lease_id,
            sender: self.inner.sender.clone(),
            active: AtomicBool::new(true),
        })
    }

    fn submit_direct(
        &self,
        action: ControllerAction,
        lease_access: LeaseAccess,
    ) -> Result<Operation, EasyConError> {
        let operation = self.inner.runtime.create_operation(None)?;
        self.attach_wake(&operation);
        if self
            .inner
            .sender
            .send(LaneCommand::Direct {
                operation: operation.clone(),
                action,
                lease_access,
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
        self.resource_cancellation.cancel();
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
            self.activate_waiting_sequence();

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
                LaneCommand::Direct {
                    operation,
                    action,
                    lease_access,
                } => {
                    self.handle_direct(operation, action, lease_access);
                }
                LaneCommand::Sequence {
                    operation,
                    sequence,
                } => {
                    self.handle_sequence(operation, sequence);
                }
                LaneCommand::Ack {
                    operation,
                    command,
                    expected_reply,
                    protocol_timeout_ns,
                } => {
                    self.handle_ack(operation, command, expected_reply, protocol_timeout_ns);
                }
                LaneCommand::AcquireAutomationLease {
                    lease_id,
                    completed,
                } => {
                    let _ = completed.send(self.acquire_automation_lease(lease_id));
                }
                LaneCommand::ReleaseAutomationLease { lease_id } => {
                    self.release_automation_lease(lease_id);
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
                resource_cancellation: self.resource_cancellation.clone(),
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

    fn handle_direct(
        &mut self,
        operation: Operation,
        action: ControllerAction,
        lease_access: LeaseAccess,
    ) {
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
        let lease_allowed = match (self.lease_owner, lease_access) {
            (None, LeaseAccess::Direct) => true,
            (Some(LeaseOwner::Automation(owner)), LeaseAccess::Automation(access)) => {
                owner == access
            }
            _ => false,
        };
        if !lease_allowed {
            operation.fail(resource_busy_error("controller write lease is owned"));
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
            completion: ReportCompletion::Direct,
        });
    }

    fn handle_sequence(&mut self, operation: Operation, sequence: PreciseSequence) {
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
        if self.lease_owner.is_some() {
            operation.fail(resource_busy_error("controller write lease is owned"));
            return;
        }
        self.set_lease_owner(Some(LeaseOwner::Sequence(operation.id())));
        let waiting = WaitingSequence {
            operation,
            sequence,
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
        if waiting.operation.snapshot().state == OperationState::Cancelling {
            self.schedule_cancel_cleanup(waiting.operation, true);
        } else {
            self.schedule_sequence(waiting);
        }
    }

    fn schedule_sequence(&mut self, waiting: WaitingSequence) {
        let origin_ns = self.clock.now_ns();
        let mut groups = waiting
            .sequence
            .steps()
            .chunk_by(|left, right| left.offset_ns == right.offset_ns);
        let group_count = groups.clone().count();
        let mut previous_due = self
            .last_dispatch_ns
            .map(|last| last.saturating_add(self.options.minimum_report_interval_ns));
        for (index, group) in groups.by_ref().enumerate() {
            for step in group {
                step.action.apply(&mut self.desired_report);
            }
            let Some(target_ns) = origin_ns.checked_add(group[0].offset_ns) else {
                self.pending_reports
                    .retain(|pending| pending.operation.id() != waiting.operation.id());
                self.schedule_failure_cleanup(
                    waiting.operation,
                    EasyConError::new(
                        ErrorDomain::Validation,
                        ErrorCode::InvalidArgument,
                        "sequence target overflowed monotonic time",
                    ),
                    true,
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
                report: self.desired_report,
                kind: WriteKind::Report,
                completion: ReportCompletion::Sequence {
                    final_report: index + 1 == group_count,
                },
            });
        }
        self.update_desired_snapshot();
    }

    fn handle_ack(
        &mut self,
        operation: Operation,
        command: Arc<[u8]>,
        expected_reply: u8,
        protocol_timeout_ns: u64,
    ) {
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
        if self.lease_owner.is_some()
            || self.waiting_sequence.is_some()
            || !self.pending_reports.is_empty()
        {
            operation.fail(resource_busy_error(
                "controller lane has pending report work",
            ));
            return;
        }

        let generation = self.next_ack_generation;
        self.next_ack_generation = self
            .next_ack_generation
            .checked_add(1)
            .expect("ACK generation exhausted");
        let now = self.clock.now_ns();
        if let Err(error) =
            self.write_payload(Some(operation.id()), WriteKind::Command, now, &command)
        {
            operation.fail(map_transport_error(error));
            return;
        }
        let deadline_ns = now.saturating_add(protocol_timeout_ns);
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
                    self.runtime.publish(
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
                    operation.succeed(OperationValue::Unit);
                    return;
                }
                Ok(_) => {
                    operation.fail(EasyConError::new(
                        ErrorDomain::Controller,
                        ErrorCode::ProtocolError,
                        "ACK generation or reply byte did not match",
                    ));
                    return;
                }
                Err(error) if error.kind() == TransportErrorKind::Cancelled => {
                    if operation.snapshot().state != OperationState::Cancelling {
                        operation.request_cancel(easycon_runtime::CancellationReason::ParentClose);
                    }
                    operation.finish_cancelled();
                    return;
                }
                Err(error) => {
                    if error.kind() == TransportErrorKind::Disconnected {
                        self.set_state(
                            ControllerState::Disconnected,
                            "controller.disconnected",
                            None,
                        );
                    }
                    operation.fail(map_transport_error(error));
                    return;
                }
            }
        }
    }

    fn acquire_automation_lease(&mut self, lease_id: u64) -> Result<(), EasyConError> {
        if self.state() != ControllerState::Connected {
            return Err(EasyConError::new(
                ErrorDomain::Controller,
                ErrorCode::DeviceDisconnected,
                "controller is not connected",
            ));
        }
        if self.lease_owner.is_some()
            || self.waiting_sequence.is_some()
            || !self.pending_reports.is_empty()
        {
            return Err(resource_busy_error("controller write lease is owned"));
        }
        self.set_lease_owner(Some(LeaseOwner::Automation(lease_id)));
        Ok(())
    }

    fn release_automation_lease(&mut self, lease_id: u64) {
        if self.lease_owner == Some(LeaseOwner::Automation(lease_id)) {
            self.set_lease_owner(None);
        }
    }

    fn observe_cancellation(&mut self) {
        if self.pending_reports.is_empty()
            && self.waiting_sequence.as_ref().is_some_and(|waiting| {
                waiting.operation.snapshot().state == OperationState::Cancelling
            })
        {
            let waiting = self
                .waiting_sequence
                .take()
                .expect("waiting sequence checked above");
            self.schedule_cancel_cleanup(waiting.operation, true);
            return;
        }
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
        let operation_id = cancelled.operation.id();
        let release_sequence = self.lease_owner == Some(LeaseOwner::Sequence(operation_id));
        self.pending_reports
            .retain(|pending| pending.operation.id() != operation_id);
        self.schedule_cancel_cleanup(cancelled.operation, release_sequence);
    }

    fn schedule_cancel_cleanup(&mut self, operation: Operation, release_sequence: bool) {
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
            operation,
            report: SwitchReport::NEUTRAL,
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
            operation,
            report: SwitchReport::NEUTRAL,
            kind: WriteKind::Neutralize,
            completion: ReportCompletion::Failed {
                error,
                release_sequence,
            },
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
                self.publish_timing_deviation(pending.operation.id(), pending.target_ns, earliest);
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
                    match pending.completion {
                        ReportCompletion::Direct => {
                            pending.operation.succeed(OperationValue::Unit);
                        }
                        ReportCompletion::Sequence {
                            final_report: false,
                        } => {}
                        ReportCompletion::Sequence { final_report: true } => {
                            self.release_sequence(pending.operation.id());
                            pending.operation.succeed(OperationValue::Unit);
                        }
                        ReportCompletion::Cancelled { release_sequence } => {
                            if release_sequence {
                                self.release_sequence(pending.operation.id());
                            }
                            pending.operation.finish_cancelled();
                        }
                        ReportCompletion::Failed {
                            error,
                            release_sequence,
                        } => {
                            if release_sequence {
                                self.release_sequence(pending.operation.id());
                            }
                            pending.operation.fail(error);
                        }
                    }
                }
                Err(error) => {
                    self.handle_report_write_failure(pending, error);
                }
            }
        }
    }

    fn handle_report_write_failure(&mut self, pending: ScheduledReport, error: TransportError) {
        let operation_id = pending.operation.id();
        let release_sequence = self.lease_owner == Some(LeaseOwner::Sequence(operation_id));
        self.pending_reports
            .retain(|queued| queued.operation.id() != operation_id);
        self.desired_report.reset();
        self.update_desired_snapshot();

        if pending.kind == WriteKind::Neutralize || error.kind() == TransportErrorKind::Disconnected
        {
            if error.kind() == TransportErrorKind::Disconnected {
                self.set_state(
                    ControllerState::Disconnected,
                    "controller.disconnected",
                    None,
                );
            }
            self.publish_neutralization_warning(Some(operation_id), &error);
            if release_sequence {
                self.release_sequence(operation_id);
            }
            match pending.completion {
                ReportCompletion::Cancelled { .. } => {
                    pending.operation.finish_cancelled();
                }
                ReportCompletion::Failed { error, .. } => {
                    pending.operation.fail(error);
                }
                ReportCompletion::Direct | ReportCompletion::Sequence { .. } => {
                    pending.operation.fail(map_transport_error(error));
                }
            }
            if self.state() == ControllerState::Disconnected {
                self.fail_pending_disconnected();
            }
            return;
        }

        self.schedule_failure_cleanup(
            pending.operation,
            map_transport_error(error),
            release_sequence,
        );
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
        let pending_reports: Vec<_> = self.pending_reports.drain(..).collect();
        for pending in pending_reports {
            let operation_id = pending.operation.id();
            if self.lease_owner == Some(LeaseOwner::Sequence(operation_id)) {
                self.set_lease_owner(None);
            }
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
        if let Some(waiting) = self.waiting_sequence.take() {
            self.release_sequence(waiting.operation.id());
            waiting.operation.fail(EasyConError::new(
                ErrorDomain::Io,
                ErrorCode::DeviceDisconnected,
                "controller disconnected before sequence dispatch",
            ));
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
        let mut operations: Vec<_> = self
            .pending_reports
            .drain(..)
            .map(|pending| pending.operation)
            .collect();
        if let Some(waiting) = self.waiting_sequence.take() {
            operations.push(waiting.operation);
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
        self.set_lease_owner(None);
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
        self.runtime.publish(
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
        self.runtime.publish(
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
        self.runtime.publish(event);
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
    operation.fail(closed_lane_error());
}

fn closed_lane_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Controller,
        ErrorCode::DeviceDisconnected,
        "controller lane is closed",
    )
}

fn resource_busy_error(message: &'static str) -> EasyConError {
    EasyConError::new(ErrorDomain::Controller, ErrorCode::ResourceBusy, message)
}
