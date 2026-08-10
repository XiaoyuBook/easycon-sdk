use std::cell::Cell;
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
use crate::concurrency::{
    TaskLifecycleState, TaskOwnerBinding, catch_isolated, contain_panic, runtime_close_rejected,
    task_join_rejected,
};
use crate::deadline::{DeadlineAdmission, DeadlineRegistration, DeadlineScheduler};
use crate::event::{
    Event, EventDraft, EventKind, EventSubscription, Severity, SubscriptionInner,
    SubscriptionOptions, close_subscriptions_with_final,
};
use crate::operation::{
    CancellationReason, Operation, OperationFallbackOutcome, OperationInner, OperationOwnerSetup,
    OperationSettlementOwner, OwnerCleanup, OwnerCleanupSettled, SettlementOwnerMode,
};

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CURRENT_SUPERVISED_TASK: Cell<Option<TaskOwnerBinding<RuntimeId, TaskId>>> =
        const { Cell::new(None) };
}

/// Root lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    /// Accepting new operations and resources.
    Active,
    /// Rejecting admission while supervised children stop.
    Closing,
    /// All supervised task, resource, and operation registries are empty.
    Closed,
    /// Deterministic close stopped at a saved, diagnosable failure.
    CloseFailed,
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

/// Stable phase identifying where deterministic close stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClosePhase {
    /// Closing notification or root cancellation setup.
    Start,
    /// Runtime-owned resource cleanup.
    ResourceCleanup,
    /// Ordinary supervised task join.
    TaskJoin,
    /// Fallback terminal completion after owners exited.
    OperationFinalization,
    /// Runtime-internal task shutdown and join.
    InternalTaskJoin,
    /// Final registry convergence check.
    RegistryConvergence,
    /// Final event publication and producer close.
    FinalEvent,
}

/// Saved diagnostic for an unrecoverable deterministic-close failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseReport {
    /// Phase that produced the primary failure.
    pub phase: ClosePhase,
    /// Stable diagnostic suitable for logs and binding error translation.
    pub diagnostic: Arc<str>,
    /// Resource associated with the failure, when applicable.
    pub resource_id: Option<ResourceId>,
    /// Task associated with the failure, when applicable.
    pub task_id: Option<TaskId>,
    /// Registry counts captured after failure containment.
    pub counts: RuntimeCounts,
}

/// Saved result returned to every concurrent and later close caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    /// Deterministic close completed and all registries converged.
    Closed,
    /// Deterministic close stopped with one saved diagnostic report.
    Failed(Arc<CloseReport>),
}

/// Saved result of one supervised task body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisedTaskOutcome {
    /// The task body returned normally.
    Completed,
    /// The Runtime boundary caught a panic from the task body.
    Panicked,
}

/// Rejection returned when a supervised task tries to join itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskJoinError {
    /// The calling thread owns the task being joined.
    SelfJoin,
}

/// Caller-local rejection that does not start or change Runtime close state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseRejection {
    /// A supervised task cannot synchronously close the Runtime that owns it.
    SupervisedTask,
    /// The deterministic close owner cannot synchronously re-enter its own Runtime close.
    CloseOwner,
}

/// Observation and explicit join handle for a Runtime-owned task.
#[derive(Clone)]
pub struct SupervisedTask {
    runtime: Weak<RuntimeInner>,
    runtime_id: RuntimeId,
    id: TaskId,
    owner: ThreadId,
    completion: Arc<TaskCompletion>,
}

/// Active resource callback invoked by Runtime close.
pub trait ManagedResource: Send + Sync + 'static {
    /// Cancels, neutralizes where applicable, and joins the resource's work.
    fn close(&self);

    /// Performs deterministic close while retaining a typed resource failure for Runtime's
    /// saved close report. Existing resources may use the compatibility boundary when their
    /// close path cannot report a domain error.
    fn close_checked(&self) -> Result<(), EasyConError> {
        self.close();
        Ok(())
    }
}

/// Cloneable root owner for operations, resources, tasks, events, and cancellation.
/// Call [`Runtime::close`] for deterministic cleanup. Dropping the final owning handle only rejects
/// admission and requests root cancellation.
pub struct Runtime {
    pub(crate) inner: Arc<RuntimeInner>,
    counts_as_owner: bool,
}

#[cfg(test)]
type FinalEventObserver = Arc<dyn Fn(RuntimeCounts) + Send + Sync>;
#[cfg(test)]
type ExternalTaskJoinObserver = Arc<dyn Fn(TaskId) + Send + Sync>;

pub(crate) struct RuntimeInner {
    id: RuntimeId,
    runtime_handles: AtomicUsize,
    state: Mutex<RuntimeState>,
    close_in_progress: AtomicBool,
    close_owner: Mutex<Option<ThreadId>>,
    state_changed: Condvar,
    close_outcome: Mutex<Option<CloseOutcome>>,
    root_cancellation: CancellationToken,
    clock: Arc<dyn Clock>,
    next_id: AtomicU64,
    next_sequence: AtomicU64,
    last_event_timestamp_ns: AtomicU64,
    operations: Mutex<HashMap<OperationId, Arc<OperationInner>>>,
    resources: Mutex<HashMap<ResourceId, Weak<dyn ManagedResource>>>,
    tasks: Mutex<HashMap<TaskId, TaskRecord>>,
    task_failures: Mutex<Vec<TaskId>>,
    #[cfg(test)]
    task_join_after_unlink: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    external_task_join_before_wait: Mutex<Option<ExternalTaskJoinObserver>>,
    #[cfg(test)]
    deadline_worker_panic: AtomicBool,
    #[cfg(test)]
    final_event_failure_at: AtomicUsize,
    #[cfg(test)]
    final_event_before_commit: Mutex<Option<FinalEventObserver>>,
    #[cfg(test)]
    operation_terminal_event_panic: AtomicBool,
    #[cfg(test)]
    operation_registry_unlink_panic: AtomicBool,
    deadline_task_id: TaskId,
    subscriptions: Mutex<Vec<Weak<SubscriptionInner>>>,
    events_closed: AtomicBool,
    deadline_sender: Sender<DeadlineSignal>,
    deadline_scheduler: Arc<DeadlineScheduler>,
    deadline_task: Mutex<Option<SupervisedTask>>,
    clock_hook: Mutex<Option<ClockChangeRegistration>>,
}

enum DeadlineSignal {
    Wake,
    Shutdown,
}

struct CloseFailure {
    phase: ClosePhase,
    diagnostic: Arc<str>,
    resource_id: Option<ResourceId>,
    task_id: Option<TaskId>,
}

struct CloseOwnerGuard<'a> {
    owner: &'a Mutex<Option<ThreadId>>,
}

impl Drop for CloseOwnerGuard<'_> {
    fn drop(&mut self) {
        *self
            .owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

impl CloseFailure {
    fn new(phase: ClosePhase, diagnostic: impl Into<Arc<str>>) -> Self {
        Self {
            phase,
            diagnostic: diagnostic.into(),
            resource_id: None,
            task_id: None,
        }
    }

    fn with_resource(mut self, resource_id: ResourceId) -> Self {
        self.resource_id = Some(resource_id);
        self
    }

    fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }
}

struct TaskRecord {
    owner: Option<ThreadId>,
    completion: Arc<TaskCompletion>,
    lifecycle: TaskLifecycleState<SupervisedTaskOutcome>,
}

struct TaskCompletion {
    outcome: Mutex<Option<SupervisedTaskOutcome>>,
    changed: Condvar,
    join_gate: Mutex<()>,
    handle: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    detached_join_before_thread_join: Mutex<Option<Sender<()>>>,
}

impl TaskCompletion {
    fn new() -> Self {
        Self {
            outcome: Mutex::new(None),
            changed: Condvar::new(),
            join_gate: Mutex::new(()),
            handle: Mutex::new(None),
            #[cfg(test)]
            detached_join_before_thread_join: Mutex::new(None),
        }
    }

    fn install_handle(&self, handle: JoinHandle<()>) {
        let mut slot = self
            .handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(slot.is_none(), "supervised task installs one join handle");
        *slot = Some(handle);
    }

    fn take_handle(&self) -> Option<JoinHandle<()>> {
        self.handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn finish(&self, outcome: SupervisedTaskOutcome) {
        *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
        self.changed.notify_all();
    }

    fn wait(&self) -> SupervisedTaskOutcome {
        let mut outcome = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(outcome) = *outcome {
                return outcome;
            }
            outcome = self
                .changed
                .wait(outcome)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn join_without_registry(
        &self,
        current: ThreadId,
    ) -> Result<SupervisedTaskOutcome, TaskJoinError> {
        let _join = self
            .join_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        {
            let handle = self
                .handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if handle
                .as_ref()
                .is_some_and(|handle| handle.thread().id() == current)
            {
                return Err(TaskJoinError::SelfJoin);
            }
        }
        let outcome = self.wait();
        if let Some(handle) = self.take_handle() {
            #[cfg(test)]
            if let Some(observer) = self
                .detached_join_before_thread_join
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = observer.send(());
            }
            let joined = contain_panic(handle.join());
            debug_assert!(
                joined.is_ok(),
                "supervised wrapper catches task body panics"
            );
        }
        Ok(outcome)
    }
}

impl SupervisedTask {
    /// Returns the Runtime-local task identifier.
    #[must_use]
    pub const fn id(&self) -> TaskId {
        self.id
    }

    /// Waits for task completion, joins its Runtime-owned thread, and unregisters it.
    ///
    /// # Errors
    ///
    /// Returns [`TaskJoinError::SelfJoin`] when called by the supervised task itself.
    pub fn join(&self) -> Result<SupervisedTaskOutcome, TaskJoinError> {
        if task_join_rejected(current_supervised_task(), self.runtime_id, self.id) {
            debug_assert_eq!(self.owner, std::thread::current().id());
            return Err(TaskJoinError::SelfJoin);
        }
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.join_supervised_task(self.id, &self.completion)
        } else {
            self.completion
                .join_without_registry(std::thread::current().id())
        }
    }
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
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.id);
        }
    }
}

