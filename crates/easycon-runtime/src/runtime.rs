use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

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
        Self {
            inner: Arc::new(RuntimeInner {
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
            }),
        }
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
        Ok(operation)
    }

    /// Requests cancellation for operations whose execution deadline has elapsed.
    pub fn poll_deadlines(&self) -> usize {
        let now = self.inner.clock.now_ns();
        let operations: Vec<_> = self
            .inner
            .operations
            .lock()
            .expect("operation registry lock poisoned")
            .values()
            .cloned()
            .collect();
        operations
            .into_iter()
            .map(|inner| Operation { inner })
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
                let _ = operation.finish_cancelled();
            }
        }
        drop(operations);

        let stopped_counts = self.counts();
        assert_eq!(
            stopped_counts,
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            },
            "supervised work remained after deterministic close"
        );

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
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry lock poisoned");
        subscriptions.retain(|subscription| {
            let Some(subscription) = subscription.upgrade() else {
                return false;
            };
            subscription.enqueue(event.clone());
            true
        });
        event
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
    use std::sync::{Arc, Barrier, Mutex};

    use crate::{
        EventClass, EventDraft, EventGap, EventKind, OperationState, OperationValue, Severity,
        SubscriptionRead, TransitionOutcome, VirtualClock, WaitResult, WaitTimeout,
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

        clock.advance_to(50);
        assert_eq!(runtime.poll_deadlines(), 1);
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
