use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{JoinHandle, ThreadId};

use easycon_model::{
    EasyConError, ErrorCode, ErrorDomain, OperationId, ResourceId, RuntimeId, TaskId,
};

use crate::cancellation::CancellationToken;
use crate::clock::{Clock, ClockChangeRegistration};
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
/// Dropping the final owning handle delegates shutdown to a dedicated finalizer. Call
/// [`Runtime::close`] when the caller must synchronously observe `Closed`.
pub struct Runtime {
    pub(crate) inner: Arc<RuntimeInner>,
    close_on_drop: bool,
}

pub(crate) struct RuntimeInner {
    id: RuntimeId,
    runtime_handles: AtomicUsize,
    state: Mutex<RuntimeState>,
    state_changed: Condvar,
    close_failed: AtomicBool,
    root_cancellation: CancellationToken,
    clock: Arc<dyn Clock>,
    next_id: AtomicU64,
    next_sequence: AtomicU64,
    operations: Mutex<HashMap<OperationId, Arc<OperationInner>>>,
    resources: Mutex<HashMap<ResourceId, Weak<dyn ManagedResource>>>,
    tasks: Mutex<HashMap<TaskId, Option<ThreadId>>>,
    tasks_changed: Condvar,
    deadline_task_id: TaskId,
    subscriptions: Mutex<Vec<Weak<SubscriptionInner>>>,
    events_closed: AtomicBool,
    deadline_sender: Sender<DeadlineSignal>,
    deadline_worker: Mutex<Option<JoinHandle<()>>>,
    clock_hook: Mutex<Option<ClockChangeRegistration>>,
}

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

    /// Binds this registration to the worker thread before it executes supervised work.
    ///
    /// A bound worker cannot call synchronous [`Runtime::close`], because it cannot both wait for
    /// and unregister itself. Repeated binding on the same thread is harmless.
    pub fn bind_to_current_thread(&self) {
        assert!(
            !self.released.load(Ordering::Acquire),
            "a released task cannot be bound to a worker thread"
        );
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let current = std::thread::current().id();
        let mut tasks = runtime.tasks.lock().expect("task registry lock poisoned");
        let owner = tasks
            .get_mut(&self.id)
            .expect("active task registration is present in Runtime registry");
        match owner {
            Some(existing) => assert_eq!(
                *existing, current,
                "a task registration cannot move between worker threads"
            ),
            None => *owner = Some(current),
        }
    }

    /// Removes the task after its worker has really exited. Calling repeatedly is harmless.
    pub fn unregister(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(runtime) = self.runtime.upgrade() {
            let removed = runtime
                .tasks
                .lock()
                .expect("task registry lock poisoned")
                .remove(&self.id);
            if removed.is_some() {
                runtime.tasks_changed.notify_all();
            }
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
        let runtime_id = RuntimeId::new(runtime_number);
        let next_id = AtomicU64::new(1);
        let deadline_task_number = next_id.fetch_add(1, Ordering::Relaxed);
        assert!(
            deadline_task_number != 0,
            "Runtime-local ID space exhausted"
        );
        let task_id = TaskId::new(deadline_task_number);
        let inner = Arc::new(RuntimeInner {
            id: runtime_id,
            runtime_handles: AtomicUsize::new(1),
            state: Mutex::new(RuntimeState::Active),
            state_changed: Condvar::new(),
            close_failed: AtomicBool::new(false),
            root_cancellation: CancellationToken::root_for_runtime(runtime_id),
            clock,
            next_id,
            next_sequence: AtomicU64::new(1),
            operations: Mutex::new(HashMap::new()),
            resources: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::from([(task_id, None)])),
            tasks_changed: Condvar::new(),
            deadline_task_id: task_id,
            subscriptions: Mutex::new(Vec::new()),
            events_closed: AtomicBool::new(false),
            deadline_sender: deadline_sender.clone(),
            deadline_worker: Mutex::new(None),
            clock_hook: Mutex::new(None),
        });

        let deadline_task = TaskRegistration {
            runtime: Arc::downgrade(&inner),
            id: task_id,
            released: AtomicBool::new(false),
        };
        let clock_sender = deadline_sender;
        let clock_hook = inner.clock.on_change(Arc::new(move || {
            let _ = clock_sender.send(DeadlineSignal::Wake);
        }));
        *inner
            .clock_hook
            .lock()
            .expect("deadline clock hook lock poisoned") = Some(clock_hook);
        let runtime = Arc::downgrade(&inner);
        let worker = std::thread::Builder::new()
            .name(format!("easycon-deadline-{runtime_number}"))
            .spawn(move || deadline_worker(runtime, deadline_receiver, deadline_task))
            .expect("failed to start Runtime deadline worker");
        *inner
            .deadline_worker
            .lock()
            .expect("deadline worker lock poisoned") = Some(worker);

        Self {
            inner,
            close_on_drop: true,
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

    /// Creates a core-internal reference that keeps storage alive without delaying final-handle close.
    #[doc(hidden)]
    #[must_use]
    pub fn clone_for_supervision(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            close_on_drop: false,
        }
    }

    /// Creates and supervises a pending operation.
    pub fn create_operation(&self, deadline_ns: Option<u64>) -> Result<Operation, EasyConError> {
        self.create_operation_with_parent(deadline_ns, &self.inner.root_cancellation)
    }

    /// Creates a pending operation below an active cancellation token owned by this Runtime.
    ///
    /// # Errors
    ///
    /// Returns a validation error when `parent` belongs to another Runtime or has already ended.
    pub fn create_operation_with_parent(
        &self,
        deadline_ns: Option<u64>,
        parent: &CancellationToken,
    ) -> Result<Operation, EasyConError> {
        let state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        ensure_active(*state)?;
        if parent.owner() != Some(self.inner.id) {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "operation cancellation parent belongs to a different Runtime",
            ));
        }
        if !parent.is_active() {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "operation cancellation parent is no longer active",
            ));
        }
        let id = OperationId::new(self.inner.allocate_id());
        let operation =
            Operation::new(id, Arc::downgrade(&self.inner), parent.child(), deadline_ns);
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
        let mut subscriptions = self
            .inner
            .subscriptions
            .lock()
            .expect("subscription registry lock poisoned");
        subscriptions.retain(|existing| existing.strong_count() != 0);
        subscriptions.push(Arc::downgrade(&subscription.inner));
        drop(subscriptions);
        drop(state);
        Ok(subscription)
    }

    /// Publishes typed domain data to all matching subscriptions without blocking producers.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::RuntimeClosing`] after the final Runtime event closes production.
    pub fn publish(&self, draft: EventDraft) -> Result<Event, EasyConError> {
        self.inner.try_publish_event(draft)
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
            .insert(task_id, None);
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
            .insert(id, None);
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
    /// Unlike final-handle drop, this method waits for the full close sequence to finish.
    ///
    /// # Panics
    ///
    /// Panics when an internal close phase fails unexpectedly. Concurrent and later close callers
    /// are woken and report the same failed-close condition instead of waiting indefinitely. Also
    /// panics without changing Runtime state when called from a bound supervised task; such a task
    /// must return and let an external owner close, or rely on final-handle drop.
    pub fn close(&self) {
        assert!(
            !self.inner.current_thread_owns_task(),
            "Runtime::close cannot synchronously wait for its calling supervised task"
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.close_impl()));
        if let Err(payload) = result {
            self.inner.fail_close_after_panic();
            std::panic::resume_unwind(payload);
        }
    }

    fn close_impl(&self) {
        let owner = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("Runtime state lock poisoned");
            loop {
                if self.inner.close_failed.load(Ordering::Acquire) {
                    break None;
                }
                match *state {
                    RuntimeState::Active => {
                        *state = RuntimeState::Closing;
                        break Some(true);
                    }
                    RuntimeState::Closing => {
                        state = self
                            .inner
                            .state_changed
                            .wait(state)
                            .expect("Runtime state lock poisoned while closing");
                    }
                    RuntimeState::Closed => break Some(false),
                }
            }
        };
        let Some(owner) = owner else {
            panic!("Runtime close previously failed");
        };
        if !owner {
            return;
        }

        self.finish_close();
    }

    fn finish_close(&self) {
        let _ = self.inner.try_publish_event(EventDraft::critical(
            EventKind::State,
            "runtime.closing",
            Severity::Info,
        ));
        self.inner.root_cancellation.cancel();

        let mut operations: Vec<_> = self
            .inner
            .operations
            .lock()
            .expect("operation registry lock poisoned")
            .values()
            .cloned()
            .map(|inner| Operation { inner })
            .collect();
        operations.sort_by_key(Operation::id);
        for operation in &operations {
            if !operation.snapshot().state.is_terminal() {
                let _ = operation.request_cancel(CancellationReason::ParentClose);
            }
        }

        let mut resources = {
            let mut registry = self
                .inner
                .resources
                .lock()
                .expect("resource registry lock poisoned");
            let mut stale = Vec::new();
            let mut live = Vec::new();
            for (&id, resource) in registry.iter() {
                if let Some(resource) = resource.upgrade() {
                    live.push((id, resource));
                } else {
                    stale.push(id);
                }
            }
            for id in stale {
                registry.remove(&id);
            }
            live
        };
        resources.sort_by_key(|(id, _)| *id);
        let mut resource_close_panic = None;
        for (id, resource) in &resources {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                resource.close();
            }));
            if let Err(payload) = result {
                let _ = self.inner.try_publish_event(
                    EventDraft::critical(
                        EventKind::Warning,
                        "runtime.resource.close_panicked",
                        Severity::Error,
                    )
                    .with_resource(*id)
                    .with_detail(
                        "ManagedResource::close panicked; registration remains supervised",
                    ),
                );
                if resource_close_panic.is_none() {
                    resource_close_panic = Some(payload);
                }
            }
        }
        drop(resources);
        if let Some(payload) = resource_close_panic {
            std::panic::resume_unwind(payload);
        }

        self.inner.wait_for_external_tasks();

        for operation in &operations {
            if !operation.snapshot().state.is_terminal() {
                let _ = operation.finish_cancelled();
            }
        }
        drop(operations);

        self.inner.stop_deadline_worker();
        self.inner.wait_for_tasks();
        assert_eq!(
            self.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            },
            "supervised work remained after deterministic close"
        );

        self.inner.close_events(EventDraft::critical(
            EventKind::State,
            "runtime.closed",
            Severity::Info,
        ));

        let mut state = self
            .inner
            .state
            .lock()
            .expect("Runtime state lock poisoned");
        *state = RuntimeState::Closed;
        drop(state);
        self.inner.state_changed.notify_all();
    }
}

