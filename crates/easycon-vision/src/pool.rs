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
    reserved: usize,
    #[cfg(test)]
    admission_waiters: usize,
    queue: VecDeque<ErasedJob>,
    in_flight: usize,
    close_result: Option<Result<(), VisionError>>,
}

struct AdmissionReservation {
    resource: Arc<NativePoolResource>,
    ticket: u64,
    pending: bool,
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
                reserved: 0,
                #[cfg(test)]
                admission_waiters: 0,
                queue: VecDeque::with_capacity(options.max_queued_jobs),
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
        let limits = *limits;
        let reservation = self.resource.reserve(cancellation)?;
        Image::validate_encoded_size(encoded.len(), &limits).map_err(VisionError::from)?;
        let encoded = own_encoded_for_decode(encoded);
        let (completion, job) = make_job(cancellation, reservation.ticket, move || {
            Image::decode_direct(&encoded, &limits).map_err(VisionError::from)
        });
        let ticket = reservation.commit(job)?;
        self.resource
            .waiter(cancellation, ticket, completion)
            .wait()
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

fn own_encoded_for_decode(encoded: &[u8]) -> Vec<u8> {
    let owned = encoded.to_vec();
    #[cfg(test)]
    test_decode_probe::observe_owned_copy(encoded, &owned);
    owned
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
                    if state.lifecycle != PoolLifecycle::Open {
                        if state.reserved == 0 {
                            break None;
                        }
                        state = wait_recover(&self.changed, state);
                        continue;
                    }
                    if let Some(job) = state.queue.pop_front() {
                        state.in_flight += 1;
                        break Some(job);
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
        let reservation = self.reserve(cancellation)?;
        let (completion, job) = make_job(cancellation, reservation.ticket, call);
        let ticket = reservation.commit(job)?;
        Ok(self.waiter(cancellation, ticket, completion))
    }

    fn reserve(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
    ) -> Result<AdmissionReservation, VisionError> {
        if cancellation.is_cancelled() {
            return Err(VisionError::cancelled(
                "native job was cancelled before admission",
            ));
        }
        let weak = Arc::downgrade(self);
        let _cancellation = cancellation.on_cancel_scoped(move || {
            if let Some(resource) = weak.upgrade() {
                let _state = lock_recover(&resource.state);
                resource.changed.notify_all();
            }
        });
        let ticket = {
            let mut state = lock_recover(&self.state);
            loop {
                if cancellation.is_cancelled() {
                    return Err(VisionError::cancelled(
                        "native job was cancelled before admission",
                    ));
                }
                if state.lifecycle != PoolLifecycle::Open {
                    return Err(VisionError::pool_closed("native pool is closed"));
                }
                if state.reserved == 0 {
                    break;
                }
                #[cfg(test)]
                {
                    state.admission_waiters += 1;
                    self.changed.notify_all();
                }
                state = wait_recover(&self.changed, state);
                #[cfg(test)]
                {
                    debug_assert!(state.admission_waiters > 0);
                    state.admission_waiters -= 1;
                    self.changed.notify_all();
                }
            }
            if state.queue.len() >= self.options.max_queued_jobs {
                return Err(VisionError::limit("native job queue is full"));
            }
            let ticket = state.next_ticket;
            state.next_ticket = state
                .next_ticket
                .checked_add(1)
                .ok_or_else(|| VisionError::internal("native job ticket space exhausted"))?;
            state.reserved = 1;
            ticket
        };
        Ok(AdmissionReservation {
            resource: Arc::clone(self),
            ticket,
            pending: true,
        })
    }

    fn waiter<T>(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
        ticket: u64,
        completion: Arc<JobCompletion<T>>,
    ) -> JobWaiter<T> {
        let weak = Arc::downgrade(self);
        let hook = cancellation.on_cancel_scoped(move || {
            if let Some(resource) = weak.upgrade() {
                resource.cancel_queued(
                    ticket,
                    VisionError::cancelled("queued native job was cancelled"),
                );
            }
        });
        JobWaiter {
            completion,
            _cancellation: hook,
        }
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
                        self.changed.notify_all();
                        while state.reserved != 0 {
                            state = wait_recover(&self.changed, state);
                        }
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
        if state.in_flight != 0 || state.reserved != 0 || !state.queue.is_empty() {
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

impl AdmissionReservation {
    fn commit(mut self, job: ErasedJob) -> Result<u64, VisionError> {
        let mut rejected = Some(job);
        let committed = {
            let mut state = lock_recover(&self.resource.state);
            if state.reserved == 0 {
                false
            } else {
                state.reserved = 0;
                state
                    .queue
                    .push_back(rejected.take().expect("pending admission owns its job"));
                true
            }
        };
        self.pending = false;
        self.resource.changed.notify_all();
        if committed {
            Ok(self.ticket)
        } else {
            drop(rejected);
            Err(VisionError::internal(
                "native admission reservation was lost before commit",
            ))
        }
    }
}

impl Drop for AdmissionReservation {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        {
            let mut state = lock_recover(&self.resource.state);
            if state.reserved != 0 {
                state.reserved = 0;
            }
        }
        self.resource.changed.notify_all();
    }
}

fn make_job<T, F>(
    cancellation: &CancellationToken,
    ticket: u64,
    call: F,
) -> (Arc<JobCompletion<T>>, ErasedJob)
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, VisionError> + Send + 'static,
{
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
    (
        completion,
        ErasedJob {
            ticket,
            run: Some(run),
            cancel: Some(cancel),
        },
    )
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
pub(crate) mod test_decode_probe {
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    static PROBE_GATE: Mutex<()> = Mutex::new(());
    static EXPECTED_LENGTH: AtomicUsize = AtomicUsize::new(0);
    static EXPECTED_FIRST: AtomicU8 = AtomicU8::new(0);
    static EXPECTED_LAST: AtomicU8 = AtomicU8::new(0);
    static OWNED_COPIES: AtomicUsize = AtomicUsize::new(0);
    static NATIVE_DECODE_CALLS: AtomicUsize = AtomicUsize::new(0);

    pub(crate) struct DecodeProbe {
        _gate: MutexGuard<'static, ()>,
    }

    impl DecodeProbe {
        pub(crate) fn start(encoded: &[u8]) -> Self {
            assert!(!encoded.is_empty(), "decode probe input must be nonempty");
            let gate = PROBE_GATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            EXPECTED_FIRST.store(encoded[0], Ordering::Release);
            EXPECTED_LAST.store(encoded[encoded.len() - 1], Ordering::Release);
            OWNED_COPIES.store(0, Ordering::Release);
            NATIVE_DECODE_CALLS.store(0, Ordering::Release);
            EXPECTED_LENGTH.store(encoded.len(), Ordering::Release);
            Self { _gate: gate }
        }

        pub(crate) fn owned_copies(&self) -> usize {
            OWNED_COPIES.load(Ordering::Acquire)
        }

        pub(crate) fn native_decode_calls(&self) -> usize {
            NATIVE_DECODE_CALLS.load(Ordering::Acquire)
        }
    }

    impl Drop for DecodeProbe {
        fn drop(&mut self) {
            EXPECTED_LENGTH.store(0, Ordering::Release);
        }
    }

    pub(crate) fn observe_owned_copy(source: &[u8], owned: &[u8]) {
        if matches_probe(source) {
            assert_eq!(owned, source, "decode owned copy changed input bytes");
            assert_ne!(
                owned.as_ptr(),
                source.as_ptr(),
                "decode owned copy must use distinct storage"
            );
            OWNED_COPIES.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn observe_native_decode(encoded: &[u8]) {
        if matches_probe(encoded) {
            NATIVE_DECODE_CALLS.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn matches_probe(encoded: &[u8]) -> bool {
        let expected_length = EXPECTED_LENGTH.load(Ordering::Acquire);
        expected_length != 0
            && encoded.len() == expected_length
            && encoded.first().copied() == Some(EXPECTED_FIRST.load(Ordering::Acquire))
            && encoded.last().copied() == Some(EXPECTED_LAST.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

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

    fn decode_probe_input(length: usize, marker: u8) -> Vec<u8> {
        let mut encoded = vec![marker; length];
        encoded[length - 1] = !marker;
        encoded
    }

    fn probe_limits(max_encoded_bytes: usize) -> VisionLimits {
        VisionLimits::try_for_images(max_encoded_bytes, 64, 64, 4096, 16_384, 1024)
            .expect("decode probe limits")
    }

    fn wait_for_pool_counts(pool: &NativePool, expected: NativePoolCounts) {
        let mut state = lock_recover(&pool.resource.state);
        loop {
            let actual = NativePoolCounts {
                queued: state.queue.len(),
                in_flight: state.in_flight,
                workers: pool.resource.options.workers,
                closed: state.lifecycle == PoolLifecycle::Closed,
            };
            if actual == expected {
                return;
            }
            let (next, timeout) = pool
                .resource
                .changed
                .wait_timeout(state, Duration::from_secs(5))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(!timeout.timed_out(), "native pool counts did not converge");
            state = next;
        }
    }

    fn wait_for_pool_lifecycle(pool: &NativePool, expected: PoolLifecycle) {
        let mut state = lock_recover(&pool.resource.state);
        while state.lifecycle != expected {
            let (next, timeout) = pool
                .resource
                .changed
                .wait_timeout(state, Duration::from_secs(5))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                !timeout.timed_out(),
                "native pool lifecycle did not converge"
            );
            state = next;
        }
    }

    fn wait_for_admission_waiters(pool: &NativePool, expected: usize) {
        let mut state = lock_recover(&pool.resource.state);
        while state.admission_waiters != expected {
            let (next, timeout) = pool
                .resource
                .changed
                .wait_timeout(state, Duration::from_secs(5))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                !timeout.timed_out(),
                "native admission waiter count did not converge"
            );
            state = next;
        }
    }

    struct ReentrantCountDrop {
        pool: NativePool,
        observed: Option<mpsc::Sender<NativePoolCounts>>,
    }

    impl Drop for ReentrantCountDrop {
        fn drop(&mut self) {
            if let Some(observed) = self.observed.take() {
                observed
                    .send(self.pool.counts())
                    .expect("reentrant drop observer");
            }
        }
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
    fn oversized_decode_rejects_before_input_owned_copy_or_native_call() {
        let (runtime, pool) = pool(1, 1);
        let cancellation = CancellationToken::root();
        let encoded = decode_probe_input(1_048_597, 0x31);
        let limits = probe_limits(1024);
        let pool_before = pool.counts();
        let runtime_before = runtime.counts();
        let native_before = crate::native_resource_counts().expect("native counts before decode");
        let probe = test_decode_probe::DecodeProbe::start(&encoded);

        let error = pool
            .decode(&encoded, &limits, &cancellation)
            .expect_err("oversized decode must be rejected");

        assert_eq!(error.kind(), VisionErrorKind::Limit);
        assert_eq!(error.message(), "encoded image exceeds limits");
        assert_eq!(probe.native_decode_calls(), 0);
        wait_for_pool_counts(&pool, pool_before);
        assert_eq!(runtime.counts(), runtime_before);
        assert_eq!(
            crate::native_resource_counts().expect("native counts after decode"),
            native_before
        );
        let owned_copies = probe.owned_copies();
        drop(probe);
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        assert_eq!(
            owned_copies, 0,
            "oversized rejection completed an input-sized owned copy"
        );
    }

    #[test]
    fn pre_cancelled_decode_rejects_before_input_owned_copy_or_native_call() {
        let (runtime, pool) = pool(1, 1);
        let cancellation = CancellationToken::root();
        cancellation.cancel();
        let encoded = decode_probe_input(1_048_609, 0x42);
        let limits = probe_limits(1024);
        let pool_before = pool.counts();
        let runtime_before = runtime.counts();
        let native_before = crate::native_resource_counts().expect("native counts before decode");
        let probe = test_decode_probe::DecodeProbe::start(&encoded);

        let error = pool
            .decode(&encoded, &limits, &cancellation)
            .expect_err("pre-cancelled decode must be rejected");

        assert_eq!(error.kind(), VisionErrorKind::Cancelled);
        assert_eq!(error.message(), "native job was cancelled before admission");
        assert_eq!(probe.native_decode_calls(), 0);
        wait_for_pool_counts(&pool, pool_before);
        assert_eq!(runtime.counts(), runtime_before);
        assert_eq!(
            crate::native_resource_counts().expect("native counts after decode"),
            native_before
        );
        let owned_copies = probe.owned_copies();
        drop(probe);
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        assert_eq!(
            owned_copies, 0,
            "pre-cancelled rejection completed an input-sized owned copy"
        );
    }

    #[test]
    fn closed_pool_decode_rejects_before_input_owned_copy_or_native_call() {
        let (runtime, pool) = pool(1, 1);
        pool.close().expect("pool close before decode");
        let cancellation = CancellationToken::root();
        let encoded = decode_probe_input(1_048_631, 0x53);
        let limits = probe_limits(1024);
        let pool_before = pool.counts();
        let runtime_before = runtime.counts();
        let native_before = crate::native_resource_counts().expect("native counts before decode");
        let probe = test_decode_probe::DecodeProbe::start(&encoded);

        let error = pool
            .decode(&encoded, &limits, &cancellation)
            .expect_err("closed pool decode must be rejected");

        assert_eq!(error.kind(), VisionErrorKind::PoolClosed);
        assert_eq!(error.message(), "native pool is closed");
        assert_eq!(probe.native_decode_calls(), 0);
        wait_for_pool_counts(&pool, pool_before);
        assert_eq!(runtime.counts(), runtime_before);
        assert_eq!(
            crate::native_resource_counts().expect("native counts after decode"),
            native_before
        );
        let owned_copies = probe.owned_copies();
        drop(probe);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        assert_eq!(
            owned_copies, 0,
            "closed-pool rejection completed an input-sized owned copy"
        );
    }

    #[test]
    fn full_queue_decode_rejects_before_input_owned_copy_or_native_call() {
        let (runtime, pool) = pool(1, 1);
        let cancellation = CancellationToken::root();
        let (entered, observed_entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let running = pool
            .resource
            .submit(&cancellation, move || {
                entered.send(()).expect("running job entered");
                released.recv().expect("running job release");
                Ok(())
            })
            .expect("running job");
        observed_entered.recv().expect("worker is blocked");
        let queued = pool
            .resource
            .submit(&cancellation, || Ok(()))
            .expect("queue filler");
        let encoded = decode_probe_input(1_048_649, 0x64);
        let limits = probe_limits(1024);
        let pool_before = pool.counts();
        assert_eq!(pool_before.queued, 1);
        assert_eq!(pool_before.in_flight, 1);
        let runtime_before = runtime.counts();
        let native_before = crate::native_resource_counts().expect("native counts before decode");
        let probe = test_decode_probe::DecodeProbe::start(&encoded);

        let error = pool
            .decode(&encoded, &limits, &cancellation)
            .expect_err("full queue decode must be rejected");

        assert_eq!(error.kind(), VisionErrorKind::Limit);
        assert_eq!(error.message(), "native job queue is full");
        assert_eq!(probe.native_decode_calls(), 0);
        wait_for_pool_counts(&pool, pool_before);
        assert_eq!(runtime.counts(), runtime_before);
        assert_eq!(
            crate::native_resource_counts().expect("native counts after decode"),
            native_before
        );
        let owned_copies = probe.owned_copies();
        drop(probe);
        release.send(()).expect("release running job");
        running.wait().expect("running job result");
        queued.wait().expect("queued job result");
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        assert_eq!(
            owned_copies, 0,
            "full-queue rejection completed an input-sized owned copy"
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
    fn later_admission_waits_for_reserved_builder_and_preserves_fifo() {
        let (runtime, pool) = pool(1, 2);
        let cancellation = CancellationToken::root();
        let order = Arc::new(Mutex::new(Vec::new()));
        let reservation = pool
            .resource
            .reserve(&cancellation)
            .expect("earlier reserved admission");

        let later_pool = pool.clone();
        let later_cancellation = cancellation.clone();
        let later_order = Arc::clone(&order);
        let (submitted, observed_submitted) = mpsc::channel();
        let submitter = std::thread::spawn(move || {
            let waiter = later_pool.resource.submit(&later_cancellation, move || {
                lock_recover(&later_order).push(2);
                Ok(2)
            });
            submitted.send(waiter).expect("later submit result");
        });
        wait_for_admission_waiters(&pool, 1);
        assert!(observed_submitted.try_recv().is_err());

        let earlier_order = Arc::clone(&order);
        let (completion, job) = make_job(&cancellation, reservation.ticket, move || {
            lock_recover(&earlier_order).push(1);
            Ok(1)
        });
        let ticket = reservation.commit(job).expect("earlier commit");
        let earlier = pool.resource.waiter(&cancellation, ticket, completion);
        let later = observed_submitted
            .recv()
            .expect("later submit completion")
            .expect("later admission");
        submitter.join().expect("submitter join");

        assert_eq!(earlier.wait(), Ok(1));
        assert_eq!(later.wait(), Ok(2));
        assert_eq!(*lock_recover(&order), vec![1, 2]);
        wait_for_admission_waiters(&pool, 0);
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn waiting_admission_cancellation_wakes_before_reserved_builder_finishes() {
        let (runtime, pool) = pool(1, 1);
        let reservation = pool
            .resource
            .reserve(&CancellationToken::root())
            .expect("earlier reserved admission");
        let cancellation = CancellationToken::root();
        let invoked = Arc::new(AtomicUsize::new(0));
        let waiting_pool = pool.clone();
        let waiting_cancellation = cancellation.clone();
        let observed_invoked = Arc::clone(&invoked);
        let (submitted, observed_submitted) = mpsc::channel();
        let submitter = std::thread::spawn(move || {
            let result = waiting_pool
                .resource
                .submit(&waiting_cancellation, move || {
                    observed_invoked.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                });
            submitted.send(result).expect("cancelled submit result");
        });
        wait_for_admission_waiters(&pool, 1);

        cancellation.cancel();
        let error = match observed_submitted
            .recv_timeout(Duration::from_secs(5))
            .expect("cancellation wakes admission waiter")
        {
            Ok(_) => panic!("cancelled waiter unexpectedly admitted a job"),
            Err(error) => error,
        };
        submitter.join().expect("submitter join");
        assert_eq!(error.kind(), VisionErrorKind::Cancelled);
        assert_eq!(error.message(), "native job was cancelled before admission");
        assert_eq!(invoked.load(Ordering::Acquire), 0);
        assert_eq!(lock_recover(&pool.resource.state).reserved, 1);
        wait_for_admission_waiters(&pool, 0);

        drop(reservation);
        assert_eq!(pool.execute(&CancellationToken::root(), || Ok(23)), Ok(23));
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn close_waits_for_reserved_builder_and_cancels_its_committed_job() {
        let (runtime, pool) = pool(1, 1);
        let cancellation = CancellationToken::root();
        let reservation = pool
            .resource
            .reserve(&cancellation)
            .expect("reserved admission");

        let closing = pool.clone();
        let (closed, observed_closed) = mpsc::channel();
        let closer =
            std::thread::spawn(move || closed.send(closing.close()).expect("close result"));
        wait_for_pool_lifecycle(&pool, PoolLifecycle::Closing);
        assert!(observed_closed.try_recv().is_err());

        let invoked = Arc::new(AtomicUsize::new(0));
        let observed_invoked = Arc::clone(&invoked);
        let (completion, job) = make_job(&cancellation, reservation.ticket, move || {
            observed_invoked.fetch_add(1, Ordering::AcqRel);
            Ok(())
        });
        let ticket = reservation.commit(job).expect("reservation commit");
        let waiter = pool.resource.waiter(&cancellation, ticket, completion);
        assert_eq!(
            waiter
                .wait()
                .expect_err("close cancels reserved queued job")
                .kind(),
            VisionErrorKind::PoolClosed
        );
        observed_closed
            .recv()
            .expect("close completion")
            .expect("pool close");
        closer.join().expect("closer join");
        assert_eq!(invoked.load(Ordering::Acquire), 0);
        assert_eq!(
            pool.counts(),
            NativePoolCounts {
                queued: 0,
                in_flight: 0,
                workers: 1,
                closed: true,
            }
        );
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn rejected_job_capture_drops_after_the_state_mutex_is_released() {
        let (runtime, pool) = pool(1, 1);
        pool.close().expect("pool close before rejection");
        let (dropped, observed_drop) = mpsc::channel();
        let capture = ReentrantCountDrop {
            pool: pool.clone(),
            observed: Some(dropped),
        };
        let error = match pool.resource.submit(&CancellationToken::root(), move || {
            drop(capture);
            Ok(())
        }) {
            Ok(_) => panic!("closed pool unexpectedly admitted a job"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), VisionErrorKind::PoolClosed);
        assert!(
            observed_drop
                .recv_timeout(Duration::from_secs(5))
                .expect("reentrant capture drop")
                .closed
        );
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn builder_panic_drops_reentrant_capture_and_releases_reservation() {
        let (runtime, pool) = pool(1, 1);
        let cancellation = CancellationToken::root();
        let (dropped, observed_drop) = mpsc::channel();
        let panic_result = catch_unwind(AssertUnwindSafe(|| {
            let _reservation = pool
                .resource
                .reserve(&cancellation)
                .expect("reserved admission");
            let _capture = ReentrantCountDrop {
                pool: pool.clone(),
                observed: Some(dropped),
            };
            panic!("scripted owned job builder panic");
        }));
        assert!(panic_result.is_err());
        assert_eq!(
            observed_drop
                .recv_timeout(Duration::from_secs(5))
                .expect("panic capture drop"),
            pool.counts()
        );
        assert_eq!(lock_recover(&pool.resource.state).reserved, 0);
        assert_eq!(pool.execute(&cancellation, || Ok(17)), Ok(17));
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