impl Drop for ResourceRegistration {
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
        let scheduler_sender = deadline_sender.clone();
        let deadline_scheduler = DeadlineScheduler::new(Arc::new(move || {
            let _ = scheduler_sender.send(DeadlineSignal::Wake);
        }));
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
            close_in_progress: AtomicBool::new(false),
            close_owner: Mutex::new(None),
            state_changed: Condvar::new(),
            close_outcome: Mutex::new(None),
            root_cancellation: CancellationToken::root_for_runtime(runtime_id),
            clock,
            next_id,
            next_sequence: AtomicU64::new(1),
            last_event_timestamp_ns: AtomicU64::new(0),
            operations: Mutex::new(HashMap::new()),
            resources: Mutex::new(HashMap::new()),
            tasks: Mutex::new(HashMap::new()),
            task_failures: Mutex::new(Vec::new()),
            #[cfg(test)]
            task_join_after_unlink: Mutex::new(None),
            #[cfg(test)]
            external_task_join_before_wait: Mutex::new(None),
            #[cfg(test)]
            deadline_worker_panic: AtomicBool::new(false),
            #[cfg(test)]
            final_event_failure_at: AtomicUsize::new(usize::MAX),
            #[cfg(test)]
            final_event_before_commit: Mutex::new(None),
            #[cfg(test)]
            operation_terminal_event_panic: AtomicBool::new(false),
            #[cfg(test)]
            operation_registry_unlink_panic: AtomicBool::new(false),
            deadline_task_id: task_id,
            subscriptions: Mutex::new(Vec::new()),
            events_closed: AtomicBool::new(false),
            deadline_sender: deadline_sender.clone(),
            deadline_scheduler,
            deadline_task: Mutex::new(None),
            clock_hook: Mutex::new(None),
        });

        let clock_sender = deadline_sender;
        let clock_hook = inner.clock.on_change(Arc::new(move || {
            let _ = clock_sender.send(DeadlineSignal::Wake);
        }));
        *inner
            .clock_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(clock_hook);
        let runtime = Arc::downgrade(&inner);
        let task = spawn_supervised_inner(
            &inner,
            task_id,
            format!("easycon-deadline-{runtime_number}"),
            move || deadline_worker(runtime, deadline_receiver),
        )
        .expect("failed to start Runtime deadline worker");
        *inner
            .deadline_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);

        Self {
            inner,
            counts_as_owner: true,
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            counts_as_owner: false,
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
        if let Some(deadline) = deadline_ns {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ensure_active(*state)?;
            if parent.owner() != Some(self.inner.id) {
                return Err(EasyConError::new(
                    ErrorDomain::Validation,
                    ErrorCode::InvalidArgument,
                    "operation cancellation parent belongs to a different Runtime",
                ));
            }
            drop(state);
            self.inner.clock.register_deadline(deadline);
        }
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure_active(*state)?;
        if parent.owner() != Some(self.inner.id) {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "operation cancellation parent belongs to a different Runtime",
            ));
        }
        let Some(cancellation) = parent.try_child() else {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "operation cancellation parent is no longer active",
            ));
        };
        let id = OperationId::new(self.inner.allocate_id());
        let operation = Operation::new(id, Arc::downgrade(&self.inner), cancellation, deadline_ns);
        self.inner
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

    /// Registers a one-shot absolute deadline in this Runtime's clock epoch.
    pub fn register_deadline(&self, target_ns: u64) -> Result<DeadlineRegistration, EasyConError> {
        let admission = self.inner.reserve_deadline_admission(target_ns)?;
        let trace_id = self.inner.clock.register_deadline(target_ns);
        let now_ns = self.inner.clock.now_ns();
        Ok(admission.commit(trace_id, now_ns, &*self.inner.clock))
    }

    /// Creates an operation with one explicit settlement owner and pre-registered cleanup record.
    pub fn create_operation_with_settlement_owner<C, S>(
        &self,
        deadline_ns: Option<u64>,
        mode: SettlementOwnerMode,
        cleanup: C,
        on_cleanup_settled: S,
    ) -> Result<(Operation, OperationSettlementOwner), EasyConError>
    where
        C: FnOnce() -> Result<(), EasyConError> + Send + 'static,
        S: FnOnce(Result<(), EasyConError>) + Send + 'static,
    {
        if let Some(deadline) = deadline_ns {
            let state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ensure_active(*state)?;
            drop(state);
            self.inner.clock.register_deadline(deadline);
        }
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure_active(*state)?;
        let Some(cancellation) = self.inner.root_cancellation.try_child() else {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "operation cancellation parent is no longer active",
            ));
        };
        let id = OperationId::new(self.inner.allocate_id());
        let owner_id = self.inner.allocate_id();
        let transfer_owner_id = match mode {
            SettlementOwnerMode::Exclusive => None,
            SettlementOwnerMode::Transferable => Some(self.inner.allocate_id()),
        };
        let setup = OperationOwnerSetup::new(
            owner_id,
            transfer_owner_id,
            Box::new(cleanup) as OwnerCleanup,
            Box::new(on_cleanup_settled) as OwnerCleanupSettled,
        );
        let (operation, owner) = Operation::new_owned(
            id,
            Arc::downgrade(&self.inner),
            cancellation,
            deadline_ns,
            setup,
        );
        self.inner
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, Arc::clone(&operation.inner));
        drop(state);
        if deadline_ns.is_some() {
            let _ = self.inner.deadline_sender.send(DeadlineSignal::Wake);
        }
        Ok((operation, owner))
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
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure_active(*state)?;
        let subscription = EventSubscription::new(options);
        let mut subscriptions = self
            .inner
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure_active(*state)?;
        let id = ResourceId::new(self.inner.allocate_id());
        self.inner
            .resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, Arc::downgrade(&resource));
        drop(state);
        Ok(ResourceRegistration {
            runtime: Arc::downgrade(&self.inner),
            id,
            released: AtomicBool::new(false),
        })
    }

    /// Spawns one Runtime-owned task after atomically admitting and registering it.
    ///
    /// The Runtime stores the thread owner, completion state, and `JoinHandle` before the task body
    /// can run. Panics from the body are caught and reported through [`SupervisedTaskOutcome`].
    pub fn spawn_supervised<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> Result<SupervisedTask, EasyConError>
    where
        F: FnOnce() + Send + 'static,
    {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure_active(*state)?;
        let id = TaskId::new(self.inner.allocate_id());
        let supervised = spawn_supervised_inner(&self.inner, id, name.into(), task)?;
        drop(state);
        Ok(supervised)
    }

    /// Spawns work after binding an explicit operation settlement owner to the supervised task.
    pub fn spawn_operation_owner<F>(
        &self,
        name: impl Into<String>,
        owner: OperationSettlementOwner,
        task: F,
    ) -> Result<SupervisedTask, EasyConError>
    where
        F: FnOnce(OperationSettlementOwner) + Send + 'static,
    {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure_active(*state)?;
        if !owner.belongs_to(&self.inner) {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "operation settlement owner belongs to a different Runtime",
            ));
        }
        let id = TaskId::new(self.inner.allocate_id());
        if !owner.bind_task(id) {
            return Err(EasyConError::new(
                ErrorDomain::Runtime,
                ErrorCode::InvalidArgument,
                "operation settlement owner is already bound",
            ));
        }
        let owner_inner = Arc::clone(&owner.inner);
        let supervised = match spawn_supervised_inner(&self.inner, id, name.into(), move || {
            task(owner);
        }) {
            Ok(supervised) => supervised,
            Err(error) => {
                owner_inner.mark_owner_task_joined(id);
                return Err(error);
            }
        };
        drop(state);
        Ok(supervised)
    }

    /// Binds an operation settlement owner to an already registered supervised task.
    ///
    /// This is for a resource with one long-lived lane that creates short-lived operations after
    /// the lane itself has been admitted. The binding is checked against this Runtime and may be
    /// performed once, before the owner claims terminal evidence.
    pub fn bind_operation_settlement_owner_to_task(
        &self,
        owner: &OperationSettlementOwner,
        task: &SupervisedTask,
    ) -> Result<(), EasyConError> {
        if !owner.belongs_to(&self.inner)
            || task.runtime_id != self.inner.id
            || !task.runtime.ptr_eq(&Arc::downgrade(&self.inner))
        {
            return Err(EasyConError::new(
                ErrorDomain::Validation,
                ErrorCode::InvalidArgument,
                "operation settlement owner and supervised task must belong to this Runtime",
            ));
        }
        let tasks = self
            .inner
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !tasks.contains_key(&task.id) {
            return Err(EasyConError::new(
                ErrorDomain::Runtime,
                ErrorCode::InvalidArgument,
                "operation settlement owner task is no longer supervised",
            ));
        }
        if !owner.bind_task(task.id) {
            return Err(EasyConError::new(
                ErrorDomain::Runtime,
                ErrorCode::InvalidArgument,
                "operation settlement owner is already bound",
            ));
        }
        drop(tasks);
        Ok(())
    }

    /// Returns current supervised counts without changing lifecycle.
    #[must_use]
    pub fn counts(&self) -> RuntimeCounts {
        self.inner.counts_recover()
    }

    /// Idempotently runs deterministic close and returns its saved terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns [`CloseRejection::SupervisedTask`] without changing Runtime state when called from
    /// one of this Runtime's supervised tasks.
    ///
    pub fn close(&self) -> Result<CloseOutcome, CloseRejection> {
        if runtime_close_rejected(current_supervised_task(), self.inner.id) {
            return Err(CloseRejection::SupervisedTask);
        }
        if self.inner.current_thread_owns_close() {
            return Err(CloseRejection::CloseOwner);
        }
        Ok(self.close_impl())
    }

    fn close_impl(&self) -> CloseOutcome {
        let owner = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                match *state {
                    RuntimeState::Active => {
                        self.inner.deadline_scheduler.seal_admission();
                        *state = RuntimeState::Closing;
                        let acquired = self.inner.close_in_progress.compare_exchange(
                            false,
                            true,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                        debug_assert!(acquired.is_ok(), "Active Runtime has no close owner");
                        break true;
                    }
                    RuntimeState::Closing => {
                        if self
                            .inner
                            .close_in_progress
                            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                        {
                            break true;
                        }
                        state = self
                            .inner
                            .state_changed
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    RuntimeState::Closed | RuntimeState::CloseFailed => break false,
                }
            }
        };
        if !owner {
            return self.inner.saved_close_outcome();
        }

        let _close_owner = self.inner.bind_close_owner();
        let outcome = self.finish_close();
        self.inner.complete_close(outcome.clone());
        outcome
    }

    fn finish_close(&self) -> CloseOutcome {
        let phase = Cell::new(ClosePhase::Start);
        let result = catch_isolated(|| self.finish_close_success_path(&phase));
        match result {
            Ok(Ok(())) => CloseOutcome::Closed,
            Ok(Err(failure)) => self.inner.failed_close_outcome(failure),
            Err(_) => self.inner.failed_close_outcome(CloseFailure::new(
                phase.get(),
                "Runtime close phase panicked",
            )),
        }
    }

    fn finish_close_success_path(&self, phase: &Cell<ClosePhase>) -> Result<(), CloseFailure> {
        let mut first_failure = if matches!(
            catch_isolated(|| {
                self.inner.try_publish_event(EventDraft::critical(
                    EventKind::State,
                    "runtime.closing",
                    Severity::Info,
                ))
            }),
            Ok(Ok(_))
        ) {
            None
        } else {
            Some(CloseFailure::new(
                ClosePhase::Start,
                "Runtime closing event could not be published",
            ))
        };
        self.inner.root_cancellation.cancel();

        phase.set(ClosePhase::OperationFinalization);
        let mut operations: Vec<_> = self
            .inner
            .operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

        phase.set(ClosePhase::ResourceCleanup);
        let mut resources = {
            let mut registry = self
                .inner
                .resources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let mut resource_failure = None;
        for (id, resource) in &resources {
            match catch_isolated(|| resource.close_checked()) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if resource_failure.is_none() {
                        resource_failure = Some(
                            CloseFailure::new(ClosePhase::ResourceCleanup, error.to_string())
                                .with_resource(*id),
                        );
                    }
                    let _ = catch_isolated(|| {
                        let _ = self.inner.try_publish_event(
                            EventDraft::critical(
                                EventKind::Warning,
                                "runtime.resource.close_failed",
                                Severity::Error,
                            )
                            .with_resource(*id)
                            .with_detail(error.to_string()),
                        );
                    });
                }
                Err(()) => {
                    if resource_failure.is_none() {
                        resource_failure = Some(
                            CloseFailure::new(
                                ClosePhase::ResourceCleanup,
                                "ManagedResource::close panicked",
                            )
                            .with_resource(*id),
                        );
                    }
                    let _ = catch_isolated(|| {
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
                    });
                }
            }
        }
        drop(resources);
        if first_failure.is_none() {
            first_failure = resource_failure;
        }

        phase.set(ClosePhase::TaskJoin);
        if let Err(failure) = self.inner.join_external_tasks()
            && first_failure.is_none()
        {
            first_failure = Some(failure);
        }

        phase.set(ClosePhase::OperationFinalization);
        let mut operation_failure = None;
        for operation in &operations {
            if !operation.snapshot().state.is_terminal() {
                match operation.inner.settle_after_owner_join() {
                    OperationFallbackOutcome::Legacy => {
                        let _ = operation.finish_cancelled();
                    }
                    OperationFallbackOutcome::AlreadyTerminal
                    | OperationFallbackOutcome::Settled => {}
                    OperationFallbackOutcome::OwnershipLost => {
                        if operation_failure.is_none() && first_failure.is_none() {
                            operation_failure = Some(CloseFailure::new(
                                ClosePhase::OperationFinalization,
                                "operation settlement ownership and evidence are unavailable",
                            ));
                        }
                    }
                }
            }
        }
        drop(operations);

        phase.set(ClosePhase::InternalTaskJoin);
        if let Err(failure) = self.inner.stop_deadline_worker()
            && first_failure.is_none()
            && operation_failure.is_none()
        {
            first_failure = Some(failure);
        }
        phase.set(ClosePhase::RegistryConvergence);
        if self.inner.counts_recover()
            != (RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            })
            && first_failure.is_none()
            && operation_failure.is_none()
        {
            first_failure = Some(CloseFailure::new(
                ClosePhase::RegistryConvergence,
                "Runtime registries did not converge during close",
            ));
        }
        if let Some(failure) = first_failure.or(operation_failure) {
            return Err(failure);
        }

        phase.set(ClosePhase::FinalEvent);
        self.inner
            .close_events(EventDraft::critical(
                EventKind::State,
                "runtime.closed",
                Severity::Info,
            ))
            .map_err(|()| {
                CloseFailure::new(
                    ClosePhase::FinalEvent,
                    "Runtime final event delivery failed",
                )
            })?;
        Ok(())
    }
}

impl Clone for Runtime {
    fn clone(&self) -> Self {
        if self.counts_as_owner {
            self.inner
                .runtime_handles
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |handles| {
                    handles.checked_add(1)
                })
                .expect("Runtime handle count exhausted");
        }
        Self {
            inner: Arc::clone(&self.inner),
            counts_as_owner: self.counts_as_owner,
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if !self.counts_as_owner {
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
            let cancel = {
                let mut state = match self.inner.state.lock() {
                    Ok(state) => state,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if *state == RuntimeState::Active {
                    self.inner.deadline_scheduler.seal_admission();
                    *state = RuntimeState::Closing;
                    true
                } else {
                    false
                }
            };
            if cancel {
                let _ = catch_isolated(|| {
                    self.inner.root_cancellation.cancel();
                });
            }
        }
    }
}

impl RuntimeInner {
    fn reserve_deadline_admission(
        &self,
        target_ns: u64,
    ) -> Result<DeadlineAdmission, EasyConError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ensure_active(*state)?;
        self.deadline_scheduler
            .reserve(target_ns)
            .map_err(|()| runtime_closing_error("Runtime deadline admission is sealed"))
    }

