use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;

use easycon_model::{
    EasyConError, ErrorCode, ErrorDomain, OperationId, ResourceId, RuntimeId, TaskId,
};

use crate::cancellation::CancellationToken;
use crate::clock::Clock;
use crate::event::{
    Event, EventDraft, EventKind, EventSubscription, Severity, SubscriptionInner,
    SubscriptionOptions,
};
use crate::operation::{CancellationReason, Operation, OperationInner};

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// Root lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    /// Accepting new operations and resources.
    Active,
    /// Rejecting admission while supervised children stop.
    Closing,
    /// All supervised task, resource, and operation registries are empty.
    Closed,
}

/// Debug counters used by lifecycle conformance tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCounts {
    /// Non-terminal operations still supervised by the Runtime.
    pub active_operations: usize,
    /// Active resources registered for deterministic close.
    pub active_resources: usize,
    /// Active task guards owned by supervised workers.
    pub active_tasks: usize,
}

/// Active resource callback invoked by Runtime close.
pub trait ManagedResource: Send + Sync + 'static {
    /// Cancels, neutralizes where applicable, and joins the resource's work.
    fn close(&self);
}

/// Cloneable root owner for operations, resources, tasks, events, and cancellation.
#[derive(Clone)]
pub struct Runtime {
    pub(crate) inner: Arc<RuntimeInner>,
}

pub(crate) struct RuntimeInner {
    id: RuntimeId,
    state: Mutex<RuntimeState>,
    state_changed: Condvar,
    root_cancellation: CancellationToken,
    clock: Arc<dyn Clock>,
    next_id: AtomicU64,
    next_sequence: AtomicU64,
    operations: Mutex<HashMap<OperationId, Arc<OperationInner>>>,
    resources: Mutex<HashMap<ResourceId, Weak<dyn ManagedResource>>>,
    tasks: Mutex<HashSet<TaskId>>,
    subscriptions: Mutex<Vec<Weak<SubscriptionInner>>>,
    deadline_sender: Sender<DeadlineSignal>,
    deadline_worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Copy)]
enum DeadlineSignal {
    Wake,
    Shutdown,
}

/// RAII registration removed by an active resource after it has really closed.
pub struct ResourceRegistration {
    runtime: Weak<RuntimeInner>,
    id: ResourceId,
    released: AtomicBool,
}

impl ResourceRegistration {
    /// Returns the Runtime-local resource identifier.
    #[must_use]
    pub const fn id(&self) -> ResourceId {
        self.id
    }

    /// Removes the resource from supervision. Calling this repeatedly is harmless.
    pub fn unregister(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(runtime) = self.runtime.upgrade() {
            runtime
                .resources
                .lock()
                .expect("resource registry lock poisoned")
                .remove(&self.id);
        }
    }
}

impl Drop for ResourceRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

/// RAII task entry dropped only after a supervised worker has exited or joined.
pub struct TaskRegistration {
    runtime: Weak<RuntimeInner>,
    id: TaskId,
    released: AtomicBool,
}

impl TaskRegistration {
    /// Returns the Runtime-local task identifier.
    #[must_use]
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Removes the task after its worker has really exited. Calling repeatedly is harmless.
    pub fn unregister(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(runtime) = self.runtime.upgrade() {
            runtime
                .tasks
                .lock()
                .expect("task registry lock poisoned")
                .remove(&self.id);
        }
    }
}

impl Drop for TaskRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