impl Clone for Runtime {
    fn clone(&self) -> Self {
        if self.close_on_drop {
            self.inner
                .runtime_handles
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |handles| {
                    handles.checked_add(1)
                })
                .expect("Runtime handle count exhausted");
        }
        Self {
            inner: Arc::clone(&self.inner),
            close_on_drop: self.close_on_drop,
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.close_on_drop {
            return;
        }
        let Ok(previous) = self.inner.runtime_handles.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |handles| handles.checked_sub(1),
        ) else {
            return;
        };
        if previous == 1 {
            let finalize = {
                let mut state = match self.inner.state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if *state == RuntimeState::Active {
                    *state = RuntimeState::Closing;
                    true
                } else {
                    false
                }
            };
            if finalize {
                spawn_finalizer(Arc::clone(&self.inner));
            }
        }
    }
}

impl RuntimeInner {
    fn current_thread_owns_task(&self) -> bool {
        let current = std::thread::current().id();
        let tasks = match self.tasks.lock() {
            Ok(tasks) => tasks,
            Err(poisoned) => poisoned.into_inner(),
        };
        tasks
            .values()
            .any(|owner| owner.as_ref().is_some_and(|owner| *owner == current))
    }

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

    pub(crate) fn try_publish_event(&self, draft: EventDraft) -> Result<Event, EasyConError> {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry lock poisoned");
        if self.events_closed.load(Ordering::Acquire) {
            return Err(events_closed_error());
        }
        let event = self.next_event(draft);
        enqueue_event(&mut subscriptions, &event);
        Ok(event)
    }