    fn current_thread_owns_close(&self) -> bool {
        *self
            .close_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            == Some(std::thread::current().id())
    }

    fn bind_close_owner(&self) -> CloseOwnerGuard<'_> {
        let mut owner = self
            .close_owner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(owner.is_none(), "deterministic close has one owner thread");
        *owner = Some(std::thread::current().id());
        drop(owner);
        CloseOwnerGuard {
            owner: &self.close_owner,
        }
    }

    fn saved_close_outcome(&self) -> CloseOutcome {
        self.close_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("terminal Runtime state has one saved close outcome")
    }

    fn complete_close(&self, outcome: CloseOutcome) {
        let target = match &outcome {
            CloseOutcome::Closed => RuntimeState::Closed,
            CloseOutcome::Failed(_) => RuntimeState::CloseFailed,
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut saved = self
            .close_outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(saved.is_none(), "close outcome is committed once");
        *saved = Some(outcome);
        *state = target;
        drop(saved);
        drop(state);
        self.state_changed.notify_all();
    }

    fn counts_recover(&self) -> RuntimeCounts {
        RuntimeCounts {
            active_operations: self
                .operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            active_resources: self
                .resources
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            active_tasks: self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
        }
    }

    fn failed_close_outcome(&self, failure: CloseFailure) -> CloseOutcome {
        let _ = catch_isolated(|| self.root_cancellation.cancel());
        let _ = catch_isolated(|| {
            let _ = self.stop_deadline_worker();
        });
        let report = Arc::new(CloseReport {
            phase: failure.phase,
            diagnostic: failure.diagnostic,
            resource_id: failure.resource_id,
            task_id: failure.task_id,
            counts: self.counts_recover(),
        });
        let mut event =
            EventDraft::critical(EventKind::State, "runtime.close_failed", Severity::Error)
                .with_detail(Arc::clone(&report.diagnostic));
        if let Some(resource_id) = report.resource_id {
            event = event.with_resource(resource_id);
        }
        let first_attempt = event.clone();
        if !matches!(
            catch_isolated(|| self.close_events(first_attempt)),
            Ok(Ok(()))
        ) && !matches!(
            catch_isolated(|| self.close_events_recovering_clock(event)),
            Ok(Ok(()))
        ) {
            self.force_close_event_producers();
        }
        CloseOutcome::Failed(report)
    }

    fn force_close_event_producers(&self) {
        self.events_closed.store(true, Ordering::Release);
        let subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for subscription in subscriptions.iter().filter_map(Weak::upgrade) {
            let _ = catch_isolated(|| subscription.close());
        }
    }

    fn join_supervised_task(
        &self,
        id: TaskId,
        completion: &Arc<TaskCompletion>,
    ) -> Result<SupervisedTaskOutcome, TaskJoinError> {
        let current = std::thread::current().id();
        if task_join_rejected(current_supervised_task(), self.id, id) {
            debug_assert!(
                self.tasks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                    .is_some_and(|record| record.owner == Some(current))
            );
            return Err(TaskJoinError::SelfJoin);
        }

        let _join = completion
            .join_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outcome = completion.wait();
        {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(record) = tasks.get_mut(&id) else {
                return Ok(outcome);
            };
            debug_assert_eq!(record.lifecycle.body_outcome(), Some(outcome));
            assert!(
                record.lifecycle.claim_join_handle(),
                "completed supervised task retains one join handle"
            );
        }
        let handle = completion
            .take_handle()
            .expect("supervised lifecycle and handle registry stay aligned");
        debug_assert_ne!(handle.thread().id(), current);
        let joined = contain_panic(handle.join());
        debug_assert!(
            joined.is_ok(),
            "supervised wrapper catches task body panics"
        );
        if outcome == SupervisedTaskOutcome::Panicked {
            let mut failures = self
                .task_failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !failures.contains(&id) {
                failures.push(id);
            }
        }
        {
            let mut tasks = self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let record = tasks
                .get_mut(&id)
                .expect("joined supervised task remains linked until diagnostics are durable");
            assert!(
                record.lifecycle.finish_join(),
                "join completion follows handle claim"
            );
            if outcome == SupervisedTaskOutcome::Panicked {
                assert!(
                    record.lifecycle.persist_panic_diagnostic(),
                    "panicked task records one durable diagnostic"
                );
            }
            assert!(
                record
                    .lifecycle
                    .unlink_registry(outcome == SupervisedTaskOutcome::Panicked),
                "task unlink follows join and durable panic diagnostics"
            );
            tasks.remove(&id);
        }
        self.mark_operation_owner_task_joined(id);
        #[cfg(test)]
        let after_unlink = self
            .task_join_after_unlink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        #[cfg(test)]
        if let Some(hook) = after_unlink {
            hook();
        }
        Ok(outcome)
    }

    fn join_external_tasks(&self) -> Result<(), CloseFailure> {
        let mut tasks: Vec<_> = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(id, _)| **id != self.deadline_task_id)
            .map(|(id, record)| (*id, Arc::clone(&record.completion)))
            .collect();
        tasks.sort_by_key(|(id, _)| *id);
        let mut panicked = Vec::new();
        for (id, completion) in tasks {
            #[cfg(test)]
            let before_wait = self
                .external_task_join_before_wait
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[cfg(test)]
            if let Some(before_wait) = before_wait {
                before_wait(id);
            }
            let outcome = self.join_supervised_task(id, &completion).map_err(|_| {
                CloseFailure::new(
                    ClosePhase::TaskJoin,
                    "Runtime close attempted to join its calling task",
                )
                .with_task(id)
            })?;
            if outcome == SupervisedTaskOutcome::Panicked {
                panicked.push(id);
            }
        }
        let prior_failures = self
            .task_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for id in prior_failures {
            if !panicked.contains(&id) {
                panicked.push(id);
            }
        }
        if let Some(id) = panicked.into_iter().min() {
            return Err(
                CloseFailure::new(ClosePhase::TaskJoin, "supervised task body panicked")
                    .with_task(id),
            );
        }
        Ok(())
    }

    fn allocate_id(&self) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        assert!(id != 0, "Runtime-local ID space exhausted");
        id
    }

    fn mark_operation_owner_task_joined(&self, task_id: TaskId) {
        for operation in self.live_operations() {
            operation.inner.mark_owner_task_joined(task_id);
        }
    }

    pub(crate) fn unregister_operation(&self, id: OperationId) {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
    }

    #[cfg(test)]
    pub(crate) fn take_operation_terminal_event_failpoint(&self) -> bool {
        self.operation_terminal_event_panic
            .swap(false, Ordering::AcqRel)
    }

    #[cfg(not(test))]
    pub(crate) const fn take_operation_terminal_event_failpoint(&self) -> bool {
        false
    }

    #[cfg(test)]
    pub(crate) fn take_operation_registry_unlink_failpoint(&self) -> bool {
        self.operation_registry_unlink_panic
            .swap(false, Ordering::AcqRel)
    }

    #[cfg(not(test))]
    pub(crate) const fn take_operation_registry_unlink_failpoint(&self) -> bool {
        false
    }

    pub(crate) fn try_publish_event(&self, draft: EventDraft) -> Result<Event, EasyConError> {
        if self.events_closed.load(Ordering::Acquire) {
            return Err(events_closed_error());
        }
        let timestamp_ns = self.clock.now_ns();
        let mut subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.events_closed.load(Ordering::Acquire) {
            return Err(events_closed_error());
        }
        let event = self.finish_event(self.allocate_event_sequence(), timestamp_ns, draft);
        enqueue_event(&mut subscriptions, &event);
        Ok(event)
    }

    fn close_events(&self, draft: EventDraft) -> Result<(), ()> {
        self.close_events_with_clock_recovery(draft, false)
    }

    fn close_events_recovering_clock(&self, draft: EventDraft) -> Result<(), ()> {
        self.close_events_with_clock_recovery(draft, true)
    }

    fn close_events_with_clock_recovery(
        &self,
        draft: EventDraft,
        recover_clock_panic: bool,
    ) -> Result<(), ()> {
        if self.events_closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let timestamp_ns = if recover_clock_panic {
            catch_isolated(|| self.clock.now_ns())
                .unwrap_or_else(|_| self.last_event_timestamp_ns.load(Ordering::Acquire))
        } else {
            self.clock.now_ns()
        };
        let mut subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.events_closed.load(Ordering::Acquire) {
            return Ok(());
        }
        #[cfg(test)]
        let fail_at = match self
            .final_event_failure_at
            .swap(usize::MAX, Ordering::AcqRel)
        {
            usize::MAX => None,
            index => Some(index),
        };
        let mut live = Vec::new();
        subscriptions.retain(|subscription| {
            let Some(subscription) = subscription.upgrade() else {
                return false;
            };
            live.push(subscription);
            true
        });
        #[cfg(test)]
        if fail_at.is_some_and(|index| index < live.len()) {
            return Err(());
        }
        #[cfg(test)]
        if let Some(observer) = self
            .final_event_before_commit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            observer(self.counts_recover());
        }
        let event = self.finish_event(self.allocate_event_sequence(), timestamp_ns, draft);
        close_subscriptions_with_final(&live, event);
        self.events_closed.store(true, Ordering::Release);
        Ok(())
    }

    fn allocate_event_sequence(&self) -> u64 {
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        assert!(sequence != 0, "event sequence space exhausted");
        sequence
    }

    fn finish_event(&self, sequence: u64, timestamp_ns: u64, draft: EventDraft) -> Event {
        self.last_event_timestamp_ns
            .store(timestamp_ns, Ordering::Release);
        Event {
            sequence,
            timestamp_ns,
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .map(|inner| Operation { inner })
            .collect();
        operations.sort_by_key(Operation::id);
        operations
    }

    fn stop_deadline_worker(&self) -> Result<(), CloseFailure> {
        self.deadline_scheduler.drain_runtime_closed();
        self.clock_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let _ = self.deadline_sender.send(DeadlineSignal::Shutdown);
        let task = self
            .deadline_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let id = task.id();
            let outcome = task.join().map_err(|_| {
                CloseFailure::new(
                    ClosePhase::InternalTaskJoin,
                    "Runtime deadline task attempted to join itself",
                )
                .with_task(id)
            })?;
            if outcome == SupervisedTaskOutcome::Panicked {
                return Err(CloseFailure::new(
                    ClosePhase::InternalTaskJoin,
                    "Runtime deadline task panicked",
                )
                .with_task(id));
            }
        }
        Ok(())
    }
}

fn spawn_supervised_inner<F>(
    runtime: &Arc<RuntimeInner>,
    id: TaskId,
    name: String,
    task: F,
) -> Result<SupervisedTask, EasyConError>
where
    F: FnOnce() + Send + 'static,
{
    let completion = Arc::new(TaskCompletion::new());
    runtime
        .tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            id,
            TaskRecord {
                owner: None,
                completion: Arc::clone(&completion),
                lifecycle: TaskLifecycleState::registered(),
            },
        );

    let (start, started) = mpsc::channel();
    let task_completion = Arc::clone(&completion);
    let runtime_id = runtime.id;
    let task_runtime = Arc::downgrade(runtime);
    let owner_binding = TaskOwnerBinding::new(runtime_id, id);
    let worker = match std::thread::Builder::new().name(name).spawn(move || {
        CURRENT_SUPERVISED_TASK.set(Some(owner_binding));
        if started.recv().is_err() {
            task_completion.finish(SupervisedTaskOutcome::Panicked);
            return;
        }
        let outcome = if catch_isolated(task).is_ok() {
            SupervisedTaskOutcome::Completed
        } else {
            SupervisedTaskOutcome::Panicked
        };
        if let Some(runtime) = task_runtime.upgrade() {
            let mut tasks = runtime
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let record = tasks
                .get_mut(&id)
                .expect("running supervised task remains registered until join");
            assert!(
                record.lifecycle.complete_body(outcome),
                "supervised body completes once after owner and handle installation"
            );
        }
        task_completion.finish(outcome);
    }) {
        Ok(worker) => worker,
        Err(error) => {
            let mut tasks = runtime
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let record = tasks
                .get_mut(&id)
                .expect("failed spawn leaves its provisional task registration");
            assert!(
                record.lifecycle.abort_spawn(),
                "spawn failure unlinks only an unbound task"
            );
            tasks.remove(&id);
            return Err(EasyConError::new(
                ErrorDomain::Runtime,
                ErrorCode::Internal,
                format!("failed to spawn supervised task: {error}"),
            ));
        }
    };
    let owner = worker.thread().id();
    completion.install_handle(worker);
    {
        let mut tasks = runtime
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let record = tasks
            .get_mut(&id)
            .expect("supervised task remains registered before its start gate opens");
        record.owner = Some(owner);
        assert!(
            record.lifecycle.bind_owner_and_retain_handle(),
            "supervised task installs owner and handle exactly once"
        );
    }
    start
        .send(())
        .expect("supervised task waits for its Runtime start gate");

    Ok(SupervisedTask {
        runtime: Arc::downgrade(runtime),
        runtime_id,
        id,
        owner,
        completion,
    })
}