impl Runtime {
    /// Creates an active Runtime without scanning or opening hardware.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let runtime_number = NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed);
        assert!(runtime_number != 0, "Runtime ID space exhausted");
        let (deadline_sender, deadline_receiver) = mpsc::channel();
        let inner = Arc::new(RuntimeInner {
            id: RuntimeId::new(runtime_number),
            state: Mutex::new(RuntimeState::Active),
            state_changed: Condvar::new(),
            root_cancellation: CancellationToken::root(),
            clock,
            next_id: AtomicU64::new(1),
            next_sequence: AtomicU64::new(1),
            operations: Mutex::new(HashMap::new()),
            resources: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashSet::new()),
            subscriptions: Mutex::new(Vec::new()),
            deadline_sender: deadline_sender.clone(),
            deadline_worker: Mutex::new(None),
        });

        let task_id = TaskId::new(inner.allocate_id());
        inner
            .tasks
            .lock()
            .expect("task registry lock poisoned")
            .insert(task_id);
        let deadline_task = TaskRegistration {
            runtime: Arc::downgrade(&inner),
            id: task_id,
            released: AtomicBool::new(false),
        };
        let clock_sender = deadline_sender;
        inner.clock.on_change(Arc::new(move || {
            let _ = clock_sender.send(DeadlineSignal::Wake);
        }));
        let runtime = Arc::downgrade(&inner);
        let worker = std::thread::Builder::new()
            .name(format!("easycon-deadline-{runtime_number}"))
            .spawn(move || deadline_worker(runtime, deadline_receiver, deadline_task))
            .expect("failed to start Runtime deadline worker");
        *inner
            .deadline_worker
            .lock()
            .expect("deadline worker lock poisoned") = Some(worker);

        Self { inner }
    }

    /// Returns the process-unique Runtime identifier.
    #[must_use]
    pub fn id(&self) -> RuntimeId {
        self.inner.id
    }

    /// Returns the authoritative lifecycle state.
    #[must_use]
    pub fn state(&self) -> RuntimeState {
        *self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned")
    }

    /// Returns the Runtime monotonic clock.
    #[must_use]
    pub fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.inner.clock)
    }

    /// Returns a child token cancelled by Runtime close.
    #[must_use]
    pub fn child_cancellation_token(&self) -> CancellationToken {
        self.inner.root_cancellation.child()
    }

    /// Creates and supervises a pending operation.
    pub fn create_operation(&self, deadline_ns: Option<u64>) -> Result<Operation, EasyConError> {
        let state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        ensure_active(*state)?;
        let id = OperationId::new(self.inner.allocate_id());
        let operation = Operation::new(
            id,
            Arc::downgrade(&self.inner),
            self.inner.root_cancellation.child(),
            deadline_ns,
        );
        if let Some(deadline) = deadline_ns {
            self.inner.clock.register_deadline(deadline);
        }
        self.inner
            .operations
            .lock()
            .expect("operation registry lock poisoned")
            .insert(id, Arc::clone(&operation.inner));
        drop(state);
        if deadline_ns.is_some() {
            let _ = self.inner.deadline_sender.send(DeadlineSignal::Wake);
        }
        Ok(operation)
    }

    /// Requests cancellation for operations whose execution deadline has elapsed.
    pub fn poll_deadlines(&self) -> usize {
        self.inner.poll_deadlines()
    }

    /// Creates an independent bounded pull subscription.
    pub fn subscribe(
        &self,
        options: SubscriptionOptions,
    ) -> Result<EventSubscription, EasyConError> {
        if options.capacity == 0 {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "event subscription capacity must be non-zero",
            ));
        }
        let state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        ensure_active(*state)?;
        let subscription = EventSubscription::new(options);
        self.inner
            .subscriptions
            .lock()
            .expect("subscription registry lock poisoned")
            .push(Arc::downgrade(&subscription.inner));
        drop(state);
        Ok(subscription)
    }

    /// Publishes typed domain data to all matching subscriptions without blocking producers.
    pub fn publish(&self, draft: EventDraft) -> Event {
        self.inner.publish_event(draft)
    }

    /// Registers an active resource for Runtime-owned shutdown.
    pub fn register_resource(
        &self,
        resource: Arc<dyn ManagedResource>,
    ) -> Result<ResourceRegistration, EasyConError> {
        let state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        ensure_active(*state)?;
        let id = ResourceId::new(self.inner.allocate_id());
        self.inner
            .resources
            .lock()
            .expect("resource registry lock poisoned")
            .insert(id, Arc::downgrade(&resource));
        drop(state);
        Ok(ResourceRegistration {
            runtime: Arc::downgrade(&self.inner),
            id,
            released: AtomicBool::new(false),
        })
    }

    /// Atomically registers an active resource and the worker task it owns.
    ///
    /// This prevents shutdown from observing a half-admitted active resource during construction.
    pub fn register_resource_with_task(
        &self,
        resource: Arc<dyn ManagedResource>,
    ) -> Result<(ResourceRegistration, TaskRegistration), EasyConError> {
        let state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        ensure_active(*state)?;
        let resource_id = ResourceId::new(self.inner.allocate_id());
        let task_id = TaskId::new(self.inner.allocate_id());
        self.inner
            .resources
            .lock()
            .expect("resource registry lock poisoned")
            .insert(resource_id, Arc::downgrade(&resource));
        self.inner
            .tasks
            .lock()
            .expect("task registry lock poisoned")
            .insert(task_id);
        drop(state);
        Ok((
            ResourceRegistration {
                runtime: Arc::downgrade(&self.inner),
                id: resource_id,
                released: AtomicBool::new(false),
            },
            TaskRegistration {
                runtime: Arc::downgrade(&self.inner),
                id: task_id,
                released: AtomicBool::new(false),
            },
        ))
    }

    /// Registers one supervised task. Its guard must outlive the worker.
    pub fn register_task(&self) -> Result<TaskRegistration, EasyConError> {
        let state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        ensure_active(*state)?;
        let id = TaskId::new(self.inner.allocate_id());
        self.inner
            .tasks
            .lock()
            .expect("task registry lock poisoned")
            .insert(id);
        drop(state);
        Ok(TaskRegistration {
            runtime: Arc::downgrade(&self.inner),
            id,
            released: AtomicBool::new(false),
        })
    }

    /// Returns current supervised counts without changing lifecycle.
    #[must_use]
    pub fn counts(&self) -> RuntimeCounts {
        RuntimeCounts {
            active_operations: self
                .inner
                .operations
                .lock()
                .expect("operation registry lock poisoned")
                .len(),
            active_resources: self
                .inner
                .resources
                .lock()
                .expect("resource registry lock poisoned")
                .len(),
            active_tasks: self
                .inner
                .tasks
                .lock()
                .expect("task registry lock poisoned")
                .len(),
        }
    }

    /// Idempotently closes resources, finishes operations, closes events, and enters `Closed`.
    pub fn close(&self) {
        let owner = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("Runtime state lock poisoned");
            loop {
                match *state {
                    RuntimeState::Active => {
                        *state = RuntimeState::Closing;
                        break true;
                    }
                    RuntimeState::Closing => {
                        state = self
                            .inner
                            .state_changed
                            .wait(state)
                            .expect("Runtime state lock poisoned while closing");
                    }
                    RuntimeState::Closed => break false,
                }
            }
        };
        if !owner {
            return;
        }

        self.inner.publish_event(EventDraft::critical(
            EventKind::State,
            "runtime.closing",
            Severity::Info,
        ));
        self.inner.root_cancellation.cancel();

        let operations: Vec<_> = self
            .inner
            .operations
            .lock()
            .expect("operation registry lock poisoned")
            .values()
            .cloned()
            .map(|inner| Operation { inner })
            .collect();
        for operation in &operations {
            if !operation.snapshot().state.is_terminal() {
                let _ = operation.request_cancel(CancellationReason::ParentClose);
            }
        }

        let resources: Vec<_> = self
            .inner
            .resources
            .lock()
            .expect("resource registry lock poisoned")
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for resource in &resources {
            resource.close();
        }
        drop(resources);

        for operation in &operations {
            if !operation.snapshot().state.is_terminal() {
                let _ = operation.finish_cancelled();
            }
        }
        drop(operations);

        self.inner.publish_event(EventDraft::critical(
            EventKind::State,
            "runtime.closed",
            Severity::Info,
        ));
        let subscriptions: Vec<_> = self
            .inner
            .subscriptions
            .lock()
            .expect("subscription registry lock poisoned")
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for subscription in subscriptions {
            subscription.close();
        }

        self.inner.stop_deadline_worker();
        assert_eq!(
            self.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            },
            "supervised work remained after deterministic close"
        );

        let mut state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        *state = RuntimeState::Closed;
        drop(state);
        self.inner.state_changed.notify_all();
        assert_eq!(
            self.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            }
        );
    }
}

