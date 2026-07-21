use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex};

use easycon_runtime::{
    CancellationHookRegistration, CancellationToken, ManagedResource, ResourceRegistration,
    Runtime, SupervisedTask, SupervisedTaskOutcome,
};

use crate::{
    ColorStatistics, EdgeMethod, HsvRange, Image, MatchResult, PixelFormat, Roi, TemplateMethod,
    VisionError, VisionLimits,
};

const MAX_NATIVE_WORKERS: usize = 64;
const MAX_QUEUED_JOBS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativePoolOptions {
    workers: usize,
    max_queued_jobs: usize,
}

impl NativePoolOptions {
    pub fn new(workers: usize, max_queued_jobs: usize) -> Result<Self, VisionError> {
        if workers == 0 || max_queued_jobs == 0 {
            return Err(VisionError::validation(
                "native pool workers and queue capacity must be non-zero",
            ));
        }
        if workers > MAX_NATIVE_WORKERS || max_queued_jobs > MAX_QUEUED_JOBS {
            return Err(VisionError::limit(
                "native pool configuration exceeds hard ceilings",
            ));
        }
        Ok(Self {
            workers,
            max_queued_jobs,
        })
    }

    #[must_use]
    pub const fn workers(self) -> usize {
        self.workers
    }

    #[must_use]
    pub const fn max_queued_jobs(self) -> usize {
        self.max_queued_jobs
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativePoolCounts {
    pub queued: usize,
    pub in_flight: usize,
    pub workers: usize,
    pub closed: bool,
}

#[derive(Clone)]
pub struct NativePool {
    resource: Arc<NativePoolResource>,
}

struct NativePoolResource {
    options: NativePoolOptions,
    commit_gate: Mutex<()>,
    start: Mutex<WorkerStart>,
    start_changed: Condvar,
    state: Mutex<PoolState>,
    changed: Condvar,
    tasks: Mutex<Vec<SupervisedTask>>,
    registration: Mutex<Option<ResourceRegistration>>,
    retention: Mutex<Option<Arc<NativePoolResource>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerStart {
    Pending,
    Run,
    Abort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PoolLifecycle {
    Open,
    Closing,
    Closed,
}

struct PoolState {
    lifecycle: PoolLifecycle,
    next_ticket: u64,
    queue: VecDeque<ErasedJob>,
    in_flight: usize,
    close_result: Option<Result<(), VisionError>>,
}

struct ErasedJob {
    ticket: u64,
    run: Option<Box<dyn FnOnce() + Send + 'static>>,
    cancel: Option<Box<dyn FnOnce(VisionError) + Send + 'static>>,
}

struct JobCompletion<T> {
    result: Mutex<Option<Result<T, VisionError>>>,
    changed: Condvar,
}

struct JobWaiter<T> {
    completion: Arc<JobCompletion<T>>,
    _cancellation: CancellationHookRegistration,
}

impl NativePool {
    pub fn new(runtime: &Runtime, options: NativePoolOptions) -> Result<Self, VisionError> {
        let resource = Arc::new(NativePoolResource {
            options,
            commit_gate: Mutex::new(()),
            start: Mutex::new(WorkerStart::Pending),
            start_changed: Condvar::new(),
            state: Mutex::new(PoolState {
                lifecycle: PoolLifecycle::Open,
                next_ticket: 1,
                queue: VecDeque::new(),
                in_flight: 0,
                close_result: None,
            }),
            changed: Condvar::new(),
            tasks: Mutex::new(Vec::with_capacity(options.workers)),
            registration: Mutex::new(None),
            retention: Mutex::new(None),
        });

        let commit = lock_recover(&resource.commit_gate);
        for worker in 0..options.workers {
            let owned = Arc::clone(&resource);
            let task = match runtime
                .spawn_supervised(format!("easycon-native-pool-{worker}"), move || {
                    owned.worker_loop()
                }) {
                Ok(task) => task,
                Err(error) => {
                    resource.set_start(WorkerStart::Abort);
                    drop(commit);
                    resource.join_constructed_workers();
                    return Err(VisionError::from_runtime(error));
                }
            };
            lock_recover(&resource.tasks).push(task);
        }

        let managed: Arc<dyn ManagedResource> = resource.clone();
        let registration = match runtime.register_resource(managed) {
            Ok(registration) => registration,
            Err(error) => {
                resource.set_start(WorkerStart::Abort);
                drop(commit);
                resource.join_constructed_workers();
                return Err(VisionError::from_runtime(error));
            }
        };
        *lock_recover(&resource.registration) = Some(registration);
        *lock_recover(&resource.retention) = Some(Arc::clone(&resource));
        resource.set_start(WorkerStart::Run);
        drop(commit);
        Ok(Self { resource })
    }

    pub fn close(&self) -> Result<(), VisionError> {
        self.resource.close_inner()
    }

    pub fn decode(
        &self,
        encoded: &[u8],
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<Image, VisionError> {
        let encoded = encoded.to_vec();
        let limits = *limits;
        self.execute(cancellation, move || {
            Image::decode_direct(&encoded, &limits).map_err(VisionError::from)
        })
    }

    pub fn encode_png(
        &self,
        image: &Image,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, VisionError> {
        let image = image.clone();
        let limits = *limits;
        self.execute(cancellation, move || {
            image.encode_png_direct(&limits).map_err(VisionError::from)
        })
    }

    pub fn convert(
        &self,
        image: &Image,
        output_format: PixelFormat,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<Image, VisionError> {
        let image = image.clone();
        let limits = *limits;
        self.execute(cancellation, move || {
            image
                .convert_direct(output_format, &limits)
                .map_err(VisionError::from)
        })
    }

    pub fn crop(
        &self,
        image: &Image,
        roi: Roi,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<Image, VisionError> {
        let image = image.clone();
        let limits = *limits;
        self.execute(cancellation, move || {
            image.crop_direct(roi, &limits).map_err(VisionError::from)
        })
    }

    pub fn match_template(
        &self,
        search: &Image,
        target: &Image,
        method: TemplateMethod,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<MatchResult, VisionError> {
        let search = search.clone();
        let target = target.clone();
        let limits = *limits;
        self.execute(cancellation, move || {
            crate::matching::match_template_direct(&search, &target, method, &limits)
                .map_err(VisionError::from)
        })
    }

    pub fn preprocess_edge(
        &self,
        image: &Image,
        method: EdgeMethod,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<Image, VisionError> {
        let image = image.clone();
        let limits = *limits;
        self.execute(cancellation, move || {
            crate::matching::preprocess_edge_direct(&image, method, &limits)
                .map_err(VisionError::from)
        })
    }

    pub fn match_edge(
        &self,
        search: &Image,
        target: &Image,
        method: EdgeMethod,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<MatchResult, VisionError> {
        let search = search.clone();
        let target = target.clone();
        let limits = *limits;
        self.execute(cancellation, move || {
            crate::matching::match_edge_direct(&search, &target, method, &limits)
                .map_err(VisionError::from)
        })
    }

    pub fn hsv_statistics(
        &self,
        image: &Image,
        roi: Roi,
        range: HsvRange,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<ColorStatistics, VisionError> {
        let image = image.clone();
        let limits = *limits;
        self.execute(cancellation, move || {
            image
                .hsv_statistics_direct(roi, range, &limits)
                .map_err(VisionError::from)
        })
    }

    #[must_use]
    pub fn counts(&self) -> NativePoolCounts {
        let state = lock_recover(&self.resource.state);
        NativePoolCounts {
            queued: state.queue.len(),
            in_flight: state.in_flight,
            workers: self.resource.options.workers,
            closed: state.lifecycle == PoolLifecycle::Closed,
        }
    }

    pub(crate) fn execute<T, F>(
        &self,
        cancellation: &CancellationToken,
        call: F,
    ) -> Result<T, VisionError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, VisionError> + Send + 'static,
    {
        self.resource.submit(cancellation, call)?.wait()
    }
}

impl NativePoolResource {
    fn set_start(&self, start: WorkerStart) {
        *lock_recover(&self.start) = start;
        self.start_changed.notify_all();
    }

    fn wait_for_start(&self) -> WorkerStart {
        let mut start = lock_recover(&self.start);
        while *start == WorkerStart::Pending {
            start = wait_recover(&self.start_changed, start);
        }
        *start
    }

    fn worker_loop(self: Arc<Self>) {
        if self.wait_for_start() == WorkerStart::Abort {
            return;
        }
        loop {
            let job = {
                let mut state = lock_recover(&self.state);
                loop {
                    if let Some(job) = state.queue.pop_front() {
                        state.in_flight += 1;
                        break Some(job);
                    }
                    if state.lifecycle != PoolLifecycle::Open {
                        break None;
                    }
                    state = wait_recover(&self.changed, state);
                }
            };
            let Some(job) = job else {
                return;
            };
            job.run();
            let mut state = lock_recover(&self.state);
            debug_assert!(state.in_flight > 0, "native pool in-flight count underflow");
            state.in_flight -= 1;
            self.changed.notify_all();
        }
    }

    fn submit<T, F>(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
        call: F,
    ) -> Result<JobWaiter<T>, VisionError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, VisionError> + Send + 'static,
    {
        if cancellation.is_cancelled() {
            return Err(VisionError::cancelled(
                "native job was cancelled before admission",
            ));
        }
        let completion = Arc::new(JobCompletion {
            result: Mutex::new(None),
            changed: Condvar::new(),
        });
        let run_completion = Arc::clone(&completion);
        let run_cancellation = cancellation.clone();
        let run = Box::new(move || {
            if run_cancellation.is_cancelled() {
                run_completion.finish(Err(VisionError::cancelled(
                    "native job was cancelled before execution",
                )));
                return;
            }
            let result = match catch_unwind(AssertUnwindSafe(call)) {
                Ok(result) => result,
                Err(payload) => {
                    let _ = catch_unwind(AssertUnwindSafe(|| drop(payload)));
                    Err(VisionError::internal("native job panicked"))
                }
            };
            if run_cancellation.is_cancelled() {
                run_completion.finish(Err(VisionError::cancelled(
                    "native job was cancelled while in flight",
                )));
            } else {
                run_completion.finish(result);
            }
        });
        let cancel_completion = Arc::clone(&completion);
        let cancel = Box::new(move |error| cancel_completion.finish(Err(error)));

        let ticket = {
            let mut state = lock_recover(&self.state);
            if state.lifecycle != PoolLifecycle::Open {
                return Err(VisionError::pool_closed("native pool is closed"));
            }
            if state.queue.len() >= self.options.max_queued_jobs {
                return Err(VisionError::limit("native job queue is full"));
            }
            let ticket = state.next_ticket;
            state.next_ticket = state
                .next_ticket
                .checked_add(1)
                .ok_or_else(|| VisionError::internal("native job ticket space exhausted"))?;
            state.queue.push_back(ErasedJob {
                ticket,
                run: Some(run),
                cancel: Some(cancel),
            });
            ticket
        };
        self.changed.notify_one();

        let weak = Arc::downgrade(self);
        let hook = cancellation.on_cancel_scoped(move || {
            if let Some(resource) = weak.upgrade() {
                resource.cancel_queued(
                    ticket,
                    VisionError::cancelled("queued native job was cancelled"),
                );
            }
        });
        Ok(JobWaiter {
            completion,
            _cancellation: hook,
        })
    }

    fn cancel_queued(&self, ticket: u64, error: VisionError) {
        let job = {
            let mut state = lock_recover(&self.state);
            state
                .queue
                .iter()
                .position(|job| job.ticket == ticket)
                .and_then(|index| state.queue.remove(index))
        };
        if let Some(job) = job {
            job.cancel(error);
            self.changed.notify_all();
        }
    }

    fn close_inner(&self) -> Result<(), VisionError> {
        let _commit = lock_recover(&self.commit_gate);
        let queued = {
            let mut state = lock_recover(&self.state);
            loop {
                match state.lifecycle {
                    PoolLifecycle::Open => {
                        state.lifecycle = PoolLifecycle::Closing;
                        break std::mem::take(&mut state.queue);
                    }
                    PoolLifecycle::Closing => {
                        state = wait_recover(&self.changed, state);
                    }
                    PoolLifecycle::Closed => {
                        return state.close_result.clone().unwrap_or_else(|| {
                            Err(VisionError::internal(
                                "closed native pool has no saved result",
                            ))
                        });
                    }
                }
            }
        };
        for job in queued {
            job.cancel(VisionError::pool_closed(
                "queued native job was cancelled by pool close",
            ));
        }
        self.changed.notify_all();

        let tasks = std::mem::take(&mut *lock_recover(&self.tasks));
        let mut result = Ok(());
        for task in tasks {
            match task.join() {
                Ok(SupervisedTaskOutcome::Completed) => {}
                Ok(SupervisedTaskOutcome::Panicked) => {
                    if result.is_ok() {
                        result = Err(VisionError::internal(
                            "native pool worker panicked outside the job boundary",
                        ));
                    }
                }
                Err(_) => {
                    if result.is_ok() {
                        result = Err(VisionError::internal(
                            "native pool worker attempted to join itself",
                        ));
                    }
                }
            }
        }

        if let Some(registration) = lock_recover(&self.registration).take() {
            registration.unregister();
        }
        lock_recover(&self.retention).take();
        let mut state = lock_recover(&self.state);
        if state.in_flight != 0 || !state.queue.is_empty() {
            result = Err(VisionError::internal(
                "native pool workers exited before ownership converged",
            ));
        }
        state.close_result = Some(result.clone());
        state.lifecycle = PoolLifecycle::Closed;
        self.changed.notify_all();
        result
    }

    fn join_constructed_workers(&self) {
        let tasks = std::mem::take(&mut *lock_recover(&self.tasks));
        for task in tasks {
            let _ = task.join();
        }
    }
}

impl ManagedResource for NativePoolResource {
    fn close(&self) {
        let _ = self.close_inner();
    }
}

impl ErasedJob {
    fn run(mut self) {
        if let Some(run) = self.run.take() {
            run();
        }
    }

    fn cancel(mut self, error: VisionError) {
        if let Some(run) = self.run.take() {
            let _ = catch_unwind(AssertUnwindSafe(|| drop(run)));
        }
        if let Some(cancel) = self.cancel.take() {
            cancel(error);
        }
    }
}

impl<T> JobCompletion<T> {
    fn finish(&self, result: Result<T, VisionError>) {
        let mut slot = lock_recover(&self.result);
        debug_assert!(slot.is_none(), "native job completes exactly once");
        if slot.is_none() {
            *slot = Some(result);
            self.changed.notify_all();
        }
    }

    fn wait(&self) -> Result<T, VisionError> {
        let mut slot = lock_recover(&self.result);
        loop {
            if let Some(result) = slot.take() {
                return result;
            }
            slot = wait_recover(&self.changed, slot);
        }
    }
}

impl<T> JobWaiter<T> {
    fn wait(self) -> Result<T, VisionError> {
        self.completion.wait()
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait_recover<'a, T>(
    condvar: &Condvar,
    guard: std::sync::MutexGuard<'a, T>,
) -> std::sync::MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use easycon_runtime::{CloseOutcome, VirtualClock};

    use crate::VisionErrorKind;

    use super::*;

    fn pool(workers: usize, queued: usize) -> (Runtime, NativePool) {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let pool = NativePool::new(
            &runtime,
            NativePoolOptions::new(workers, queued).expect("test options"),
        )
        .expect("test pool");
        (runtime, pool)
    }

    #[test]
    fn options_enforce_hard_bounds() {
        assert_eq!(
            NativePoolOptions::new(0, 1)
                .expect_err("zero workers")
                .kind(),
            VisionErrorKind::Validation
        );
        assert_eq!(
            NativePoolOptions::new(1, MAX_QUEUED_JOBS + 1)
                .expect_err("queue ceiling")
                .kind(),
            VisionErrorKind::Limit
        );
    }

    #[test]
    fn one_worker_starts_jobs_in_fifo_ticket_order() {
        let (runtime, pool) = pool(1, 4);
        let token = CancellationToken::root();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (entered, observed_entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let first_order = Arc::clone(&order);
        let first = pool
            .resource
            .submit(&token, move || {
                entered.send(()).expect("first entered");
                released.recv().expect("first release");
                lock_recover(&first_order).push(1);
                Ok(1)
            })
            .expect("first job");
        observed_entered.recv().expect("first running");
        let second_order = Arc::clone(&order);
        let second = pool
            .resource
            .submit(&token, move || {
                lock_recover(&second_order).push(2);
                Ok(2)
            })
            .expect("second job");
        let third_order = Arc::clone(&order);
        let third = pool
            .resource
            .submit(&token, move || {
                lock_recover(&third_order).push(3);
                Ok(3)
            })
            .expect("third job");
        release.send(()).expect("release first");
        assert_eq!(first.wait(), Ok(1));
        assert_eq!(second.wait(), Ok(2));
        assert_eq!(third.wait(), Ok(3));
        assert_eq!(*lock_recover(&order), vec![1, 2, 3]);
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn queued_cancel_never_invokes_native_job() {
        let (runtime, pool) = pool(1, 2);
        let running = CancellationToken::root();
        let (entered, observed_entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let first = pool
            .resource
            .submit(&running, move || {
                entered.send(()).expect("first entered");
                released.recv().expect("first release");
                Ok(())
            })
            .expect("first job");
        observed_entered.recv().expect("first running");

        let invoked = Arc::new(AtomicUsize::new(0));
        let cancelled = CancellationToken::root();
        let observed_invoked = Arc::clone(&invoked);
        let second = pool
            .resource
            .submit(&cancelled, move || {
                observed_invoked.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
            .expect("queued job");
        cancelled.cancel();
        assert_eq!(
            second.wait().expect_err("queued cancellation").kind(),
            VisionErrorKind::Cancelled
        );
        assert_eq!(invoked.load(Ordering::Acquire), 0);
        release.send(()).expect("release first");
        first.wait().expect("first result");
        assert_eq!(invoked.load(Ordering::Acquire), 0);
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn in_flight_cancel_waits_for_owner_return_before_terminal_result() {
        let (runtime, pool) = pool(1, 1);
        let token = CancellationToken::root();
        let (entered, observed_entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let waiter = pool
            .resource
            .submit(&token, move || {
                entered.send(()).expect("job entered");
                released.recv().expect("job release");
                Ok(7)
            })
            .expect("job");
        observed_entered.recv().expect("job running");
        token.cancel();
        let (finished, observed_finished) = mpsc::channel();
        let joiner = std::thread::spawn(move || finished.send(waiter.wait()).expect("result"));
        assert!(observed_finished.try_recv().is_err());
        release.send(()).expect("release native call");
        assert_eq!(
            observed_finished
                .recv()
                .expect("cancel result")
                .expect_err("in-flight cancellation")
                .kind(),
            VisionErrorKind::Cancelled
        );
        joiner.join().expect("waiter join");
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn close_cancels_queue_and_joins_in_flight_work() {
        let (runtime, pool) = pool(1, 2);
        let token = CancellationToken::root();
        let (entered, observed_entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let first = pool
            .resource
            .submit(&token, move || {
                entered.send(()).expect("job entered");
                released.recv().expect("job release");
                Ok(())
            })
            .expect("first job");
        observed_entered.recv().expect("job running");
        let second = pool.resource.submit(&token, || Ok(())).expect("queued job");

        let closing = pool.clone();
        let (closed, observed_closed) = mpsc::channel();
        let closer =
            std::thread::spawn(move || closed.send(closing.close()).expect("close result"));
        assert_eq!(
            second.wait().expect_err("queued close").kind(),
            VisionErrorKind::PoolClosed
        );
        assert!(observed_closed.try_recv().is_err());
        release.send(()).expect("release in-flight job");
        first.wait().expect("in-flight result");
        observed_closed
            .recv()
            .expect("close completion")
            .expect("close");
        closer.join().expect("closer");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn job_panic_isolated_and_worker_accepts_the_next_ticket() {
        let (runtime, pool) = pool(1, 2);
        let token = CancellationToken::root();
        let error = pool
            .execute::<(), _>(&token, || panic!("scripted native job panic"))
            .expect_err("panic is contained");
        assert_eq!(error.kind(), VisionErrorKind::Internal);
        assert_eq!(pool.execute(&token, || Ok(9)), Ok(9));
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn runtime_close_owns_pool_shutdown_and_registry_convergence() {
        let (runtime, pool) = pool(2, 2);
        assert_eq!(runtime.counts().active_resources, 1);
        assert_eq!(runtime.counts().active_tasks, 3);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        assert!(pool.counts().closed);
        assert_eq!(runtime.counts().active_resources, 0);
        assert_eq!(runtime.counts().active_tasks, 0);
        assert_eq!(pool.close(), Ok(()));
    }
}