fn current_supervised_task() -> Option<TaskOwnerBinding<RuntimeId, TaskId>> {
    CURRENT_SUPERVISED_TASK.try_with(Cell::get).ok().flatten()
}

fn deadline_worker(runtime: Weak<RuntimeInner>, receiver: Receiver<DeadlineSignal>) {
    loop {
        let Some(runtime) = runtime.upgrade() else {
            break;
        };
        #[cfg(test)]
        if runtime.deadline_worker_panic.swap(false, Ordering::AcqRel) {
            panic!("failpoint:runtime.deadline_worker.panic");
        }
        runtime.deadline_scheduler.fire_due(&*runtime.clock);
        runtime.poll_deadlines();
        let next_operation = runtime.next_deadline_ns();
        let next_registration = runtime.deadline_scheduler.next_target_ns();
        let next_deadline = match (next_operation, next_registration) {
            (Some(operation), Some(registration)) => Some(operation.min(registration)),
            (Some(operation), None) => Some(operation),
            (None, Some(registration)) => Some(registration),
            (None, None) => None,
        };
        let wait = next_deadline.and_then(|deadline| runtime.clock.real_wait_duration(deadline));
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

fn runtime_closing_error(message: &'static str) -> EasyConError {
    EasyConError::new(ErrorDomain::Runtime, ErrorCode::RuntimeClosing, message)
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
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, Instant};

    use crate::{
        DeadlineResolution, DeadlineWaitResult, EventClass, EventDraft, EventGap, EventKind,
        OperationSnapshot, OperationState, OperationValue, SettlementEvidence, Severity,
        SubscriptionRead, SystemClock, TerminalCandidate, TransitionOutcome, VirtualClock,
        WaitResult, WaitTimeout,
    };

    use super::*;

    struct DropPanickingPayload {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropPanickingPayload {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::AcqRel);
            panic!("scripted panic payload drop");
        }
    }

    struct SnapshotOperationOnDrop {
        operation: Operation,
        entered: mpsc::Sender<()>,
        completed: mpsc::Sender<OperationState>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for SnapshotOperationOnDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::AcqRel);
            let _ = self.entered.send(());
            let state = self.operation.snapshot().state;
            let _ = self.completed.send(state);
        }
    }

    fn panic_with_drop_panicking_payload(drops: Arc<AtomicUsize>) -> ! {
        std::panic::panic_any(DropPanickingPayload { drops });
    }

    struct PanickingNowClock {
        inner: VirtualClock,
        panic_thread: Mutex<Option<ThreadId>>,
        payload_drops: Arc<AtomicUsize>,
    }

    impl PanickingNowClock {
        fn new(now_ns: u64) -> Self {
            Self {
                inner: VirtualClock::new(now_ns),
                panic_thread: Mutex::new(None),
                payload_drops: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn panic_on_current_thread(&self) {
            *self.panic_thread.lock().expect("panic thread lock") =
                Some(std::thread::current().id());
        }

        fn payload_drop_count(&self) -> usize {
            self.payload_drops.load(Ordering::Acquire)
        }
    }

    impl Clock for PanickingNowClock {
        fn now_ns(&self) -> u64 {
            let should_panic = *self
                .panic_thread
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                == Some(std::thread::current().id());
            if should_panic {
                panic_with_drop_panicking_payload(Arc::clone(&self.payload_drops));
            }
            self.inner.now_ns()
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.inner.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> crate::DeadlineId {
            self.inner.register_deadline(target_ns)
        }

        fn record_dispatch(&self, id: crate::DeadlineId, actual_ns: u64) {
            self.inner.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.inner.real_wait_duration(target_ns)
        }
    }

    #[derive(Default)]
    struct OneShotThreadClock {
        inner: VirtualClock,
        panic_thread: Mutex<Option<ThreadId>>,
    }

    impl OneShotThreadClock {
        fn panic_next_on_current_thread(&self) {
            *self.panic_thread.lock().expect("panic thread lock") =
                Some(std::thread::current().id());
        }
    }

    impl Clock for OneShotThreadClock {
        fn now_ns(&self) -> u64 {
            let current = std::thread::current().id();
            let should_panic = {
                let mut panic_thread = self
                    .panic_thread
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *panic_thread == Some(current) {
                    *panic_thread = None;
                    true
                } else {
                    false
                }
            };
            assert!(!should_panic, "scripted warning event clock panic");
            self.inner.now_ns()
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.inner.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> crate::DeadlineId {
            self.inner.register_deadline(target_ns)
        }

        fn record_dispatch(&self, id: crate::DeadlineId, actual_ns: u64) {
            self.inner.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.inner.real_wait_duration(target_ns)
        }
    }

    struct CloseRuntimeOnThreadExit {
        runtime: Runtime,
        result: mpsc::SyncSender<Result<CloseOutcome, CloseRejection>>,
    }

    impl Drop for CloseRuntimeOnThreadExit {
        fn drop(&mut self) {
            let _ = self.result.send(self.runtime.close());
        }
    }

    struct JoinTaskOnThreadExit {
        task: SupervisedTask,
        result: mpsc::SyncSender<Result<SupervisedTaskOutcome, TaskJoinError>>,
    }

    impl Drop for JoinTaskOnThreadExit {
        fn drop(&mut self) {
            let _ = self.result.send(self.task.join());
        }
    }

    struct BlockThreadExit {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        order: Arc<AtomicUsize>,
        exit_order: Arc<AtomicUsize>,
    }

    impl Drop for BlockThreadExit {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.release.recv();
            self.exit_order.store(
                self.order.fetch_add(1, Ordering::AcqRel) + 1,
                Ordering::Release,
            );
        }
    }

    thread_local! {
        static CLOSE_RUNTIME_ON_THREAD_EXIT: RefCell<Option<CloseRuntimeOnThreadExit>> =
            const { RefCell::new(None) };
        static JOIN_TASK_ON_THREAD_EXIT: RefCell<Option<JoinTaskOnThreadExit>> =
            const { RefCell::new(None) };
        static BLOCK_THREAD_EXIT: RefCell<Option<BlockThreadExit>> = const { RefCell::new(None) };
    }

    fn spawn_blocked_operation_waiter(
        operation: &Operation,
    ) -> (mpsc::Receiver<()>, std::thread::JoinHandle<WaitResult>) {
        let (blocked, observed_block) = mpsc::channel();
        operation.observe_next_wait_blocked(blocked);
        let waiting_operation = operation.clone();
        let waiter = std::thread::spawn(move || {
            waiting_operation.wait(WaitTimeout::For(Duration::from_secs(2)))
        });
        (observed_block, waiter)
    }

    // conformance: operation.immutable-terminal
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

        let codes: Vec<_> =
            std::iter::from_fn(
                || match events.read(WaitTimeout::Poll).expect("event read") {
                    SubscriptionRead::Event(event) => Some(event.code),
                    SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
                },
            )
            .collect();
        assert_eq!(
            codes,
            ["runtime.operation.running", "runtime.operation.succeeded"]
        );
    }

    // conformance: operation.hook-capture-drop-reentrant
    #[test]
    fn terminal_discards_hook_captures_outside_state_lock_for_success_and_failure() {
        for terminal in [OperationState::Succeeded, OperationState::Failed] {
            let runtime = Runtime::new(Arc::new(VirtualClock::default()));
            let operation = runtime.create_operation(None).expect("operation");
            assert_eq!(operation.start(), TransitionOutcome::Applied);

            let hook_calls = Arc::new(AtomicUsize::new(0));
            let drops = Arc::new(AtomicUsize::new(0));
            let (drop_entered, observed_drop_entered) = mpsc::channel();
            let (drop_completed, observed_drop_completed) = mpsc::channel();
            let capture = SnapshotOperationOnDrop {
                operation: operation.clone(),
                entered: drop_entered,
                completed: drop_completed,
                drops: Arc::clone(&drops),
            };
            let observed_hook_calls = Arc::clone(&hook_calls);
            operation.on_cancel(move || {
                let _ = &capture;
                observed_hook_calls.fetch_add(1, Ordering::AcqRel);
            });

            let (wait_blocked, observed_wait_blocked) = mpsc::channel();
            operation.observe_next_wait_blocked(wait_blocked);
            let waiting_operation = operation.clone();
            let (wait_finished, observed_wait_finished) = mpsc::sync_channel(1);
            let waiter = std::thread::spawn(move || {
                let result = waiting_operation.wait(WaitTimeout::Infinite);
                wait_finished.send(result).expect("wait observer");
            });
            observed_wait_blocked
                .recv_timeout(Duration::from_secs(2))
                .expect("operation waiter blocked before terminal transaction");

            let finishing_operation = operation.clone();
            let (finish_completed, observed_finish_completed) = mpsc::sync_channel(1);
            let finisher = std::thread::spawn(move || {
                let outcome = match terminal {
                    OperationState::Succeeded => finishing_operation.succeed(OperationValue::Unit),
                    OperationState::Failed => finishing_operation.fail(EasyConError::new(
                        ErrorDomain::Internal,
                        ErrorCode::Internal,
                        "scripted failure",
                    )),
                    _ => unreachable!("test covers success and failure terminal paths"),
                };
                finish_completed.send(outcome).expect("finish observer");
            });

            observed_drop_entered
                .recv_timeout(Duration::from_secs(2))
                .expect("discarded hook capture destructor entered");
            assert_eq!(
                observed_drop_completed
                    .recv_timeout(Duration::from_secs(2))
                    .expect("discarded hook capture destructor re-entered snapshot"),
                OperationState::Running
            );
            assert_eq!(
                observed_finish_completed
                    .recv_timeout(Duration::from_secs(2))
                    .expect("terminal transaction completed"),
                TransitionOutcome::Applied
            );

            let snapshot = operation.snapshot();
            assert_eq!(snapshot.state, terminal);
            match terminal {
                OperationState::Succeeded => {
                    assert_eq!(snapshot.result, Some(OperationValue::Unit));
                    assert!(snapshot.error.is_none());
                }
                OperationState::Failed => {
                    assert!(snapshot.result.is_none());
                    assert_eq!(
                        snapshot.error.as_ref().expect("failure error").code(),
                        ErrorCode::Internal
                    );
                }
                _ => unreachable!("test covers success and failure terminal paths"),
            }
            assert_eq!(hook_calls.load(Ordering::Acquire), 0);
            assert_eq!(drops.load(Ordering::Acquire), 1);
            assert_eq!(runtime.counts().active_operations, 0);
            assert!(matches!(
                observed_wait_finished
                    .recv_timeout(Duration::from_secs(2))
                    .expect("terminal waiter notified"),
                WaitResult::Completed(OperationSnapshot { state, .. }) if state == terminal
            ));

            finisher.join().expect("operation finisher");
            waiter.join().expect("operation waiter");
            runtime.close().expect("Runtime close");
        }
    }

    // conformance: operation.unlink-before-wake
    #[test]
    fn terminal_wait_observes_the_operation_already_unregistered() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let (blocked, observed_block) = mpsc::channel();
        operation.observe_next_wait_blocked(blocked);
        let observed_runtime = runtime.clone();
        let observed_operation = operation.clone();
        let waiter = std::thread::spawn(move || {
            assert!(matches!(
                observed_operation.wait(WaitTimeout::Infinite),
                WaitResult::Completed(_)
            ));
            observed_runtime.counts().active_operations
        });
        observed_block
            .recv_timeout(Duration::from_secs(2))
            .expect("operation waiter blocked");

        operation.succeed(OperationValue::Unit);

        assert_eq!(waiter.join().expect("operation waiter"), 0);
        runtime.close().expect("Runtime close");
    }

    // conformance: timeout.wait-does-not-cancel
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
        runtime.close().expect("Runtime close");
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
        first.close().expect("first Runtime close");
        second.close().expect("second Runtime close");
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
        runtime.close().expect("Runtime close");
    }

    // conformance: operation.child-admission
    #[test]
    fn parent_terminal_seals_child_admission_before_owner_cleanup() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let parent = runtime.create_operation(None).expect("parent operation");
        let parent_token = parent.cancellation_token();
        parent.start();
        let admitted_child = runtime
            .create_operation_with_parent(None, &parent_token)
            .expect("child admitted before parent terminal sealing");
        admitted_child.start();
        let (cleanup_started, observed_cleanup) = mpsc::sync_channel(0);
        let (release_cleanup, released) = mpsc::sync_channel(0);
        let finishing_parent = parent.clone();
        let finisher = std::thread::spawn(move || {
            finishing_parent.succeed_after_cleanup(OperationValue::Unit, || {
                cleanup_started.send(()).expect("cleanup observer");
                released.recv().expect("cleanup release");
            })
        });

        observed_cleanup.recv().expect("owner cleanup started");
        assert_eq!(admitted_child.snapshot().state, OperationState::Cancelling);
        assert_eq!(
            admitted_child.snapshot().cancellation_reason,
            Some(CancellationReason::ParentClose)
        );
        let late_child = runtime.create_operation_with_parent(None, &parent_token);
        release_cleanup.send(()).expect("release owner cleanup");
        assert_eq!(
            finisher.join().expect("parent finisher"),
            TransitionOutcome::Applied
        );

        let error = match late_child {
            Err(error) => error,
            Ok(_) => panic!("parent terminal transaction admitted a new child"),
        };
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        runtime.close().expect("Runtime close");
    }

    // conformance: operation.reentrant-hook
    #[test]
    fn terminal_child_hook_can_observe_parent_without_deadlock() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let child = operation.cancellation_token().child();
        let observed_operation = operation.clone();
        let (hook_finished, hook_observer) = mpsc::sync_channel(0);
        child.on_cancel(move || {
            let snapshot = observed_operation.snapshot();
            hook_finished
                .send(snapshot.state)
                .expect("hook observer remains alive");
        });

        let finishing_operation = operation.clone();
        let (finish_sent, finish_received) = mpsc::sync_channel(0);
        let finisher = std::thread::spawn(move || {
            let outcome = finishing_operation.succeed(OperationValue::Unit);
            finish_sent.send(outcome).expect("finish observer");
        });

        assert_eq!(
            hook_observer
                .recv_timeout(Duration::from_secs(2))
                .expect("child cancellation hook must not deadlock"),
            OperationState::Running
        );
        assert_eq!(
            finish_received
                .recv_timeout(Duration::from_secs(2))
                .expect("terminal transaction must complete"),
            TransitionOutcome::Applied
        );
        finisher.join().expect("operation finisher");
        runtime.close().expect("Runtime close");
    }

    // conformance: operation.failure-notify
    #[test]
    fn terminal_faults_still_unlink_and_wake_waiters() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("events");
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        runtime
            .inner
            .operation_terminal_event_panic
            .store(true, Ordering::Release);
        runtime
            .inner
            .operation_registry_unlink_panic
            .store(true, Ordering::Release);

        let (first_blocked, first_waiter) = spawn_blocked_operation_waiter(&operation);
        let (second_blocked, second_waiter) = spawn_blocked_operation_waiter(&operation);
        first_blocked
            .recv_timeout(Duration::from_secs(2))
            .expect("first operation waiter blocked");
        second_blocked
            .recv_timeout(Duration::from_secs(2))
            .expect("second operation waiter blocked");
        let finish = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation.succeed(OperationValue::Unit)
        }));

        assert_eq!(
            finish.expect("terminal transaction must isolate faults"),
            TransitionOutcome::Applied
        );
        for waiter in [first_waiter, second_waiter] {
            assert!(matches!(
                waiter.join().expect("terminal waiter"),
                WaitResult::Completed(OperationSnapshot {
                    state: OperationState::Succeeded,
                    ..
                })
            ));
        }
        assert!(
            runtime
                .inner
                .operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        assert!(
            !runtime
                .inner
                .operation_terminal_event_panic
                .load(Ordering::Acquire)
        );
        assert!(
            !runtime
                .inner
                .operation_registry_unlink_panic
                .load(Ordering::Acquire)
        );
        let codes: Vec<_> =
            std::iter::from_fn(
                || match events.read(WaitTimeout::Poll).expect("event read") {
                    SubscriptionRead::Event(event) => Some(event.code),
                    SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
                },
            )
            .collect();
        assert_eq!(codes, ["runtime.operation.running"]);

        let operation = runtime.create_operation(None).expect("hook operation");
        operation.start();
        let child = operation.cancellation_token().child();
        child.on_cancel(|| panic!("scripted terminal child hook panic"));
        let (blocked, waiter) = spawn_blocked_operation_waiter(&operation);
        blocked
            .recv_timeout(Duration::from_secs(2))
            .expect("hook-fault waiter blocked");
        let finish = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation.succeed(OperationValue::Unit)
        }));
        assert_eq!(
            finish.expect("hook panic must not escape terminal transaction"),
            TransitionOutcome::Applied
        );
        assert!(child.is_cancelled());
        assert!(matches!(
            waiter.join().expect("hook-fault waiter"),
            WaitResult::Completed(OperationSnapshot {
                state: OperationState::Succeeded,
                ..
            })
        ));

        let operation = runtime.create_operation(None).expect("hook-drop operation");
        operation.start();
        let closure_drops = Arc::new(AtomicUsize::new(0));
        let closure_capture = DropPanickingPayload {
            drops: Arc::clone(&closure_drops),
        };
        operation.on_cancel(move || {
            let _ = &closure_capture;
        });
        let (blocked, waiter) = spawn_blocked_operation_waiter(&operation);
        blocked
            .recv_timeout(Duration::from_secs(2))
            .expect("hook-drop waiter blocked");
        let finish = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation.succeed(OperationValue::Unit)
        }));
        let waiter_result = waiter.join().expect("hook-drop waiter");
        assert_eq!(
            finish.expect("hook drop must not escape terminal transaction"),
            TransitionOutcome::Applied
        );
        assert_eq!(closure_drops.load(Ordering::Acquire), 1);
        assert!(matches!(
            waiter_result,
            WaitResult::Completed(OperationSnapshot {
                state: OperationState::Succeeded,
                ..
            })
        ));

        let operation = runtime.create_operation(None).expect("poison operation");
        operation.start();
        let poisoned_operation = operation.clone();
        assert!(
            std::thread::spawn(move || poisoned_operation.poison_state_for_test())
                .join()
                .is_err()
        );
        let (blocked, waiter) = spawn_blocked_operation_waiter(&operation);
        blocked
            .recv_timeout(Duration::from_secs(2))
            .expect("poison-fault waiter blocked");
        assert_eq!(
            operation.succeed(OperationValue::Unit),
            TransitionOutcome::Applied
        );
        assert!(matches!(
            waiter.join().expect("poison-fault waiter"),
            WaitResult::Completed(OperationSnapshot {
                state: OperationState::Succeeded,
                ..
            })
        ));
        assert!(
            runtime
                .inner
                .operations
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        runtime.close().expect("Runtime close");
    }

    #[test]
    fn owner_cleanup_panic_commits_diagnostic_failure_and_notifies() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let payload_drops = Arc::new(AtomicUsize::new(0));
        let observed_payload_drops = Arc::clone(&payload_drops);

        let finish = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            operation.succeed_after_cleanup(OperationValue::Unit, move || {
                panic_with_drop_panicking_payload(observed_payload_drops);
            })
        }));

        assert!(
            finish.is_ok(),
            "owner cleanup panic escaped terminal transaction"
        );
        let snapshot = operation.snapshot();
        assert_eq!(snapshot.state, OperationState::Failed);
        assert_eq!(
            snapshot.error.expect("cleanup failure").code(),
            ErrorCode::Internal
        );
        assert!(matches!(
            operation.wait(WaitTimeout::Poll),
            WaitResult::Completed(_)
        ));
        assert_eq!(payload_drops.load(Ordering::Acquire), 1);
        assert_eq!(runtime.counts().active_operations, 0);
        runtime.close().expect("Runtime close");
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
        runtime.close().expect("Runtime close");
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

        let observed: Vec<_> =
            std::iter::from_fn(
                || match events.read(WaitTimeout::Poll).expect("event read") {
                    SubscriptionRead::Event(event) => Some(event),
                    SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
                },
            )
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

        runtime.close().expect("Runtime close");

        assert_eq!(observed.try_iter().count(), 0);
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    }

    // conformance: timeout.deadline-cancels
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

    // conformance: event.gap-range
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

        let SubscriptionRead::Event(gap) =
            subscription.read(WaitTimeout::Poll).expect("event read")
        else {
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

    // conformance: event.query-authoritative
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
            subscription.read(WaitTimeout::Poll).expect("event read"),
            SubscriptionRead::Event(Event {
                kind: EventKind::Gap(_),
                ..
            })
        ));
        assert!(matches!(
            subscription.read(WaitTimeout::Poll).expect("event read"),
            SubscriptionRead::Event(Event {
                code: "runtime.operation.succeeded",
                kind: EventKind::Terminal,
                ..
            })
        ));
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    }

    // conformance: event.sequence-order
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

        let events: Vec<_> = std::iter::from_fn(|| {
            match subscription.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => Some(event),
                SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
            }
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

    // conformance: event.producer-nonblocking
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

        let events: Vec<_> = std::iter::from_fn(|| {
            match subscription.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => Some(event),
                SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
            }
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
        task: Mutex<Option<SupervisedTask>>,
        closed: AtomicBool,
    }

    impl ManagedResource for TestResource {
        fn close(&self) {
            self.closed.store(true, Ordering::Release);
            if let Some(task) = self.task.lock().expect("task lock").take() {
                assert_eq!(
                    task.join().expect("test task cannot join itself"),
                    SupervisedTaskOutcome::Completed
                );
            }
            self.registration.lock().expect("registration lock").take();
        }
    }

    #[derive(Default)]
    struct PanickingResource {
        registration: Mutex<Option<ResourceRegistration>>,
    }

    impl ManagedResource for PanickingResource {
        fn close(&self) {
            panic!("scripted resource close panic");
        }
    }

    // conformance: runtime.resource-failure-deadline-service
    #[test]
    fn resource_failure_keeps_deadline_service_until_external_owner_join() {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let registration = runtime.register_deadline(10).expect("deadline");
        let signal = registration.signal();
        let (resolution, observed_resolution) = mpsc::sync_channel(1);
        let task = runtime
            .spawn_supervised("deadline-owner", move || {
                resolution
                    .send(signal.wait(WaitTimeout::Infinite))
                    .expect("deadline result observer");
                drop(registration);
            })
            .expect("deadline owner task");

        let panicking = Arc::new(PanickingResource::default());
        let managed: Arc<dyn ManagedResource> = panicking.clone();
        let resource_registration = runtime.register_resource(managed).expect("resource");
        let resource_id = resource_registration.id();
        *panicking.registration.lock().expect("registration lock") = Some(resource_registration);

        let task_id = task.id();
        let (join_started, observed_join) = mpsc::sync_channel(1);
        *runtime
            .inner
            .external_task_join_before_wait
            .lock()
            .expect("external join observer lock") = Some(Arc::new(move |id| {
            if id == task_id {
                join_started.send(()).expect("external join observer");
            }
        }));
        let closing_runtime = runtime.clone();
        let closer = std::thread::spawn(move || closing_runtime.close());

        observed_join
            .recv_timeout(Duration::from_secs(2))
            .expect("resource failure must proceed to external owner join");
        clock.advance_to(10);
        assert!(matches!(
            observed_resolution
                .recv_timeout(Duration::from_secs(2))
                .expect("deadline resolution"),
            DeadlineWaitResult::Resolved(outcome)
                if outcome.resolution == DeadlineResolution::Fired
        ));

        let CloseOutcome::Failed(report) =
            closer.join().expect("close thread").expect("close outcome")
        else {
            panic!("resource panic cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::ResourceCleanup);
        assert_eq!(report.resource_id, Some(resource_id));
        assert_eq!(task.join(), Ok(SupervisedTaskOutcome::Completed));
        panicking
            .registration
            .lock()
            .expect("registration lock")
            .take();
        assert_eq!(runtime.counts().active_tasks, 0);
    }

    // conformance: operation.claimed-owner-close-wait
    #[test]
    fn close_waits_for_an_already_claimed_unbound_owner() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (cleanup_started, observed_cleanup) = mpsc::sync_channel(0);
        let (release_cleanup, cleanup_released) = mpsc::sync_channel(0);
        let (operation, owner) = runtime
            .create_operation_with_settlement_owner(
                None,
                SettlementOwnerMode::Exclusive,
                move || {
                    cleanup_started.send(()).expect("cleanup start observer");
                    cleanup_released.recv().expect("cleanup release");
                    Ok(())
                },
                |_| {},
            )
            .expect("owned operation");
        assert_eq!(operation.start(), TransitionOutcome::Applied);
        let (settled, observed_settled) = mpsc::sync_channel(1);
        let settling = std::thread::spawn(move || {
            settled
                .send(owner.settle(
                    SettlementEvidence::EffectAccepted,
                    TerminalCandidate::Success(OperationValue::Unit),
                ))
                .expect("settlement observer");
        });
        observed_cleanup
            .recv_timeout(Duration::from_secs(2))
            .expect("cleanup started");

        let (wait_blocked, observed_wait_blocked) = mpsc::channel();
        operation.observe_next_wait_blocked(wait_blocked);
        let closing_runtime = runtime.clone();
        let (closed, observed_close) = mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            closed
                .send(closing_runtime.close())
                .expect("close observer");
        });
        observed_wait_blocked
            .recv_timeout(Duration::from_secs(2))
            .expect("close observes the claimed terminal transaction");
        assert!(matches!(
            observed_close.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        release_cleanup.send(()).expect("release cleanup");
        settling.join().expect("settlement thread");
        assert_eq!(
            observed_settled.recv_timeout(Duration::from_secs(2)),
            Ok(TransitionOutcome::Applied)
        );
        assert_eq!(
            observed_close.recv_timeout(Duration::from_secs(2)),
            Ok(Ok(CloseOutcome::Closed))
        );
        closer.join().expect("close thread");
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    }

    struct WarningEventPanickingResource {
        clock: Arc<OneShotThreadClock>,
        registration: Mutex<Option<ResourceRegistration>>,
    }

    impl ManagedResource for WarningEventPanickingResource {
        fn close(&self) {
            self.clock.panic_next_on_current_thread();
            panic!("scripted resource close panic before warning event fault");
        }
    }

    struct BlockingPanickingResource {
        registration: Mutex<Option<ResourceRegistration>>,
        close_count: AtomicUsize,
        callback_started: mpsc::Sender<()>,
        release_callback: Mutex<mpsc::Receiver<()>>,
    }

    impl ManagedResource for BlockingPanickingResource {
        fn close(&self) {
            self.close_count.fetch_add(1, Ordering::AcqRel);
            self.callback_started
                .send(())
                .expect("resource close observer");
            self.release_callback
                .lock()
                .expect("resource close release lock")
                .recv()
                .expect("resource close release");
            panic!("scripted blocking resource close panic");
        }
    }

    #[test]
    fn resource_close_panic_does_not_unwind_and_still_joins_owner() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let panicking = Arc::new(PanickingResource::default());
        let managed: Arc<dyn ManagedResource> = panicking.clone();
        *panicking.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));
        let (owner_wake, owner_woken) = mpsc::channel();
        operation.on_cancel(move || owner_wake.send(()).expect("owner cancellation wake"));
        let (cleanup_started, observed_cleanup) = mpsc::channel();
        let (release_owner, owner_released) = mpsc::channel();
        let owner_operation = operation.clone();
        let owner = runtime
            .spawn_supervised("operation-owner", move || {
                owner_woken.recv().expect("owner cancellation");
                cleanup_started.send(()).expect("cleanup observer");
                owner_released.recv().expect("owner cleanup release");
                owner_operation.finish_cancelled();
            })
            .expect("owner task");

        let closing_runtime = runtime.clone();
        let closer = std::thread::spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| closing_runtime.close()))
        });
        observed_cleanup
            .recv_timeout(Duration::from_secs(2))
            .expect("owner cleanup started");
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        release_owner.send(()).expect("release owner cleanup");
        let close_result = closer.join().expect("close thread");
        assert_eq!(
            owner.join().expect("owner task join"),
            SupervisedTaskOutcome::Completed
        );
        panicking
            .registration
            .lock()
            .expect("registration lock")
            .take();

        assert!(
            close_result.is_ok(),
            "close failure crossed the API boundary"
        );
        assert!(matches!(close_result, Ok(Ok(CloseOutcome::Failed(_)))));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
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

    struct ReentrantCloseResource {
        runtime: Runtime,
        result: mpsc::SyncSender<Result<CloseOutcome, CloseRejection>>,
        registration: Mutex<Option<ResourceRegistration>>,
    }

    impl ManagedResource for ReentrantCloseResource {
        fn close(&self) {
            let result = self.runtime.close();
            let _ = self.result.send(result);
            self.registration.lock().expect("registration lock").take();
        }
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

        runtime.close().expect("Runtime close");

        assert!(resource.saw_cancelling.load(Ordering::Acquire));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    }

    // conformance: runtime.reentrant-close
    #[test]
    fn resource_callback_reentrant_close_is_rejected_without_self_wait() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (result, observed_result) = mpsc::sync_channel(1);
        let resource = Arc::new(ReentrantCloseResource {
            runtime: runtime.clone(),
            result,
            registration: Mutex::new(None),
        });
        let managed: Arc<dyn ManagedResource> = resource.clone();
        *resource.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));

        let closing_runtime = runtime.clone();
        let closer = std::thread::spawn(move || closing_runtime.close());
        assert_eq!(
            observed_result
                .recv_timeout(Duration::from_secs(2))
                .expect("reentrant close must fail without self-wait"),
            Err(CloseRejection::CloseOwner)
        );
        assert_eq!(
            closer.join().expect("outer close"),
            Ok(CloseOutcome::Closed)
        );
        assert_eq!(runtime.state(), RuntimeState::Closed);
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

        runtime.close().expect("Runtime close");

        assert_eq!(*close_order.lock().expect("close order lock"), [1, 2, 3]);
        let terminal_ids: Vec<_> =
            std::iter::from_fn(
                || match events.read(WaitTimeout::Poll).expect("event read") {
                    SubscriptionRead::Event(event) => Some(event),
                    SubscriptionRead::Closed | SubscriptionRead::Timeout => None,
                },
            )
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

        runtime.close().expect("Runtime close");

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
        *resource.task.lock().expect("task lock") = Some(
            runtime
                .spawn_supervised("test-resource", || {})
                .expect("task"),
        );
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();

        assert_eq!(
            runtime.close().expect("first Runtime close"),
            CloseOutcome::Closed
        );
        assert_eq!(
            runtime.close().expect("repeated Runtime close"),
            CloseOutcome::Closed
        );

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
            match events.read(WaitTimeout::Poll).expect("event read") {
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

    // conformance: runtime.drop-limited
    #[test]
    fn final_owning_drop_only_rejects_admission_and_requests_cancellation() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let supervised = runtime.clone_for_supervision();
        let cancellation = runtime.child_cancellation_token();
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let resource = Arc::new(TestResource::default());
        let managed: Arc<dyn ManagedResource> = resource.clone();
        *resource.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));

        drop(runtime);

        assert_eq!(supervised.state(), RuntimeState::Closing);
        assert!(cancellation.is_cancelled());
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);
        assert!(!resource.closed.load(Ordering::Acquire));
        assert!(!supervised.inner.events_closed.load(Ordering::Acquire));
        let error = match supervised.create_operation(None) {
            Err(error) => error,
            Ok(_) => panic!("Drop must reject new admission synchronously"),
        };
        assert_eq!(error.code(), ErrorCode::RuntimeClosing);
        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Timeout => break,
                SubscriptionRead::Closed => panic!("Drop must not close event production"),
            }
        }
        assert!(!codes.contains(&"runtime.closed"));
        assert!(!codes.contains(&"runtime.close_failed"));

        assert_eq!(
            supervised.close().expect("explicit close takeover"),
            CloseOutcome::Closed
        );
        assert!(resource.closed.load(Ordering::Acquire));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    }

    // conformance: runtime.healthy-cleanup-after-panic
    // conformance: runtime.saved-close-failed
    // conformance: runtime.owner-terminal-order
    // conformance: runtime.close-report
    #[test]
    fn resource_panic_saves_one_report_after_owner_terminal_and_join() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let (owner_wake, owner_woken) = mpsc::channel();
        operation.on_cancel(move || owner_wake.send(()).expect("owner cancellation wake"));
        let (cleanup_started, observed_cleanup) = mpsc::channel();
        let (release_cleanup, cleanup_released) = mpsc::channel();
        let owner_operation = operation.clone();
        let owner = runtime
            .spawn_supervised("failed-resource-owner", move || {
                owner_woken.recv().expect("owner cancellation");
                cleanup_started.send(()).expect("cleanup observer");
                cleanup_released.recv().expect("cleanup release");
                owner_operation.finish_cancelled();
            })
            .expect("owner task");

        let (callback_started, observed_callback) = mpsc::channel();
        let (release_callback, callback_released) = mpsc::channel();
        let panicking = Arc::new(BlockingPanickingResource {
            registration: Mutex::new(None),
            close_count: AtomicUsize::new(0),
            callback_started,
            release_callback: Mutex::new(callback_released),
        });
        let managed: Arc<dyn ManagedResource> = panicking.clone();
        let registration = runtime.register_resource(managed).expect("resource");
        let panicking_id = registration.id();
        *panicking.registration.lock().expect("registration lock") = Some(registration);
        let healthy = Arc::new(TestResource::default());
        let managed: Arc<dyn ManagedResource> = healthy.clone();
        *healthy.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));

        let barrier = Arc::new(Barrier::new(3));
        let callers: Vec<_> = (0..2)
            .map(|_| {
                let closing = runtime.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    closing.close().expect("external close caller")
                })
            })
            .collect();
        barrier.wait();
        observed_callback
            .recv_timeout(Duration::from_secs(2))
            .expect("resource callback started");
        observed_cleanup
            .recv_timeout(Duration::from_secs(2))
            .expect("owner cleanup started");
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        release_callback
            .send(())
            .expect("release resource callback");
        release_cleanup.send(()).expect("release owner cleanup");

        let outcomes: Vec<_> = callers
            .into_iter()
            .map(|caller| caller.join().expect("close caller"))
            .collect();
        let reports: Vec<_> = outcomes
            .iter()
            .map(|outcome| match outcome {
                CloseOutcome::Failed(report) => Arc::clone(report),
                CloseOutcome::Closed => panic!("resource panic cannot report Closed"),
            })
            .collect();
        assert!(Arc::ptr_eq(&reports[0], &reports[1]));
        assert_eq!(reports[0].phase, ClosePhase::ResourceCleanup);
        assert_eq!(reports[0].resource_id, Some(panicking_id));
        assert_eq!(reports[0].task_id, None);
        assert_eq!(
            reports[0].diagnostic.as_ref(),
            "ManagedResource::close panicked"
        );
        assert_eq!(
            reports[0].counts,
            RuntimeCounts {
                active_operations: 0,
                active_resources: 1,
                active_tasks: 0,
            }
        );
        assert_eq!(runtime.state(), RuntimeState::CloseFailed);
        assert_eq!(panicking.close_count.load(Ordering::Acquire), 1);
        assert!(healthy.closed.load(Ordering::Acquire));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert!(matches!(
            operation.wait(WaitTimeout::Poll),
            WaitResult::Completed(_)
        ));
        let later = runtime.close().expect("later close caller");
        let CloseOutcome::Failed(later_report) = later else {
            panic!("saved close failure changed to Closed");
        };
        assert!(Arc::ptr_eq(&reports[0], &later_report));
        assert_eq!(panicking.close_count.load(Ordering::Acquire), 1);

        let mut observed = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
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
            Some("runtime.close_failed")
        );
        assert!(!observed.iter().any(|event| event.code == "runtime.closed"));

        assert_eq!(
            owner.join().expect("owner task join"),
            SupervisedTaskOutcome::Completed
        );
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        panicking
            .registration
            .lock()
            .expect("registration lock")
            .take();
        assert_eq!(
            runtime.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            }
        );

        let clock = Arc::new(OneShotThreadClock::default());
        let runtime = Runtime::new(clock.clone());
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("warning-fault events");
        let panicking = Arc::new(WarningEventPanickingResource {
            clock,
            registration: Mutex::new(None),
        });
        let managed: Arc<dyn ManagedResource> = panicking.clone();
        let registration = runtime.register_resource(managed).expect("resource");
        let panicking_id = registration.id();
        *panicking.registration.lock().expect("registration lock") = Some(registration);
        let healthy = Arc::new(TestResource::default());
        let managed: Arc<dyn ManagedResource> = healthy.clone();
        *healthy.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));

        let CloseOutcome::Failed(report) = runtime.close().expect("Runtime close") else {
            panic!("resource and warning event failure cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::ResourceCleanup);
        assert_eq!(report.resource_id, Some(panicking_id));
        assert_eq!(
            report.diagnostic.as_ref(),
            "ManagedResource::close panicked"
        );
        assert_eq!(
            report.counts,
            RuntimeCounts {
                active_operations: 0,
                active_resources: 1,
                active_tasks: 0,
            }
        );
        assert!(healthy.closed.load(Ordering::Acquire));

        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("failed close queue must be closed"),
            }
        }
        assert_eq!(codes.last(), Some(&"runtime.close_failed"));
        assert!(!codes.contains(&"runtime.resource.close_panicked"));
        assert!(!codes.contains(&"runtime.closed"));

        panicking
            .registration
            .lock()
            .expect("registration lock")
            .take();
        assert_eq!(runtime.counts().active_resources, 0);
    }

    #[test]
    fn deadline_task_panic_is_a_saved_close_failure() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("events");
        runtime
            .inner
            .deadline_worker_panic
            .store(true, Ordering::Release);
        let _ = runtime.inner.deadline_sender.send(DeadlineSignal::Wake);
        let started = Instant::now();
        loop {
            let finished = runtime
                .inner
                .deadline_task
                .lock()
                .expect("deadline task lock")
                .as_ref()
                .is_some_and(|task| {
                    task.completion
                        .outcome
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some()
                });
            if finished {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "deadline worker did not observe the injected panic"
            );
            std::thread::yield_now();
        }

        let deadline_id = runtime.inner.deadline_task_id;
        let outcome = runtime.close().expect("Runtime close caller");
        let CloseOutcome::Failed(report) = outcome else {
            panic!("deadline task panic cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::InternalTaskJoin);
        assert_eq!(report.task_id, Some(deadline_id));
        assert_eq!(report.resource_id, None);
        assert_eq!(runtime.state(), RuntimeState::CloseFailed);
        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("failed close queue must be closed"),
            }
        }
        assert_eq!(codes.last(), Some(&"runtime.close_failed"));
        assert!(!codes.contains(&"runtime.closed"));
    }

    // conformance: operation.poison-recovery
    #[test]
    fn poisoned_operation_registry_does_not_kill_deadline_supervision() {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let operation = runtime.create_operation(Some(10)).expect("operation");
        operation.start();
        let (cancelled, observed_cancel) = mpsc::sync_channel(1);
        operation.on_cancel(move || {
            let _ = cancelled.send(());
        });

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
        clock.advance_to(10);

        observed_cancel
            .recv_timeout(Duration::from_secs(2))
            .expect("deadline worker must recover the operation registry");
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        assert_eq!(operation.finish_cancelled(), TransitionOutcome::Applied);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn poisoned_close_registry_recovers_and_concurrent_callers_observe_closed() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let poisoned_runtime = Arc::clone(&runtime.inner);
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned_runtime
                    .resources
                    .lock()
                    .expect("resource registry lock");
                panic!("scripted resource registry poison");
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
                    closing.close().expect("close caller")
                })
            })
            .collect();

        barrier.wait();
        assert!(
            callers
                .into_iter()
                .all(|caller| caller.join().expect("close caller") == CloseOutcome::Closed)
        );
        assert_eq!(runtime.state(), RuntimeState::Closed);
        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("failed close queue must be closed"),
            }
        }
        assert_eq!(codes.last(), Some(&"runtime.closed"));
    }

    #[test]
    fn explicit_close_can_take_ownership_after_final_owning_drop() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let supervised = runtime.clone_for_supervision();
        let cancellation = runtime.child_cancellation_token();
        let tasks = supervised.inner.tasks.lock().expect("task registry lock");

        drop(runtime);

        assert_eq!(supervised.state(), RuntimeState::Closing);
        assert!(!supervised.inner.close_in_progress.load(Ordering::Acquire));
        assert!(cancellation.is_cancelled());
        let error = match supervised.create_operation(None) {
            Err(error) => error,
            Ok(_) => panic!("final handle drop must reject admission synchronously"),
        };
        assert_eq!(error.code(), ErrorCode::RuntimeClosing);
        drop(tasks);
        assert_eq!(
            supervised.close().expect("explicit close takeover"),
            CloseOutcome::Closed
        );
        assert_eq!(supervised.state(), RuntimeState::Closed);
    }

    #[test]
    fn final_owning_drop_inside_a_supervised_task_is_nonblocking_and_nonfinalizing() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("subscribe");
        let worker_runtime = runtime.clone();
        let (release, released) = mpsc::channel();
        let (finished, observed_finish) = mpsc::channel();
        let task = runtime
            .spawn_supervised("final-owner-drop", move || {
                released.recv().expect("worker release");
                drop(worker_runtime);
                finished.send(()).expect("worker completion");
            })
            .expect("task");

        drop(runtime);
        release.send(()).expect("release worker");

        observed_finish
            .recv_timeout(Duration::from_secs(2))
            .expect("final Runtime drop must not wait for its own task");
        assert_eq!(
            task.join().expect("supervised task cannot join itself"),
            SupervisedTaskOutcome::Completed
        );
        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Timeout => break,
                SubscriptionRead::Closed => panic!("Drop must not finalize event production"),
            }
        }
        assert!(!codes.contains(&"runtime.closed"));
        assert!(!codes.contains(&"runtime.close_failed"));
    }

    // conformance: runtime.detached-task-join
    #[test]
    fn task_join_after_runtime_storage_drop_waits_for_thread_exit() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let runtime_storage = Arc::downgrade(&runtime.inner);
        let (body_started, observed_body_start) = mpsc::channel();
        let (release_body, body_release) = mpsc::channel();
        let (exit_entered, observed_exit_entry) = mpsc::channel();
        let (release_exit, exit_release) = mpsc::channel();
        let order = Arc::new(AtomicUsize::new(0));
        let exit_order = Arc::new(AtomicUsize::new(0));
        let join_order = Arc::new(AtomicUsize::new(0));
        let task_order = Arc::clone(&order);
        let task_exit_order = Arc::clone(&exit_order);
        let task = runtime
            .spawn_supervised("detached-join", move || {
                BLOCK_THREAD_EXIT.with(|slot| {
                    *slot.borrow_mut() = Some(BlockThreadExit {
                        entered: exit_entered,
                        release: exit_release,
                        order: task_order,
                        exit_order: task_exit_order,
                    });
                });
                body_started.send(()).expect("body start observer");
                body_release.recv().expect("body release");
            })
            .expect("task");
        observed_body_start
            .recv_timeout(Duration::from_secs(2))
            .expect("task body started");

        drop(runtime);
        let storage_wait = Instant::now();
        while runtime_storage.upgrade().is_some() {
            assert!(
                storage_wait.elapsed() < Duration::from_secs(2),
                "Runtime storage did not release after final owning Drop"
            );
            std::thread::yield_now();
        }

        let (joiner_ready, observed_joiner_ready) = mpsc::channel();
        let (begin_join, join_release) = mpsc::channel();
        let (join_claimed, observed_join_claim) = mpsc::channel();
        *task
            .completion
            .detached_join_before_thread_join
            .lock()
            .expect("detached join observer") = Some(join_claimed);
        let (joined, observed_join) = mpsc::channel();
        let joining_order = Arc::clone(&order);
        let observed_join_order = Arc::clone(&join_order);
        let joiner = std::thread::spawn(move || {
            joiner_ready.send(()).expect("joiner ready observer");
            join_release.recv().expect("join release");
            let outcome = task.join();
            observed_join_order.store(
                joining_order.fetch_add(1, Ordering::AcqRel) + 1,
                Ordering::Release,
            );
            joined.send(outcome).expect("join result observer");
        });
        observed_joiner_ready
            .recv_timeout(Duration::from_secs(2))
            .expect("task joiner ready");

        release_body.send(()).expect("release task body");
        observed_exit_entry
            .recv_timeout(Duration::from_secs(2))
            .expect("thread-exit destructor entered");

        begin_join.send(()).expect("start task join");
        observed_join_claim
            .recv_timeout(Duration::from_secs(2))
            .expect("detached join claimed OS handle");

        release_exit.send(()).expect("release thread exit");
        let outcome = observed_join
            .recv_timeout(Duration::from_secs(2))
            .expect("join after thread exit");
        joiner.join().expect("task joiner");
        assert_eq!(exit_order.load(Ordering::Acquire), 1);
        assert_eq!(join_order.load(Ordering::Acquire), 2);
        assert_eq!(outcome, Ok(SupervisedTaskOutcome::Completed));
    }

    #[test]
    fn dropping_a_nonfinal_runtime_handle_keeps_the_runtime_active() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let other = runtime.clone();

        drop(other);

        assert_eq!(runtime.state(), RuntimeState::Active);
        runtime.close().expect("Runtime close");
    }

    // conformance: event.final-bypasses-filter
    #[test]
    fn final_runtime_event_bypasses_subscription_severity_filter() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions {
                minimum_severity: Severity::Error,
                ..SubscriptionOptions::default()
            })
            .expect("subscribe");

        runtime.close().expect("Runtime close");

        assert!(matches!(
            events.read(WaitTimeout::Poll).expect("event read"),
            SubscriptionRead::Event(Event {
                code: "runtime.closed",
                ..
            })
        ));
        assert_eq!(
            events.read(WaitTimeout::Poll).expect("event read"),
            SubscriptionRead::Closed
        );
    }

    // conformance: vertical.final-event-order
    #[test]
    fn runtime_closed_follows_task_join_and_registry_convergence() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let events = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("events");
        let task = runtime
            .spawn_supervised("final-event-order", || {})
            .expect("task");
        assert_eq!(task.completion.wait(), SupervisedTaskOutcome::Completed);

        let (unlinked, observed_unlink) = mpsc::channel();
        let (release_unlink, unlink_release) = mpsc::channel();
        let hook_release = Arc::new(Mutex::new(unlink_release));
        *runtime
            .inner
            .task_join_after_unlink
            .lock()
            .expect("task join hook lock") = Some(Arc::new(move || {
            unlinked.send(()).expect("task unlink observer");
            hook_release
                .lock()
                .expect("task unlink release lock")
                .recv()
                .expect("task unlink release");
        }));
        let (final_counts, observed_final_counts) = mpsc::channel();
        *runtime
            .inner
            .final_event_before_commit
            .lock()
            .expect("final event observer lock") = Some(Arc::new(move |counts| {
            final_counts.send(counts).expect("final counts observer");
        }));

        let closing_runtime = runtime.clone();
        let closer = std::thread::spawn(move || closing_runtime.close());
        observed_unlink
            .recv_timeout(Duration::from_secs(2))
            .expect("close joined and unlinked ordinary task");
        runtime
            .inner
            .task_join_after_unlink
            .lock()
            .expect("task join hook lock")
            .take();

        let mut codes = Vec::new();
        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Timeout => break,
                SubscriptionRead::Closed => {
                    panic!("RuntimeClosed was committed before task join returned")
                }
            }
        }
        assert!(!codes.contains(&"runtime.closed"));

        release_unlink.send(()).expect("release task join");
        assert_eq!(
            closer.join().expect("Runtime closer"),
            Ok(CloseOutcome::Closed)
        );
        assert_eq!(
            observed_final_counts
                .recv_timeout(Duration::from_secs(2))
                .expect("RuntimeClosed precommit counts"),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            }
        );
        assert_eq!(task.join(), Ok(SupervisedTaskOutcome::Completed));
        assert_eq!(
            runtime.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            }
        );

        loop {
            match events.read(WaitTimeout::Poll).expect("event read") {
                SubscriptionRead::Event(event) => codes.push(event.code),
                SubscriptionRead::Closed => break,
                SubscriptionRead::Timeout => panic!("closed subscription must finish draining"),
            }
        }
        assert_eq!(codes.last(), Some(&"runtime.closed"));
        let error = runtime
            .publish(EventDraft::ordinary(
                EventKind::Data,
                "test.after_runtime_closed",
                Severity::Info,
            ))
            .expect_err("post-close event production must be rejected");
        assert_eq!(error.code(), ErrorCode::RuntimeClosing);
    }

    // conformance: runtime.start-failure-continues-close
    #[test]
    fn closing_event_failure_still_joins_external_owner_and_settles_operation() {
        let clock = Arc::new(PanickingNowClock::new(41));
        let runtime = Runtime::new(clock.clone());
        let operation = runtime.create_operation(None).expect("operation");
        assert_eq!(operation.start(), TransitionOutcome::Applied);
        let (cancelled, observed_cancel) = mpsc::sync_channel(1);
        operation.on_cancel(move || cancelled.send(()).expect("owner cancellation wake"));
        let (cleanup_started, observed_cleanup) = mpsc::sync_channel(1);
        let (release_cleanup, cleanup_released) = mpsc::sync_channel(0);
        let owner_operation = operation.clone();
        let owner = runtime
            .spawn_supervised("start-failure-owner", move || {
                observed_cancel.recv().expect("owner cancellation");
                cleanup_started.send(()).expect("cleanup start observer");
                cleanup_released.recv().expect("cleanup release");
                assert_eq!(
                    owner_operation.finish_cancelled(),
                    TransitionOutcome::Applied
                );
            })
            .expect("owner task");
        let owner_id = owner.id();
        let (join_started, observed_join) = mpsc::sync_channel(1);
        *runtime
            .inner
            .external_task_join_before_wait
            .lock()
            .expect("external join observer lock") = Some(Arc::new(move |id| {
            if id == owner_id {
                join_started.send(()).expect("external join observer");
            }
        }));
        let resource = Arc::new(TestResource::default());
        let managed: Arc<dyn ManagedResource> = resource.clone();
        *resource.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));

        let closing_runtime = runtime.clone();
        let closing_clock = clock.clone();
        let (closed, observed_close) = mpsc::sync_channel(1);
        let closer = std::thread::spawn(move || {
            closing_clock.panic_on_current_thread();
            closed
                .send(closing_runtime.close())
                .expect("close observer");
        });
        observed_cleanup
            .recv_timeout(Duration::from_secs(2))
            .expect("owner cleanup started");
        observed_join
            .recv_timeout(Duration::from_secs(2))
            .expect("close continued to external owner join");
        assert!(resource.closed.load(Ordering::Acquire));
        assert!(matches!(
            observed_close.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        release_cleanup.send(()).expect("release cleanup");
        let CloseOutcome::Failed(report) = observed_close
            .recv_timeout(Duration::from_secs(2))
            .expect("close outcome")
            .expect("external close caller")
        else {
            panic!("closing event failure cannot report Closed");
        };
        closer.join().expect("close thread");
        assert_eq!(report.phase, ClosePhase::Start);
        assert_eq!(owner.join(), Ok(SupervisedTaskOutcome::Completed));
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert_eq!(
            runtime.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            }
        );
    }

    // conformance: runtime.final-event-failure
    #[test]
    fn final_event_failure_never_exposes_runtime_closed() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let first = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("first subscription");
        let second = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("second subscription");
        runtime
            .inner
            .final_event_failure_at
            .store(1, Ordering::Release);

        let CloseOutcome::Failed(report) = runtime.close().expect("Runtime close") else {
            panic!("injected final event failure cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::FinalEvent);
        assert_eq!(runtime.state(), RuntimeState::CloseFailed);

        for subscription in [&first, &second] {
            let mut codes = Vec::new();
            loop {
                match subscription.read(WaitTimeout::Poll).expect("event read") {
                    SubscriptionRead::Event(event) => codes.push(event.code),
                    SubscriptionRead::Closed => break,
                    SubscriptionRead::Timeout => {
                        panic!("failed-close subscription must be closed")
                    }
                }
            }
            assert_eq!(codes.last(), Some(&"runtime.close_failed"));
            assert!(!codes.contains(&"runtime.closed"));
        }

        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let first = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("first subscription");
        let second = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("second subscription");
        let resource = Arc::new(PanickingResource::default());
        let managed: Arc<dyn ManagedResource> = resource.clone();
        *resource.registration.lock().expect("registration lock") =
            Some(runtime.register_resource(managed).expect("resource"));
        runtime
            .inner
            .final_event_failure_at
            .store(1, Ordering::Release);

        let CloseOutcome::Failed(report) = runtime.close().expect("Runtime close") else {
            panic!("resource and final-event failure cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::ResourceCleanup);
        assert_eq!(runtime.state(), RuntimeState::CloseFailed);

        for subscription in [&first, &second] {
            let mut codes = Vec::new();
            loop {
                match subscription.read(WaitTimeout::Poll).expect("event read") {
                    SubscriptionRead::Event(event) => codes.push(event.code),
                    SubscriptionRead::Closed => break,
                    SubscriptionRead::Timeout => {
                        panic!("failed-close subscription must be closed")
                    }
                }
            }
            assert_eq!(codes.last(), Some(&"runtime.close_failed"));
            assert!(!codes.contains(&"runtime.closed"));
        }

        let clock = Arc::new(PanickingNowClock::new(41));
        let runtime = Runtime::new(clock.clone());
        let first = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("first subscription");
        let second = runtime
            .subscribe(SubscriptionOptions::default())
            .expect("second subscription");
        runtime
            .publish(EventDraft::critical(
                EventKind::State,
                "test.before_clock_failure",
                Severity::Info,
            ))
            .expect("event before clock failure");
        clock.panic_on_current_thread();

        let CloseOutcome::Failed(report) = runtime.close().expect("Runtime close") else {
            panic!("persistent final event failure cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::Start);
        assert_eq!(runtime.state(), RuntimeState::CloseFailed);

        for subscription in [&first, &second] {
            let mut observed = Vec::new();
            loop {
                match subscription.read(WaitTimeout::Poll).expect("event read") {
                    SubscriptionRead::Event(event) => observed.push(event),
                    SubscriptionRead::Closed => break,
                    SubscriptionRead::Timeout => {
                        panic!("failed-close subscription must be closed")
                    }
                }
            }
            assert_eq!(
                observed.last().map(|event| event.code),
                Some("runtime.close_failed")
            );
            assert_eq!(observed.last().map(|event| event.timestamp_ns), Some(41));
            assert_eq!(
                observed
                    .iter()
                    .filter(|event| event.code == "runtime.close_failed")
                    .count(),
                1
            );
            assert!(!observed.iter().any(|event| event.code == "runtime.closed"));
        }
        assert!(clock.payload_drop_count() > 0);
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
            left_runtime.close().expect("Runtime close");
            left_runtime.state()
        });
        let right_runtime = runtime.clone();
        let right_barrier = barrier.clone();
        let right = std::thread::spawn(move || {
            right_barrier.wait();
            right_runtime.close().expect("Runtime close");
            right_runtime.state()
        });
        barrier.wait();

        assert_eq!(left.join().expect("left close"), RuntimeState::Closed);
        assert_eq!(right.join().expect("right close"), RuntimeState::Closed);
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert_eq!(runtime.counts().active_tasks, 0);
    }

    #[test]
    fn supervised_task_panic_is_caught_joined_and_saved_with_its_id() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let payload_drops = Arc::new(AtomicUsize::new(0));
        let observed_payload_drops = Arc::clone(&payload_drops);
        let task = runtime
            .spawn_supervised("panicking-task", move || {
                panic_with_drop_panicking_payload(observed_payload_drops);
            })
            .expect("task");
        let task_id = task.id();
        let joining_task = task.clone();
        let (joined, observed_join) = mpsc::channel();
        let joiner = std::thread::spawn(move || {
            let _ = joined.send(joining_task.join());
        });
        assert_eq!(
            observed_join
                .recv_timeout(Duration::from_secs(2))
                .expect("panicked task join must complete"),
            Ok(SupervisedTaskOutcome::Panicked)
        );
        joiner.join().expect("task join observer");
        assert_eq!(payload_drops.load(Ordering::Acquire), 1);

        let outcome = runtime.close().expect("Runtime close");
        let CloseOutcome::Failed(report) = outcome else {
            panic!("task panic cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::TaskJoin);
        assert_eq!(report.task_id, Some(task_id));
        assert_eq!(report.resource_id, None);
        assert_eq!(runtime.state(), RuntimeState::CloseFailed);
        assert_eq!(task.join(), Ok(SupervisedTaskOutcome::Panicked));
        let CloseOutcome::Failed(later) = runtime.close().expect("repeated Runtime close") else {
            panic!("saved task failure changed to Closed");
        };
        assert!(Arc::ptr_eq(&report, &later));
    }

    // conformance: runtime.task-panic-race
    #[test]
    fn concurrent_external_join_cannot_hide_a_task_panic_from_close() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let task = runtime
            .spawn_supervised("panicking-task", || panic!("scripted task panic"))
            .expect("task");
        assert_eq!(task.completion.wait(), SupervisedTaskOutcome::Panicked);

        let unlinked = Arc::new(Barrier::new(2));
        let release_join = Arc::new(Barrier::new(2));
        let hook_unlinked = Arc::clone(&unlinked);
        let hook_release = Arc::clone(&release_join);
        *runtime
            .inner
            .task_join_after_unlink
            .lock()
            .expect("task join hook lock") = Some(Arc::new(move || {
            hook_unlinked.wait();
            hook_release.wait();
        }));

        let joining_task = task.clone();
        let joiner = std::thread::spawn(move || joining_task.join());
        unlinked.wait();
        runtime
            .inner
            .task_join_after_unlink
            .lock()
            .expect("task join hook lock")
            .take();
        let outcome = runtime.close().expect("Runtime close");
        release_join.wait();
        assert_eq!(
            joiner.join().expect("external joiner"),
            Ok(SupervisedTaskOutcome::Panicked)
        );

        let CloseOutcome::Failed(report) = outcome else {
            panic!("an observed supervised task panic cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::TaskJoin);
        assert_eq!(report.task_id, Some(task.id()));
    }

    // conformance: runtime.self-close-rejected
    // conformance: runtime.task-owned
    #[test]
    fn task_close_without_manual_binding_does_not_self_wait() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let worker_runtime = runtime.clone();
        let (finished, observed_finish) = mpsc::channel();
        let task = runtime
            .spawn_supervised("self-close", move || {
                finished
                    .send(worker_runtime.close())
                    .expect("close completion observer");
            })
            .expect("task");
        let task_id = task.id();

        assert_eq!(
            observed_finish
                .recv_timeout(Duration::from_secs(2))
                .expect("supervised task close must fail fast"),
            Err(CloseRejection::SupervisedTask)
        );
        assert_eq!(runtime.state(), RuntimeState::Active);
        let tasks = runtime.inner.tasks.lock().expect("task registry lock");
        let record = tasks
            .get(&task_id)
            .expect("Runtime retains task until join");
        assert!(record.owner.is_some());
        assert!(
            record
                .completion
                .handle
                .lock()
                .expect("task handle slot")
                .is_some()
        );
        assert!(Arc::ptr_eq(&record.completion, &task.completion));
        drop(tasks);

        runtime.close().expect("external Runtime close");
        assert_eq!(task.join(), Ok(SupervisedTaskOutcome::Completed));
        assert_eq!(runtime.state(), RuntimeState::Closed);
        assert_eq!(runtime.counts().active_tasks, 0);
    }

    // conformance: runtime.task-exit-owned
    #[test]
    fn task_exit_destructor_close_is_rejected_before_state_change() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let worker_runtime = runtime.clone_for_supervision();
        let (result, observed_result) = mpsc::sync_channel(1);
        let task = runtime
            .spawn_supervised("exit-close", move || {
                CLOSE_RUNTIME_ON_THREAD_EXIT.with(|slot| {
                    *slot.borrow_mut() = Some(CloseRuntimeOnThreadExit {
                        runtime: worker_runtime,
                        result,
                    });
                });
            })
            .expect("task");

        assert_eq!(
            observed_result
                .recv_timeout(Duration::from_secs(2))
                .expect("thread-exit close result"),
            Err(CloseRejection::SupervisedTask)
        );
        assert_eq!(runtime.state(), RuntimeState::Active);
        assert_eq!(task.join(), Ok(SupervisedTaskOutcome::Completed));
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    // conformance: runtime.task-exit-self-join
    #[test]
    fn task_exit_destructor_cannot_join_its_own_thread() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (publish_task, receive_task) = mpsc::sync_channel(0);
        let (result, observed_result) = mpsc::sync_channel(1);
        let task = runtime
            .spawn_supervised("exit-join", move || {
                let own_task = receive_task.recv().expect("own task handle");
                JOIN_TASK_ON_THREAD_EXIT.with(|slot| {
                    *slot.borrow_mut() = Some(JoinTaskOnThreadExit {
                        task: own_task,
                        result,
                    });
                });
            })
            .expect("task");
        publish_task.send(task.clone()).expect("publish own task");

        assert_eq!(
            observed_result
                .recv_timeout(Duration::from_secs(2))
                .expect("thread-exit join result"),
            Err(TaskJoinError::SelfJoin)
        );
        assert_eq!(runtime.state(), RuntimeState::Active);
        assert_eq!(task.join(), Ok(SupervisedTaskOutcome::Completed));
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn supervised_task_cannot_join_its_own_runtime_owned_handle() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (publish_handle, receive_handle) = mpsc::channel::<SupervisedTask>();
        let (publish_result, receive_result) = mpsc::channel();
        let task = runtime
            .spawn_supervised("self-join", move || {
                let own_handle = receive_handle.recv().expect("own task handle");
                publish_result
                    .send(own_handle.join())
                    .expect("self-join result");
            })
            .expect("task");
        publish_handle
            .send(task.clone())
            .expect("publish own task handle");

        assert_eq!(
            receive_result
                .recv_timeout(Duration::from_secs(2))
                .expect("self-join must fail fast"),
            Err(TaskJoinError::SelfJoin)
        );
        assert_eq!(
            task.join().expect("external task join"),
            SupervisedTaskOutcome::Completed
        );
        runtime.close().expect("Runtime close");
    }

    #[test]
    fn close_waits_for_a_supervised_task_to_finish_cleanup() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let cancellation = runtime.child_cancellation_token();
        let operation = runtime.create_operation(None).expect("operation");
        operation.start();
        let (operation_cancelled, observed_operation_cancelled) = mpsc::channel();
        operation.on_cancel(move || {
            operation_cancelled
                .send(())
                .expect("operation cancellation observer");
        });
        let (wake, woken) = mpsc::channel();
        cancellation.on_cancel(move || {
            let _ = wake.send(());
        });
        let (cleanup_started, observed_cleanup) = mpsc::channel();
        let (release_cleanup, released) = mpsc::channel();
        let cleanup_operation = operation.clone();
        let task = runtime
            .spawn_supervised("operation-cleanup", move || {
                woken.recv().expect("root cancellation");
                cleanup_started.send(()).expect("cleanup observer");
                released.recv().expect("cleanup release");
                cleanup_operation.finish_cancelled();
            })
            .expect("task");
        let (closed, observed_close) = mpsc::channel();
        let closing_runtime = runtime.clone();
        let closer = std::thread::spawn(move || {
            closing_runtime.close().expect("Runtime close");
            closed.send(()).expect("close observer");
        });

        observed_operation_cancelled
            .recv_timeout(Duration::from_secs(2))
            .expect("close requested operation cancellation");
        assert_eq!(operation.snapshot().state, OperationState::Cancelling);
        assert!(matches!(
            observed_close.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
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
        assert_eq!(
            task.join().expect("supervised task cannot join itself"),
            SupervisedTaskOutcome::Completed
        );
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
            without_logs.read(WaitTimeout::Poll).expect("event read"),
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
        runtime.close().expect("Runtime close");
    }

    #[test]
    fn dropping_observer_does_not_cancel_supervision() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let operation = runtime.create_operation(None).expect("operation");
        drop(operation);

        assert_eq!(runtime.counts().active_operations, 1);
        runtime.close().expect("Runtime close");
        assert_eq!(runtime.counts().active_operations, 0);
    }
}