    fn close_events(&self, draft: EventDraft) {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .expect("subscription registry lock poisoned");
        if self.events_closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let event = self.next_event(draft);
        subscriptions.retain(|subscription| {
            let Some(subscription) = subscription.upgrade() else {
                return false;
            };
            subscription.enqueue_final(event.clone());
            subscription.close();
            true
        });
    }

    fn fail_close_after_panic(&self) {
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            if *state == RuntimeState::Active {
                *state = RuntimeState::Closing;
            }
            self.close_failed.store(true, Ordering::Release);
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.root_cancellation.cancel();
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finish_operations_after_close_failure();
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.stop_deadline_worker();
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.close_events(EventDraft::critical(
                EventKind::State,
                "runtime.close_panicked",
                Severity::Error,
            ));
        }));
        self.state_changed.notify_all();
    }

    fn fail_close_without_finalizer(&self) {
        {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
            if *state == RuntimeState::Active {
                *state = RuntimeState::Closing;
            }
            self.close_failed.store(true, Ordering::Release);
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.root_cancellation.cancel();
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finish_operations_after_close_failure();
        }));
        let _ = self.deadline_sender.send(DeadlineSignal::Shutdown);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.close_events(EventDraft::critical(
                EventKind::State,
                "runtime.close_panicked",
                Severity::Error,
            ));
        }));
        self.state_changed.notify_all();
    }

    fn finish_operations_after_close_failure(&self) {
        let mut operations: Vec<_> = match self.operations.lock() {
            Ok(operations) => operations
                .values()
                .cloned()
                .map(|inner| Operation { inner })
                .collect(),
            Err(poisoned) => poisoned
                .into_inner()
                .values()
                .cloned()
                .map(|inner| Operation { inner })
                .collect(),
        };
        operations.sort_by_key(Operation::id);
        for operation in operations {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = operation.request_cancel(CancellationReason::ParentClose);
            }));
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = operation.finish_cancelled();
            }));
        }
    }

    fn next_event(&self, draft: EventDraft) -> Event {
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        assert!(sequence != 0, "event sequence space exhausted");
        Event {
            sequence,
            timestamp_ns: self.clock.now_ns(),
            class: draft.class,
            kind: draft.kind,
            code: draft.code,
            severity: draft.severity,
            operation_id: draft.operation_id,
            resource_id: draft.resource_id,
            detail: draft.detail,
        }
    }

    fn poll_deadlines(&self) -> usize {
        let now = self.clock.now_ns();
        let mut due: Vec<_> = self
            .live_operations()
            .into_iter()
            .filter(|operation| {
                operation
                    .deadline_ns()
                    .is_some_and(|deadline| deadline <= now)
            })
            .collect();
        due.sort_by_key(|operation| (operation.deadline_ns().unwrap_or(u64::MAX), operation.id()));
        due.into_iter()
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
        let mut operations: Vec<_> = self
            .operations
            .lock()
            .expect("operation registry lock poisoned")
            .values()
            .cloned()
            .map(|inner| Operation { inner })
            .collect();
        operations.sort_by_key(Operation::id);
        operations
    }

    fn stop_deadline_worker(&self) {
        self.clock_hook
            .lock()
            .expect("deadline clock hook lock poisoned")
            .take();
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

    fn wait_for_tasks(&self) {
        let mut tasks = self.tasks.lock().expect("task registry lock poisoned");
        while !tasks.is_empty() {
            tasks = self
                .tasks_changed
                .wait(tasks)
                .expect("task registry lock poisoned while closing");
        }
    }

    fn wait_for_external_tasks(&self) {
        let mut tasks = self.tasks.lock().expect("task registry lock poisoned");
        while tasks.keys().any(|id| *id != self.deadline_task_id) {
            tasks = self
                .tasks_changed
                .wait(tasks)
                .expect("task registry lock poisoned while closing");
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
    task: TaskRegistration,
) {
    task.bind_to_current_thread();
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

fn close_after_final_handle_drop(inner: Arc<RuntimeInner>) {
    let runtime = Runtime {
        inner,
        close_on_drop: false,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.finish_close();
    }));
    if result.is_err() {
        runtime.inner.fail_close_after_panic();
    }
}

fn spawn_finalizer(inner: Arc<RuntimeInner>) {
    let failure_fallback = Arc::clone(&inner);
    if std::thread::Builder::new()
        .name(format!("easycon-finalizer-{}", inner.id.get()))
        .spawn(move || close_after_final_handle_drop(inner))
        .is_err()
    {
        failure_fallback.fail_close_without_finalizer();
        // No thread exists that can safely own deterministic cleanup. Retaining storage is safer
        // than running resource callbacks inline from Drop or freeing state still used by workers.
        std::mem::forget(failure_fallback);
    }
}

fn enqueue_event(subscriptions: &mut Vec<Weak<SubscriptionInner>>, event: &Event) {
    subscriptions.retain(|subscription| {
        let Some(subscription) = subscription.upgrade() else {
            return false;
        };
        subscription.enqueue(event.clone());
        true
    });
}

fn events_closed_error() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Runtime,
        ErrorCode::RuntimeClosing,
        "Runtime event production is closed",
    )
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
    use std::time::{Duration, Instant};

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
    fn terminal_wait_observes_the_operation_already_unregistered() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let observed_runtime = runtime.clone();
        let observed_operation = operation.clone();
        let waiter = std::thread::spawn(move || {
            assert!(matches!(
                observed_operation.wait(WaitTimeout::Infinite),
                WaitResult::Completed(_)
            ));
            observed_runtime.counts().active_operations
        });

        operation.succeed(OperationValue::Unit);

        assert_eq!(waiter.join().expect("operation waiter"), 0);
        runtime.close();
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
    fn runtime_owned_parent_cancels_child_operation_state() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let parent = runtime.child_cancellation_token();
        let operation = runtime
            .create_operation_with_parent(None, &parent)
            .expect("child operation");
        operation.start();

        parent.cancel();

        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        assert_eq!(
            operation.snapshot().cancellation_reason,
            Some(CancellationReason::ParentClose)
        );
        operation.finish_cancelled();
        runtime.close();
    }

    #[test]
    fn operation_parent_must_belong_to_the_same_runtime() {
        let first = Runtime::new(Arc::new(VirtualClock::default()));
        let second = Runtime::new(Arc::new(VirtualClock::default()));
        let parent = first.child_cancellation_token();

        let Err(error) = second.create_operation_with_parent(None, &parent) else {
            panic!("cross-Runtime cancellation parent must be rejected");
        };

        assert_eq!(error.domain(), ErrorDomain::Validation);
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        first.close();
        second.close();
    }

    #[test]
    fn terminal_operation_token_cannot_parent_new_work() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let parent = runtime.create_operation(None).expect("parent operation");
        let parent_token = parent.cancellation_token();
        parent.start();
        parent.succeed(OperationValue::Unit);

        let Err(error) = runtime.create_operation_with_parent(None, &parent_token) else {
            panic!("terminal operation token must not parent new work");
        };

        assert_eq!(error.domain(), ErrorDomain::Validation);
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        runtime.close();
    }

    #[test]
    fn terminal_parent_cancels_an_already_running_child_operation() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let parent = runtime.create_operation(None).expect("parent operation");
        let child = runtime
            .create_operation_with_parent(None, &parent.cancellation_token())
            .expect("child operation");
        parent.start();
        child.start();

        parent.succeed(OperationValue::Unit);

        assert_eq!(parent.snapshot().state, OperationState::Succeeded);
        assert_eq!(child.snapshot().state, OperationState::Cancelling);
        assert_eq!(
            child.snapshot().cancellation_reason,
            Some(CancellationReason::ParentClose)
        );
        child.finish_cancelled();
        runtime.close();
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
    fn cancelling_is_a_state_event_until_cleanup_commits_terminal() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");

        operation.start();
        operation.cancel();
        operation.finish_cancelled();

        let observed: Vec<_> = std::iter::from_fn(|| match events.read(WaitTimeout::Poll) {
            SubscriptionRead::Event(event) => Some(event),
            SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
        })
        .collect();
        assert!(observed.iter().any(|event| {
            event.code == "runtime.operation.cancelling" && event.kind == EventKind::State
        }));
        assert!(observed.iter().all(|event| {
            event.kind != EventKind::Terminal || event.code == "runtime.operation.cancelled"
        }));
    }

    #[test]
    fn terminal_operation_drops_obsolete_cancellation_hooks() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        let (cancelled, observed) = mpsc::channel();
        let before_terminal = cancelled.clone();
        operation.on_cancel(move || {
            let _ = before_terminal.send(());
        });
        operation.start();
        operation.succeed(OperationValue::Unit);
        operation.on_cancel(move || {
            let _ = cancelled.send(());
        });

        runtime.close();

        assert_eq!(observed.try_iter().count(), 0);
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
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

        runtime
            .publish(EventDraft::ordinary(
                EventKind::Log,
                "test.one",
                Severity::Debug,
            ))
            .expect("publish");
        clock.advance_to(1);
        runtime
            .publish(EventDraft::ordinary(
                EventKind::Log,
                "test.two",
                Severity::Debug,
            ))
            .expect("publish");
        clock.advance_to(2);
        runtime
            .publish(EventDraft::ordinary(
                EventKind::Log,
                "test.three",
                Severity::Debug,
            ))
            .expect("publish");

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
        runtime
            .publish(EventDraft::ordinary(
                EventKind::Log,
                "test.log",
                Severity::Debug,
            ))
            .expect("publish");
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
        runtime
            .publish(EventDraft::critical(
                EventKind::State,
                "test.critical",
                Severity::Info,
            ))
            .expect("publish");
        runtime
            .publish(EventDraft::ordinary(
                EventKind::Log,
                "test.dropped",
                Severity::Debug,
            ))
            .expect("publish");
        runtime
            .publish(EventDraft::ordinary(
                EventKind::Log,
                "test.latest",
                Severity::Debug,
            ))
            .expect("publish");

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
                        runtime
                            .publish(EventDraft::ordinary(
                                EventKind::Data,
                                "test.concurrent",
                                Severity::Info,
                            ))
                            .expect("publish");
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

    #[derive(Default)]
    struct PanickingResource {
        registration: Mutex<Option<ResourceRegistration>>,
        task: Mutex<Option<TaskRegistration>>,
    }

    impl ManagedResource for PanickingResource {
        fn close(&self) {
            panic!("scripted resource close panic");
        }
    }

    struct ThreadReportingResource {
        registration: Mutex<Option<ResourceRegistration>>,
        closed_on: mpsc::Sender<std::thread::ThreadId>,
    }

    impl ManagedResource for ThreadReportingResource {
        fn close(&self) {
            let _ = self.closed_on.send(std::thread::current().id());
            self.registration.lock().expect("registration lock").take();
        }
    }

    struct OrderedResource {
        marker: u8,
        order: Arc<Mutex<Vec<u8>>>,
        registration: Mutex<Option<ResourceRegistration>>,
    }

    impl ManagedResource for OrderedResource {
        fn close(&self) {
            self.order
                .lock()
                .expect("close order lock")
                .push(self.marker);
            self.registration.lock().expect("registration lock").take();
        }
    }

    struct AlreadyDroppedResource;

    impl ManagedResource for AlreadyDroppedResource {
        fn close(&self) {
            panic!("a dropped resource cannot be closed");
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
    fn close_uses_runtime_id_order_for_resources_and_terminal_operations() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let close_order = Arc::new(Mutex::new(Vec::new()));
        let mut resources = Vec::new();
        for marker in 1..=3 {
            let resource = Arc::new(OrderedResource {
                marker,
                order: close_order.clone(),
                registration: Mutex::new(None),
            });
            let managed: Arc<dyn ManagedResource> = resource.clone();
            *resource.registration.lock().expect("registration lock") =
                Some(runtime.register_resource(managed).expect("resource"));
            resources.push(resource);
        }
        let operations: Vec<_> = (0..3)
            .map(|_| {
                let operation = runtime.create_operation(None).expect("operation");
                operation.start();
                operation
            })
            .collect();
        let operation_ids: Vec<_> = operations.iter().map(Operation::id).collect();

        runtime.close();

        assert_eq!(*close_order.lock().expect("close order lock"), [1, 2, 3]);
        let terminal_ids: Vec<_> = std::iter::from_fn(|| match events.read(WaitTimeout::Poll) {
            SubscriptionRead::Event(event) => Some(event),
            SubscriptionRead::Closed | SubscriptionRead::Timeout => None,
        })
        .filter(|event| event.code == "runtime.operation.cancelled")
        .filter_map(|event| event.operation_id)
        .collect();
        assert_eq!(terminal_ids, operation_ids);
        drop(resources);
    }

    #[test]
    fn close_prunes_a_registration_whose_weak_resource_already_expired() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let resource: Arc<dyn ManagedResource> = Arc::new(AlreadyDroppedResource);
        let registration = runtime
            .register_resource(resource.clone())
            .expect("resource registration");
        drop(resource);

        runtime.close();

        assert_eq!(runtime.counts().active_resources, 0);
        drop(registration);
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
        let error = runtime
            .publish(EventDraft::ordinary(
                EventKind::Data,
                "test.after_close",
                Severity::Info,
            ))
            .expect_err("event production is closed");
        assert_eq!(error.code(), ErrorCode::RuntimeClosing);
    }

    #[test]
    fn dropping_last_runtime_handle_eventually_closes_operations_and_events() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();

        drop(runtime);

        assert!(matches!(
            operation.wait(WaitTimeout::For(Duration::from_secs(2))),
            WaitResult::Completed(_)
        ));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        let mut last_code = None;
        loop {
            match events.read(WaitTimeout::For(Duration::from_secs(2))) {
                SubscriptionRead::Event(event) => last_code = Some(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("last Runtime drop must close subscriptions"),
            }
        }
        assert_eq!(last_code, Some("runtime.closed"));
    }

    #[test]
    fn final_handle_drop_resource_panic_fails_without_waiting_for_its_task() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let supervised = runtime.clone_for_supervision();
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let panicking = Arc::new(PanickingResource::default());
        let managed: Arc<dyn ManagedResource> = panicking.clone();
        let (registration, task) = runtime
            .register_resource_with_task(managed)
            .expect("resource and task");
        let panicking_id = registration.id();
        *panicking.registration.lock().expect("registration lock") = Some(registration);
        *panicking.task.lock().expect("task lock") = Some(task);
        let healthy = Arc::new(TestResource::default());
        let managed: Arc<dyn ManagedResource> = healthy.clone();
        *healthy.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));

        drop(runtime);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| supervised.close())).is_err(),
            "resource panic must wake close callers with failure"
        );

        assert_eq!(supervised.state(), RuntimeState::Closing);
        assert!(healthy.closed.load(Ordering::Acquire));
        assert!(matches!(
            operation.wait(WaitTimeout::Poll),
            WaitResult::Completed(_)
        ));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert_eq!(
            supervised.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 1,
                active_tasks: 1,
            }
        );
        let mut observed = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll) {
                SubscriptionRead::Event(event) => observed.push(event),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("failed close queue must be closed"),
            }
        }
        assert!(observed.iter().any(|event| {
            event.code == "runtime.resource.close_panicked"
                && event.resource_id == Some(panicking_id)
                && event.severity == Severity::Error
        }));
        assert_eq!(
            observed.last().map(|event| event.code),
            Some("runtime.close_panicked")
        );
        assert!(!observed.iter().any(|event| event.code == "runtime.closed"));

        panicking.task.lock().expect("task lock").take();
        panicking
            .registration
            .lock()
            .expect("registration lock")
            .take();
    }

    #[test]
    fn final_handle_drop_reports_a_deadline_worker_panic_before_success() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let supervised = runtime.clone_for_supervision();
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("events");
        let poisoned_runtime = Arc::clone(&runtime.inner);
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned_runtime
                    .operations
                    .lock()
                    .expect("operation registry lock");
                panic!("scripted operation registry poison");
            })
            .join()
            .is_err()
        );
        let _ = runtime.inner.deadline_sender.send(DeadlineSignal::Wake);
        let started = Instant::now();
        loop {
            let finished = runtime
                .inner
                .deadline_worker
                .lock()
                .expect("deadline worker lock")
                .as_ref()
                .is_some_and(JoinHandle::is_finished);
            if finished {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "deadline worker did not observe the injected panic"
            );
            std::thread::yield_now();
        }

        drop(runtime);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| supervised.close())).is_err(),
            "worker panic must wake close callers with failure"
        );
        assert_eq!(supervised.state(), RuntimeState::Closing);
        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll) {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("failed close queue must be closed"),
            }
        }
        assert_eq!(codes.last(), Some(&"runtime.close_panicked"));
        assert!(!codes.contains(&"runtime.closed"));
    }

    #[test]
    fn unexpected_close_panic_wakes_concurrent_callers_and_closes_events() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let poisoned_runtime = Arc::clone(&runtime.inner);
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned_runtime
                    .operations
                    .lock()
                    .expect("operation registry lock");
                panic!("scripted operation registry poison");
            })
            .join()
            .is_err()
        );
        let barrier = Arc::new(Barrier::new(3));
        let callers: Vec<_> = (0..2)
            .map(|_| {
                let closing = runtime.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| closing.close()))
                        .is_err()
                })
            })
            .collect();

        barrier.wait();
        assert!(
            callers
                .into_iter()
                .all(|caller| caller.join().expect("close caller"))
        );
        assert_eq!(runtime.state(), RuntimeState::Closing);
        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll) {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("failed close queue must be closed"),
            }
        }
        assert_eq!(codes.last(), Some(&"runtime.close_panicked"));
    }

    #[test]
    fn dropping_last_runtime_handle_rejects_admission_before_returning() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let supervised = runtime.clone_for_supervision();
        let tasks = supervised.inner.tasks.lock().expect("task registry lock");

        drop(runtime);

        assert_eq!(supervised.state(), RuntimeState::Closing);
        let error = match supervised.create_operation(None) {
            Err(error) => error,
            Ok(_) => panic!("final handle drop must reject admission synchronously"),
        };
        assert_eq!(error.code(), ErrorCode::RuntimeClosing);
        drop(tasks);
        supervised.close();
        assert_eq!(supervised.state(), RuntimeState::Closed);
    }

    #[test]
    fn dropping_final_runtime_handle_inside_a_supervised_task_does_not_self_deadlock() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let task = runtime.register_task().expect("task");
        let worker_runtime = runtime.clone();
        let (release, released) = mpsc::channel();
        let (finished, observed_finish) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            task.bind_to_current_thread();
            released.recv().expect("worker release");
            drop(worker_runtime);
            drop(task);
            finished.send(()).expect("worker completion");
        });

        drop(runtime);
        release.send(()).expect("release worker");

        observed_finish
            .recv_timeout(Duration::from_secs(2))
            .expect("final Runtime drop must not wait for its own task");
        worker.join().expect("supervised worker");
        let mut last_code = None;
        loop {
            match events.read(WaitTimeout::For(Duration::from_secs(2))) {
                SubscriptionRead::Event(event) => last_code = Some(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("delegated Runtime close did not finish"),
            }
        }
        assert_eq!(last_code, Some("runtime.closed"));
    }

    #[test]
    fn dropping_last_runtime_handle_does_not_close_resources_inline() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let (closed_on, observed_close) = mpsc::channel();
        let resource = Arc::new(ThreadReportingResource {
            registration: Mutex::new(None),
            closed_on,
        });
        let managed: Arc<dyn ManagedResource> = resource.clone();
        *resource.registration.lock().expect("registration lock") = Some(
            runtime
                .register_resource(managed)
                .expect("resource registration"),
        );
        let dropping_thread = std::thread::current().id();

        drop(runtime);

        let close_thread = observed_close
            .recv_timeout(Duration::from_secs(2))
            .expect("delegated resource close");
        assert_ne!(close_thread, dropping_thread);
        loop {
            match events.read(WaitTimeout::For(Duration::from_secs(2))) {
                SubscriptionRead::Event(_) => {}
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("delegated Runtime close did not finish"),
            }
        }
    }

    #[test]
    fn dropping_a_nonfinal_runtime_handle_keeps_the_runtime_active() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let other = runtime.clone();

        drop(other);

        assert_eq!(runtime.state(), RuntimeState::Active);
        runtime.close();
    }

    #[test]
    fn final_runtime_event_bypasses_subscription_severity_filter() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions {
                minimum_severity: Severity::Error,
                ..SubscriptionOptions::default()
            })
            .expect("subscribe");

        runtime.close();

        assert!(matches!(
            events.read(WaitTimeout::Poll),
            SubscriptionRead::Event(Event {
                code: "runtime.closed",
                ..
            })
        ));
        assert_eq!(events.read(WaitTimeout::Poll), SubscriptionRead::Closed);
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
    fn explicit_close_from_a_supervised_task_is_rejected_without_deadlock() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let task = runtime.register_task().expect("task");
        let worker_runtime = runtime.clone();
        let (finished, observed_finish) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            task.bind_to_current_thread();
            let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                worker_runtime.close();
            }))
            .is_err();
            finished.send(rejected).expect("close rejection observer");
            drop(task);
        });

        assert!(
            observed_finish
                .recv_timeout(Duration::from_secs(2))
                .expect("supervised task close must fail fast")
        );
        worker.join().expect("supervised worker");
        assert_eq!(runtime.state(), RuntimeState::Active);

        runtime.close();
        assert_eq!(runtime.state(), RuntimeState::Closed);
    }

    #[test]
    fn close_waits_for_a_supervised_task_to_finish_cleanup() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let cancellation = runtime.child_cancellation_token();
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let task = runtime.register_task().expect("task");
        let (wake, woken) = mpsc::channel();
        cancellation.on_cancel(move || {
            let _ = wake.send(());
        });
        let (cleanup_started, observed_cleanup) = mpsc::channel();
        let (release_cleanup, released) = mpsc::channel();
        let cleanup_operation = operation.clone();
        let worker = std::thread::spawn(move || {
            task.bind_to_current_thread();
            woken.recv().expect("root cancellation");
            cleanup_started.send(()).expect("cleanup observer");
            released.recv().expect("cleanup release");
            cleanup_operation.finish_cancelled();
            drop(task);
        });
        let (closed, observed_close) = mpsc::channel();
        let closing_runtime = runtime.clone();
        let closer = std::thread::spawn(move || {
            closing_runtime.close();
            closed.send(()).expect("close observer");
        });

        observed_cleanup
            .recv_timeout(Duration::from_secs(2))
            .expect("task began cancellation cleanup");
        assert!(matches!(
            observed_close.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        assert_eq!(
            operation.wait(WaitTimeout::For(Duration::from_millis(50))),
            WaitResult::Timeout
        );
        release_cleanup.send(()).expect("release task cleanup");
        observed_close
            .recv_timeout(Duration::from_secs(2))
            .expect("Runtime waited for task cleanup");
        worker.join().expect("supervised worker");
        closer.join().expect("Runtime closer");

        assert_eq!(runtime.state(), RuntimeState::Closed);
        assert_eq!(runtime.counts().active_tasks, 0);
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
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

        runtime
            .publish(EventDraft::ordinary(
                EventKind::Log,
                "test.log",
                Severity::Info,
            ))
            .expect("publish");
        runtime
            .publish(EventDraft::critical(
                EventKind::Warning,
                "test.warning",
                Severity::Warning,
            ))
            .expect("publish");

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
    fn new_subscription_prunes_dropped_registry_history() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        for _ in 0..128 {
            drop(
                runtime
                    .subscribe(SubscriptionOptions::default())
                    .expect("temporary subscription"),
            );
        }

        let live = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("live subscription");

        assert_eq!(
            runtime
                .inner
                .subscriptions
                .lock()
                .expect("subscription registry")
                .len(),
            1
        );
        drop(live);
        runtime.close();
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