impl RuntimeInner {
    fn allocate_id(&self) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        assert!(id != 0, "Runtime-local ID space exhausted");
        id
    }

    pub(crate) fn unregister_operation(&self, id: OperationId) {
        self.operations
            .lock()
            .expect("operation registry lock poisoned")
            .remove(&id);
    }

    pub(crate) fn publish_event(&self, draft: EventDraft) -> Event {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry lock poisoned");
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        assert!(sequence != 0, "event sequence space exhausted");
        let event = Event {
            sequence,
            timestamp_ns: self.clock.now_ns(),
            class: draft.class,
            kind: draft.kind,
            code: draft.code,
            severity: draft.severity,
            operation_id: draft.operation_id,
            resource_id: draft.resource_id,
            detail: draft.detail,
        };
        subscriptions.retain(|subscription| {
            let Some(subscription) = subscription.upgrade() else {
                return false;
            };
            subscription.enqueue(event.clone());
            true
        });
        event
    }

    fn poll_deadlines(&self) -> usize {
        let now = self.clock.now_ns();
        self.live_operations()
            .into_iter()
            .filter(|operation| {
                operation
                    .deadline_ns()
                    .is_some_and(|deadline| deadline <= now)
            })
            .filter(|operation| {
                operation.request_cancel(CancellationReason::Deadline)
                    == crate::operation::TransitionOutcome::Applied
            })
            .count()
    }

    fn next_deadline_ns(&self) -> Option<u64> {
        self.live_operations()
            .into_iter()
            .filter(|operation| !operation.snapshot().state.is_terminal())
            .filter(|operation| {
                matches!(
                    operation.snapshot().state,
                    crate::operation::OperationState::Pending
                        | crate::operation::OperationState::Running
                )
            })
            .filter_map(|operation| operation.deadline_ns())
            .min()
    }

    fn live_operations(&self) -> Vec<Operation> {
        self.operations
            .lock()
            .expect("operation registry lock poisoned")
            .values()
            .cloned()
            .map(|inner| Operation { inner })
            .collect()
    }

    fn stop_deadline_worker(&self) {
        let _ = self.deadline_sender.send(DeadlineSignal::Shutdown);
        let worker = self
            .deadline_worker
            .lock()
            .expect("deadline worker lock poisoned")
            .take();
        if let Some(worker) = worker
            && worker.thread().id() != std::thread::current().id()
        {
            worker
                .join()
                .expect("Runtime deadline worker must not panic");
        }
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.stop_deadline_worker();
    }
}

fn deadline_worker(
    runtime: Weak<RuntimeInner>,
    receiver: Receiver<DeadlineSignal>,
    _task: TaskRegistration,
) {
    loop {
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        runtime.poll_deadlines();
        let wait = runtime
            .next_deadline_ns()
            .and_then(|deadline| runtime.clock.real_wait_duration(deadline));
        drop(runtime);

        let signal = match wait {
            Some(duration) => match receiver.recv_timeout(duration) {
                Ok(signal) => Some(signal),
                Err(RecvTimeoutError::Timeout) => Some(DeadlineSignal::Wake),
                Err(RecvTimeoutError::Disconnected) => None,
            },
            None => receiver.recv().ok(),
        };
        match signal {
            Some(DeadlineSignal::Wake) => {}
            Some(DeadlineSignal::Shutdown) | None => break,
        }
    }
}

fn ensure_active(state: RuntimeState) -> Result<(), EasyConError> {
    if state == RuntimeState::Active {
        Ok(())
    } else {
        Err(EasyConError::new(
            ErrorDomain::Runtime,
            ErrorCode::RuntimeClosing,
            "Runtime is closing or closed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::Duration;

    use crate::{
        EventClass, EventDraft, EventGap, EventKind, OperationState, OperationValue, Severity,
        SubscriptionRead, SystemClock, TransitionOutcome, VirtualClock, WaitResult, WaitTimeout,
    };

    use super::*;

    #[test]
    fn operation_commits_terminal_state_and_event_once() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");

        assert_eq!(operation.start(), TransitionOutcome::Applied);
        assert_eq!(
            operation.succeed(OperationValue::U64(7)),
            TransitionOutcome::Applied
        );
        assert_eq!(
            operation.fail(EasyConError::new(
                ErrorDomain::Internal,
                ErrorCode::Internal,
                "too late",
            )),
            TransitionOutcome::AlreadyTerminal
        );

        let snapshot = operation.snapshot();
        assert_eq!(snapshot.state, OperationState::Succeeded);
        assert_eq!(snapshot.result, Some(OperationValue::U64(7)));
        assert!(snapshot.error.is_none());
        assert_eq!(runtime.counts().active_operations, 0);

        let codes: Vec<_> = std::iter::from_fn(|| match events.read(WaitTimeout::Poll) {
            SubscriptionRead::Event(event) => Some(event.code),
            SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
        })
        .collect();
        assert_eq!(
            codes,
            ["runtime.operation.running", "runtime.operation.succeeded"]
        );
    }

    #[test]
    fn wait_timeout_does_not_cancel_operation() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();

        assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);
        assert_eq!(operation.snapshot().state, OperationState::Running);
        assert!(!operation.cancellation_token().is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent_before_and_after_terminal_commit() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();

        assert_eq!(operation.cancel(), TransitionOutcome::Applied);
        assert_eq!(operation.cancel(), TransitionOutcome::Unchanged);
        assert_eq!(operation.finish_cancelled(), TransitionOutcome::Applied);
        let terminal = operation.snapshot();
        assert_eq!(operation.cancel(), TransitionOutcome::AlreadyTerminal);
        assert_eq!(operation.snapshot(), terminal);
    }

    #[test]
    fn deadline_requests_cancel_but_cleanup_owns_terminal_commit() {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let operation = runtime.create_operation(Some(50)).expect("operation");
        operation.start();
        let (cancelled, observed) = mpsc::channel();
        operation.on_cancel(move || cancelled.send(()).expect("deadline observer remains alive"));

        clock.advance_to(50);
        observed
            .recv_timeout(Duration::from_secs(2))
            .expect("deadline worker requested cancellation");
        assert_eq!(runtime.poll_deadlines(), 0);
        assert_eq!(clock.deadline_trace()[0].target_ns, 50);
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        assert_eq!(
            operation.snapshot().cancellation_reason,
            Some(CancellationReason::Deadline)
        );
        assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);

        assert_eq!(operation.finish_cancelled(), TransitionOutcome::Applied);
        let snapshot = operation.snapshot();
        assert_eq!(snapshot.state, OperationState::Cancelled);
        assert_eq!(
            snapshot.error.expect("deadline error").code(),
            ErrorCode::DeadlineExceeded
        );
    }

    #[test]
    fn system_clock_deadline_needs_no_external_poll() {
        let clock = Arc::new(SystemClock::new());
        let runtime = Runtime::new(clock.clone());
        let deadline = clock.now_ns().saturating_add(20_000_000);
        let operation = runtime.create_operation(Some(deadline)).expect("operation");
        operation.start();
        let (cancelled, observed) = mpsc::channel();
        operation.on_cancel(move || cancelled.send(()).expect("deadline observer remains alive"));

        observed
            .recv_timeout(Duration::from_secs(2))
            .expect("SystemClock deadline worker requested cancellation");
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        assert_eq!(operation.finish_cancelled(), TransitionOutcome::Applied);
    }

    #[test]
    fn cancel_and_success_race_has_one_terminal_path() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let barrier = Arc::new(Barrier::new(3));

        let success_operation = operation.clone();
        let success_barrier = barrier.clone();
        let success = std::thread::spawn(move || {
            success_barrier.wait();
            success_operation.succeed(OperationValue::Unit)
        });
        let cancel_operation = operation.clone();
        let cancel_barrier = barrier.clone();
        let cancel = std::thread::spawn(move || {
            cancel_barrier.wait();
            cancel_operation.cancel()
        });
        barrier.wait();

        let outcomes = [
            success.join().expect("success thread"),
            cancel.join().expect("cancel thread"),
        ];
        if operation.snapshot().state == OperationState::Cancelling {
            assert_eq!(operation.finish_cancelled(), TransitionOutcome::Applied);
        }
        assert!(operation.snapshot().state.is_terminal());
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == TransitionOutcome::Applied)
                .count(),
            1
        );
    }

    #[test]
    fn queue_overflow_reports_gap_without_blocking_or_changing_state() {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let subscription = runtime
            .subscribe(SubscriptionOptions {
                capacity: 2,
                ..SubscriptionOptions::default()
            })
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");

        runtime.publish(EventDraft::ordinary(
            EventKind::Log,
            "test.one",
            Severity::Debug,
        ));
        clock.advance_to(1);
        runtime.publish(EventDraft::ordinary(
            EventKind::Log,
            "test.two",
            Severity::Debug,
        ));
        clock.advance_to(2);
        runtime.publish(EventDraft::ordinary(
            EventKind::Log,
            "test.three",
            Severity::Debug,
        ));

        let SubscriptionRead::Event(gap) = subscription.read(WaitTimeout::Poll) else {
            panic!("expected gap event");
        };
        assert_eq!(
            gap.kind,
            EventKind::Gap(EventGap {
                first_sequence: 1,
                last_sequence: 1,
                dropped_count: 1
            })
        );
        assert_eq!(operation.snapshot().state, OperationState::Pending);
        assert_eq!(subscription.queued_len(), 2);
    }

    #[test]
    fn capacity_one_preserves_latest_terminal_and_query_is_authoritative() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let subscription = runtime
            .subscribe(SubscriptionOptions {
                capacity: 1,
                ..SubscriptionOptions::default()
            })
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");
        runtime.publish(EventDraft::ordinary(
            EventKind::Log,
            "test.log",
            Severity::Debug,
        ));
        operation.start();
        operation.succeed(OperationValue::Unit);

        assert!(matches!(
            subscription.read(WaitTimeout::Poll),
            SubscriptionRead::Event(Event {
                kind: EventKind::Gap(_),
                ..
            })
        ));
        assert!(matches!(
            subscription.read(WaitTimeout::Poll),
            SubscriptionRead::Event(Event {
                code: "runtime.operation.succeeded",
                kind: EventKind::Terminal,
                ..
            })
        ));
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    }

    #[test]
    fn gap_remains_between_earlier_critical_and_later_event() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let subscription = runtime
            .subscribe(SubscriptionOptions {
                capacity: 2,
                ..SubscriptionOptions::default()
            })
            .expect("subscribe");
        runtime.publish(EventDraft::critical(
            EventKind::State,
            "test.critical",
            Severity::Info,
        ));
        runtime.publish(EventDraft::ordinary(
            EventKind::Log,
            "test.dropped",
            Severity::Debug,
        ));
        runtime.publish(EventDraft::ordinary(
            EventKind::Log,
            "test.latest",
            Severity::Debug,
        ));

        let events: Vec<_> = std::iter::from_fn(|| match subscription.read(WaitTimeout::Poll) {
            SubscriptionRead::Event(event) => Some(event),
            SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
        })
        .collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].code, "test.critical");
        assert_eq!(events[1].code, "runtime.event_gap");
        assert_eq!(events[2].code, "test.latest");
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[test]
    fn concurrent_publishers_enqueue_in_global_sequence_order() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let subscription = runtime
            .subscribe(SubscriptionOptions {
                capacity: 512,
                ..SubscriptionOptions::default()
            })
            .expect("subscribe");
        let barrier = Arc::new(Barrier::new(9));
        let publishers: Vec<_> = (0..8)
            .map(|_| {
                let runtime = runtime.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..32 {
                        runtime.publish(EventDraft::ordinary(
                            EventKind::Data,
                            "test.concurrent",
                            Severity::Info,
                        ));
                    }
                })
            })
            .collect();
        barrier.wait();
        for publisher in publishers {
            publisher.join().expect("publisher thread");
        }

        let events: Vec<_> = std::iter::from_fn(|| match subscription.read(WaitTimeout::Poll) {
            SubscriptionRead::Event(event) => Some(event),
            SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
        })
        .collect();
        assert_eq!(events.len(), 256);
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
    }

    #[derive(Default)]
    struct TestResource {
        registration: Mutex<Option<ResourceRegistration>>,
        task: Mutex<Option<TaskRegistration>>,
        closed: AtomicBool,
    }

    impl ManagedResource for TestResource {
        fn close(&self) {
            self.closed.store(true, Ordering::Release);
            self.task.lock().expect("task lock").take();
            self.registration.lock().expect("registration lock").take();
        }
    }

    struct CancellationObservingResource {
        operation: Operation,
        saw_cancelling: AtomicBool,
        registration: Mutex<Option<ResourceRegistration>>,
    }

    impl ManagedResource for CancellationObservingResource {
        fn close(&self) {
            self.saw_cancelling.store(
                self.operation.snapshot().state == OperationState::Cancelling,
                Ordering::Release,
            );
            self.registration.lock().expect("registration lock").take();
        }
    }

    #[test]
    fn close_requests_operation_cancel_before_resource_callbacks() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let resource = Arc::new(CancellationObservingResource {
            operation: operation.clone(),
            saw_cancelling: AtomicBool::new(false),
            registration: Mutex::new(None),
        });
        let managed: Arc<dyn ManagedResource> = resource.clone();
        *resource.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));

        runtime.close();

        assert!(resource.saw_cancelling.load(Ordering::Acquire));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    }

    #[test]
    fn close_is_idempotent_drains_events_and_zeroes_registries() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let resource = Arc::new(TestResource::default());
        let managed: Arc<dyn ManagedResource> = resource.clone();
        *resource.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));
        *resource.task.lock().expect("task lock") = Some(runtime.register_task().expect("task"));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();

        runtime.close();
        runtime.close();

        assert!(resource.closed.load(Ordering::Acquire));
        assert_eq!(runtime.state(), RuntimeState::Closed);
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert_eq!(
            runtime.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            }
        );

        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll) {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("closed queue must not return timeout"),
            }
        }
        assert!(codes.contains(&"runtime.closed"));
        assert_eq!(codes.last(), Some(&"runtime.closed"));
    }

    #[test]
    fn concurrent_close_callers_observe_the_same_closed_state() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let barrier = Arc::new(Barrier::new(3));

        let left_runtime = runtime.clone();
        let left_barrier = barrier.clone();
        let left = std::thread::spawn(move || {
            left_barrier.wait();
            left_runtime.close();
            left_runtime.state()
        });
        let right_runtime = runtime.clone();
        let right_barrier = barrier.clone();
        let right = std::thread::spawn(move || {
            right_barrier.wait();
            right_runtime.close();
            right_runtime.state()
        });
        barrier.wait();

        assert_eq!(left.join().expect("left close"), RuntimeState::Closed);
        assert_eq!(right.join().expect("right close"), RuntimeState::Closed);
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert_eq!(runtime.counts().active_tasks, 0);
    }

    #[test]
    fn subscriptions_are_independent_and_filters_are_local() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let with_logs = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let without_logs = runtime
            .subscribe(SubscriptionOptions {
                include_logs: false,
                ..SubscriptionOptions::default()
            })
            .expect("subscribe");

        runtime.publish(EventDraft::ordinary(
            EventKind::Log,
            "test.log",
            Severity::Info,
        ));
        runtime.publish(EventDraft::critical(
            EventKind::Warning,
            "test.warning",
            Severity::Warning,
        ));

        assert_eq!(with_logs.queued_len(), 2);
        assert_eq!(without_logs.queued_len(), 1);
        assert!(matches!(
            without_logs.read(WaitTimeout::Poll),
            SubscriptionRead::Event(Event {
                code: "test.warning",
                class: EventClass::Critical,
                ..
            })
        ));
    }

    #[test]
    fn dropping_observer_does_not_cancel_supervision() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        drop(operation);

        assert_eq!(runtime.counts().active_operations, 1);
        runtime.close();
        assert_eq!(runtime.counts().active_operations, 0);
    }
}
