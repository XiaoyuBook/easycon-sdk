use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use easycon_native_sys::ocr as native;
use easycon_runtime::{CancellationToken, ManagedResource, ResourceRegistration, Runtime};

use crate::{Image, NativePool, VisionError, VisionErrorKind, VisionLimits};

const MAX_OCR_ENGINES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OcrEngineMode {
    Default,
    LstmOnly,
}

impl OcrEngineMode {
    const fn into_native(self) -> native::EngineMode {
        match self {
            Self::Default => native::EngineMode::Default,
            Self::LstmOnly => native::EngineMode::LstmOnly,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OcrPageSegmentation {
    Auto,
    SingleBlock,
    SingleLine,
    SingleWord,
}

impl OcrPageSegmentation {
    const fn into_native(self) -> native::PageSegmentation {
        match self {
            Self::Auto => native::PageSegmentation::Auto,
            Self::SingleBlock => native::PageSegmentation::SingleBlock,
            Self::SingleLine => native::PageSegmentation::SingleLine,
            Self::SingleWord => native::PageSegmentation::SingleWord,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OcrConfig {
    model_root: PathBuf,
    language: String,
    engine_mode: OcrEngineMode,
    page_segmentation: OcrPageSegmentation,
    max_output_bytes: usize,
}

impl OcrConfig {
    pub fn new(
        model_root: &Path,
        language: &str,
        engine_mode: OcrEngineMode,
        page_segmentation: OcrPageSegmentation,
        max_output_bytes: usize,
    ) -> Result<Self, VisionError> {
        if max_output_bytes == 0 || max_output_bytes > native::MAX_OCR_OUTPUT_BYTES {
            return Err(VisionError::limit(
                "OCR output limit exceeds the hard ceiling",
            ));
        }
        validate_language(language)?;
        let model_root = std::fs::canonicalize(model_root).map_err(|_| {
            VisionError::new(
                VisionErrorKind::ModelNotFound,
                "OCR model root was not found",
            )
        })?;
        if !model_root.is_dir() || model_root.to_str().is_none() {
            return Err(VisionError::new(
                VisionErrorKind::ModelNotFound,
                "OCR model root is not a UTF-8 directory",
            ));
        }
        for name in language.split('+') {
            let model = model_root.join(format!("{name}.traineddata"));
            let canonical = std::fs::canonicalize(&model).map_err(|_| {
                VisionError::new(
                    VisionErrorKind::ModelNotFound,
                    format!("OCR model {name}.traineddata was not found"),
                )
            })?;
            if !canonical.starts_with(&model_root) || !canonical.is_file() {
                return Err(VisionError::new(
                    VisionErrorKind::ModelNotFound,
                    "OCR model is outside the explicit model root",
                ));
            }
        }
        Ok(Self {
            model_root: native_compatible_canonical_path(model_root),
            language: language.to_owned(),
            engine_mode,
            page_segmentation,
            max_output_bytes,
        })
    }

    #[must_use]
    pub fn model_root(&self) -> &Path {
        &self.model_root
    }

    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    #[must_use]
    pub const fn engine_mode(&self) -> OcrEngineMode {
        self.engine_mode
    }

    #[must_use]
    pub const fn page_segmentation(&self) -> OcrPageSegmentation {
        self.page_segmentation
    }

    #[must_use]
    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OcrOutput {
    text: String,
    confidence: f32,
}

impl OcrOutput {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn confidence(&self) -> f32 {
        self.confidence
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcrPoolCounts {
    pub open: bool,
    pub created: usize,
    pub creating: usize,
    pub borrowed: usize,
    pub idle: usize,
    pub waiters: usize,
}

#[derive(Clone)]
pub struct OcrPool {
    resource: Arc<OcrPoolResource>,
}

struct OcrPoolResource {
    config: OcrConfig,
    max_engines: usize,
    factory: Arc<dyn OcrEngineFactory>,
    commit_gate: Mutex<()>,
    state: Mutex<OcrState>,
    changed: Condvar,
    registration: Mutex<Option<ResourceRegistration>>,
    retention: Mutex<Option<Arc<OcrPoolResource>>>,
    #[cfg(test)]
    waiter_observer: Mutex<Option<std::sync::mpsc::Sender<u64>>>,
    #[cfg(test)]
    close_observer: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

struct OcrState {
    open: bool,
    closed: bool,
    close_result: Option<Result<(), VisionError>>,
    next_ticket: u64,
    created: usize,
    creating: usize,
    borrowed: usize,
    idle: Vec<Box<dyn PooledOcrEngine>>,
    waiters: VecDeque<u64>,
}

trait OcrEngineFactory: Send + Sync {
    fn create(&self, config: &OcrConfig) -> Result<Box<dyn PooledOcrEngine>, VisionError>;
}

trait PooledOcrEngine: Send {
    fn process(
        &mut self,
        image: &Image,
        limits: &VisionLimits,
        config: &OcrConfig,
    ) -> Result<OcrOutput, EngineFailure>;
}

struct EngineFailure {
    error: VisionError,
    poison: bool,
}

struct NativeOcrFactory;

struct NativeOcrEngine {
    engine: native::OcrEngine,
}

struct OcrLease {
    pool: Arc<OcrPoolResource>,
    engine: Option<Box<dyn PooledOcrEngine>>,
    poisoned: bool,
}

impl OcrPool {
    pub fn new(
        runtime: &Runtime,
        config: OcrConfig,
        max_engines: usize,
    ) -> Result<Self, VisionError> {
        Self::new_with_factory(runtime, config, max_engines, Arc::new(NativeOcrFactory))
    }

    fn new_with_factory(
        runtime: &Runtime,
        config: OcrConfig,
        max_engines: usize,
        factory: Arc<dyn OcrEngineFactory>,
    ) -> Result<Self, VisionError> {
        if max_engines == 0 {
            return Err(VisionError::validation(
                "OCR pool engine capacity must be non-zero",
            ));
        }
        if max_engines > MAX_OCR_ENGINES {
            return Err(VisionError::limit(
                "OCR pool engine capacity exceeds the hard ceiling",
            ));
        }
        let resource = Arc::new(OcrPoolResource {
            config,
            max_engines,
            factory,
            commit_gate: Mutex::new(()),
            state: Mutex::new(OcrState {
                open: true,
                closed: false,
                close_result: None,
                next_ticket: 1,
                created: 0,
                creating: 0,
                borrowed: 0,
                idle: Vec::new(),
                waiters: VecDeque::new(),
            }),
            changed: Condvar::new(),
            registration: Mutex::new(None),
            retention: Mutex::new(None),
            #[cfg(test)]
            waiter_observer: Mutex::new(None),
            #[cfg(test)]
            close_observer: Mutex::new(None),
        });
        let commit = lock_recover(&resource.commit_gate);
        let managed: Arc<dyn ManagedResource> = resource.clone();
        let registration = runtime
            .register_resource(managed)
            .map_err(VisionError::from_runtime)?;
        *lock_recover(&resource.registration) = Some(registration);
        *lock_recover(&resource.retention) = Some(Arc::clone(&resource));
        drop(commit);
        Ok(Self { resource })
    }

    pub fn recognize(
        &self,
        native_pool: &NativePool,
        image: &Image,
        limits: &VisionLimits,
        cancellation: &CancellationToken,
    ) -> Result<OcrOutput, VisionError> {
        let resource = Arc::clone(&self.resource);
        let image = image.clone();
        let limits = *limits;
        let worker_cancellation = cancellation.clone();
        native_pool.execute(cancellation, move || {
            let mut lease = resource.acquire(&worker_cancellation)?;
            match lease.process(&image, &limits) {
                Ok(output) => Ok(output),
                Err(failure) => {
                    if failure.poison {
                        lease.poison();
                    }
                    Err(failure.error)
                }
            }
        })
    }

    pub fn close(&self) -> Result<(), VisionError> {
        self.resource.close_inner()
    }

    #[must_use]
    pub fn counts(&self) -> OcrPoolCounts {
        self.resource.counts()
    }
}

impl OcrPoolResource {
    fn counts(&self) -> OcrPoolCounts {
        let state = lock_recover(&self.state);
        OcrPoolCounts {
            open: state.open,
            created: state.created,
            creating: state.creating,
            borrowed: state.borrowed,
            idle: state.idle.len(),
            waiters: state.waiters.len(),
        }
    }

    fn acquire(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
    ) -> Result<OcrLease, VisionError> {
        if cancellation.is_cancelled() {
            return Err(VisionError::cancelled(
                "OCR engine acquire was cancelled before admission",
            ));
        }
        let ticket = {
            let mut state = lock_recover(&self.state);
            if !state.open {
                return Err(VisionError::pool_closed("OCR pool is closed"));
            }
            let ticket = state.next_ticket;
            state.next_ticket = state
                .next_ticket
                .checked_add(1)
                .ok_or_else(|| VisionError::internal("OCR ticket space exhausted"))?;
            state.waiters.push_back(ticket);
            ticket
        };
        #[cfg(test)]
        if let Some(observer) = lock_recover(&self.waiter_observer).as_ref() {
            let _ = observer.send(ticket);
        }
        let weak = Arc::downgrade(self);
        let _hook = cancellation.on_cancel_scoped(move || {
            if let Some(pool) = weak.upgrade() {
                let _state = lock_recover(&pool.state);
                pool.changed.notify_all();
            }
        });

        loop {
            let mut state = lock_recover(&self.state);
            if !state.open {
                remove_waiter(&mut state.waiters, ticket);
                self.changed.notify_all();
                return Err(VisionError::pool_closed(
                    "OCR engine acquire was interrupted by close",
                ));
            }
            if cancellation.is_cancelled() {
                remove_waiter(&mut state.waiters, ticket);
                self.changed.notify_all();
                return Err(VisionError::cancelled("OCR engine acquire was cancelled"));
            }
            if state.waiters.front().copied() != Some(ticket) {
                drop(wait_recover(&self.changed, state));
                continue;
            }
            if let Some(engine) = state.idle.pop() {
                state.waiters.pop_front();
                state.borrowed += 1;
                self.changed.notify_all();
                return Ok(OcrLease {
                    pool: Arc::clone(self),
                    engine: Some(engine),
                    poisoned: false,
                });
            }
            if state.created + state.creating < self.max_engines {
                state.waiters.pop_front();
                state.creating += 1;
                self.changed.notify_all();
                drop(state);
                let created = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.factory.create(&self.config)
                })) {
                    Ok(result) => result,
                    Err(payload) => {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            drop(payload);
                        }));
                        Err(VisionError::internal("OCR engine factory panicked"))
                    }
                };
                let mut state = lock_recover(&self.state);
                debug_assert!(state.creating > 0, "OCR creating count underflow");
                state.creating -= 1;
                self.changed.notify_all();
                let engine = created?;
                if !state.open {
                    drop(state);
                    drop(engine);
                    return Err(VisionError::pool_closed(
                        "OCR engine creation completed after close",
                    ));
                }
                if cancellation.is_cancelled() {
                    drop(state);
                    drop(engine);
                    return Err(VisionError::cancelled(
                        "OCR engine creation completed after cancellation",
                    ));
                }
                state.created += 1;
                state.borrowed += 1;
                return Ok(OcrLease {
                    pool: Arc::clone(self),
                    engine: Some(engine),
                    poisoned: false,
                });
            }
            drop(wait_recover(&self.changed, state));
        }
    }

    fn close_inner(&self) -> Result<(), VisionError> {
        let _commit = lock_recover(&self.commit_gate);
        let idle = {
            let mut state = lock_recover(&self.state);
            if state.closed {
                return state.close_result.clone().unwrap_or_else(|| {
                    Err(VisionError::internal("closed OCR pool has no saved result"))
                });
            }
            if state.open {
                state.open = false;
                #[cfg(test)]
                if let Some(observer) = lock_recover(&self.close_observer).as_ref() {
                    let _ = observer.send(());
                }
                self.changed.notify_all();
            }
            while state.creating != 0 || state.borrowed != 0 {
                state = wait_recover(&self.changed, state);
            }
            let idle = std::mem::take(&mut state.idle);
            if idle.len() > state.created {
                state.close_result = Some(Err(VisionError::internal(
                    "OCR idle count exceeds created engines",
                )));
                state.created = 0;
            } else {
                state.created -= idle.len();
            }
            idle
        };
        drop(idle);

        if let Some(registration) = lock_recover(&self.registration).take() {
            registration.unregister();
        }
        lock_recover(&self.retention).take();
        let mut state = lock_recover(&self.state);
        let result = if let Some(error) = state.close_result.clone() {
            error
        } else if state.created == 0 && state.creating == 0 && state.borrowed == 0 {
            Ok(())
        } else {
            Err(VisionError::internal(
                "OCR pool ownership did not converge during close",
            ))
        };
        state.close_result = Some(result.clone());
        state.closed = true;
        self.changed.notify_all();
        result
    }
}

impl ManagedResource for OcrPoolResource {
    fn close(&self) {
        let _ = self.close_inner();
    }
}

impl OcrLease {
    fn process(
        &mut self,
        image: &Image,
        limits: &VisionLimits,
    ) -> Result<OcrOutput, EngineFailure> {
        self.engine
            .as_mut()
            .expect("live OCR lease owns one engine")
            .process(image, limits, &self.pool.config)
    }

    fn poison(&mut self) {
        self.poisoned = true;
    }
}

impl Drop for OcrLease {
    fn drop(&mut self) {
        let Some(engine) = self.engine.take() else {
            return;
        };
        let mut state = lock_recover(&self.pool.state);
        debug_assert!(state.borrowed > 0, "OCR borrowed count underflow");
        state.borrowed -= 1;
        if self.poisoned || !state.open {
            debug_assert!(state.created > 0, "OCR created count underflow");
            state.created -= 1;
            self.pool.changed.notify_all();
            drop(state);
            drop(engine);
        } else {
            state.idle.push(engine);
            self.pool.changed.notify_all();
        }
    }
}

impl OcrEngineFactory for NativeOcrFactory {
    fn create(&self, config: &OcrConfig) -> Result<Box<dyn PooledOcrEngine>, VisionError> {
        native::OcrEngine::create(
            config.model_root(),
            config.language(),
            config.engine_mode().into_native(),
        )
        .map(|engine| Box::new(NativeOcrEngine { engine }) as Box<dyn PooledOcrEngine>)
        .map_err(VisionError::from_native)
    }
}

impl PooledOcrEngine for NativeOcrEngine {
    fn process(
        &mut self,
        image: &Image,
        limits: &VisionLimits,
        config: &OcrConfig,
    ) -> Result<OcrOutput, EngineFailure> {
        let view = image.native_view(limits).map_err(|error| EngineFailure {
            error: VisionError::from(error),
            poison: false,
        })?;
        let output = self
            .engine
            .process(
                view,
                config.page_segmentation().into_native(),
                config.max_output_bytes(),
            )
            .map_err(|failure| {
                let poison = failure.poisons_engine();
                EngineFailure {
                    error: VisionError::from_native(failure.into_error()),
                    poison,
                }
            })?;
        if !output.confidence.is_finite() || !(0.0..=1.0).contains(&output.confidence) {
            return Err(EngineFailure {
                error: VisionError::internal("OCR confidence is invalid"),
                poison: true,
            });
        }
        let confidence = output.confidence as f32;
        if !confidence.is_finite() {
            return Err(EngineFailure {
                error: VisionError::internal("OCR confidence does not fit f32"),
                poison: true,
            });
        }
        Ok(OcrOutput {
            text: output.text,
            confidence,
        })
    }
}

fn validate_language(language: &str) -> Result<(), VisionError> {
    if language.is_empty() || language.len() > 128 {
        return Err(VisionError::validation("OCR language length is invalid"));
    }
    for name in language.split('+') {
        if name.is_empty()
            || !name
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'_' || value == b'-')
        {
            return Err(VisionError::validation("OCR language is invalid"));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn native_compatible_canonical_path(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    text.strip_prefix(r"\\?\")
        .map_or(path.clone(), PathBuf::from)
}

#[cfg(not(windows))]
fn native_compatible_canonical_path(path: PathBuf) -> PathBuf {
    path
}

fn remove_waiter(waiters: &mut VecDeque<u64>, ticket: u64) {
    if let Some(index) = waiters.iter().position(|candidate| *candidate == ticket) {
        waiters.remove(index);
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

    use super::*;

    struct FakeFactory {
        created: Arc<AtomicUsize>,
        destroyed: Arc<AtomicUsize>,
    }

    struct FakeEngine {
        destroyed: Arc<AtomicUsize>,
    }

    impl Drop for FakeEngine {
        fn drop(&mut self) {
            self.destroyed.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl PooledOcrEngine for FakeEngine {
        fn process(
            &mut self,
            _image: &Image,
            _limits: &VisionLimits,
            _config: &OcrConfig,
        ) -> Result<OcrOutput, EngineFailure> {
            Ok(OcrOutput {
                text: "synthetic".to_owned(),
                confidence: 1.0,
            })
        }
    }

    impl OcrEngineFactory for FakeFactory {
        fn create(&self, _config: &OcrConfig) -> Result<Box<dyn PooledOcrEngine>, VisionError> {
            self.created.fetch_add(1, Ordering::AcqRel);
            Ok(Box::new(FakeEngine {
                destroyed: Arc::clone(&self.destroyed),
            }))
        }
    }

    struct BlockingFactory {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        destroyed: Arc<AtomicUsize>,
    }

    struct ProcessFailingFactory {
        destroyed: Arc<AtomicUsize>,
    }

    struct ProcessFailingEngine {
        destroyed: Arc<AtomicUsize>,
    }

    impl Drop for ProcessFailingEngine {
        fn drop(&mut self) {
            self.destroyed.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl PooledOcrEngine for ProcessFailingEngine {
        fn process(
            &mut self,
            _image: &Image,
            _limits: &VisionLimits,
            _config: &OcrConfig,
        ) -> Result<OcrOutput, EngineFailure> {
            Err(EngineFailure {
                error: VisionError::new(VisionErrorKind::Native, "scripted native OCR exception"),
                poison: true,
            })
        }
    }

    impl OcrEngineFactory for ProcessFailingFactory {
        fn create(&self, _config: &OcrConfig) -> Result<Box<dyn PooledOcrEngine>, VisionError> {
            Ok(Box::new(ProcessFailingEngine {
                destroyed: Arc::clone(&self.destroyed),
            }))
        }
    }

    struct CreateFailingFactory;

    impl OcrEngineFactory for CreateFailingFactory {
        fn create(&self, _config: &OcrConfig) -> Result<Box<dyn PooledOcrEngine>, VisionError> {
            Err(VisionError::new(
                VisionErrorKind::ModelNotFound,
                "scripted missing model",
            ))
        }
    }

    impl OcrEngineFactory for BlockingFactory {
        fn create(&self, _config: &OcrConfig) -> Result<Box<dyn PooledOcrEngine>, VisionError> {
            self.entered.send(()).expect("create entered");
            lock_recover(&self.release).recv().expect("create release");
            Ok(Box::new(FakeEngine {
                destroyed: Arc::clone(&self.destroyed),
            }))
        }
    }

    fn config() -> OcrConfig {
        OcrConfig {
            model_root: PathBuf::from("Z:/explicit-test-model-root"),
            language: "eng".to_owned(),
            engine_mode: OcrEngineMode::Default,
            page_segmentation: OcrPageSegmentation::SingleLine,
            max_output_bytes: 4096,
        }
    }

    fn fake_pool(max_engines: usize) -> (Runtime, OcrPool, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let created = Arc::new(AtomicUsize::new(0));
        let destroyed = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(FakeFactory {
            created: Arc::clone(&created),
            destroyed: Arc::clone(&destroyed),
        });
        let pool = OcrPool::new_with_factory(&runtime, config(), max_engines, factory)
            .expect("fake OCR pool");
        (runtime, pool, created, destroyed)
    }

    #[test]
    fn waiters_acquire_one_engine_in_fifo_ticket_order() {
        let (runtime, pool, created, destroyed) = fake_pool(1);
        let first = pool
            .resource
            .acquire(&CancellationToken::root())
            .expect("initial lease");
        let (waiter, observed_waiter) = mpsc::channel();
        *lock_recover(&pool.resource.waiter_observer) = Some(waiter);
        let (acquired, observed_acquired) = mpsc::channel();

        let first_pool = Arc::clone(&pool.resource);
        let first_acquired = acquired.clone();
        let first_waiter = std::thread::spawn(move || {
            let lease = first_pool
                .acquire(&CancellationToken::root())
                .expect("first waiter");
            first_acquired.send(1).expect("first acquired");
            drop(lease);
        });
        observed_waiter.recv().expect("first ticket");
        let second_pool = Arc::clone(&pool.resource);
        let second_waiter = std::thread::spawn(move || {
            let lease = second_pool
                .acquire(&CancellationToken::root())
                .expect("second waiter");
            acquired.send(2).expect("second acquired");
            drop(lease);
        });
        observed_waiter.recv().expect("second ticket");
        drop(first);
        assert_eq!(observed_acquired.recv().expect("first order"), 1);
        assert_eq!(observed_acquired.recv().expect("second order"), 2);
        first_waiter.join().expect("first waiter join");
        second_waiter.join().expect("second waiter join");
        assert_eq!(created.load(Ordering::Acquire), 1);
        pool.close().expect("pool close");
        assert_eq!(destroyed.load(Ordering::Acquire), 1);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn cancelled_waiter_is_removed_without_consuming_the_engine() {
        let (runtime, pool, created, destroyed) = fake_pool(1);
        let lease = pool
            .resource
            .acquire(&CancellationToken::root())
            .expect("initial lease");
        let (waiter, observed_waiter) = mpsc::channel();
        *lock_recover(&pool.resource.waiter_observer) = Some(waiter);
        let cancellation = CancellationToken::root();
        let waiter_token = cancellation.clone();
        let waiting_pool = Arc::clone(&pool.resource);
        let (result, observed_result) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            result
                .send(waiting_pool.acquire(&waiter_token).map(drop))
                .expect("acquire result");
        });
        observed_waiter.recv().expect("waiter admitted");
        cancellation.cancel();
        assert_eq!(
            observed_result
                .recv()
                .expect("cancel result")
                .expect_err("cancelled acquire")
                .kind(),
            VisionErrorKind::Cancelled
        );
        thread.join().expect("waiter join");
        drop(lease);
        assert_eq!(created.load(Ordering::Acquire), 1);
        pool.close().expect("pool close");
        assert_eq!(destroyed.load(Ordering::Acquire), 1);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn poisoned_engine_is_destroyed_instead_of_returned_idle() {
        let (runtime, pool, created, destroyed) = fake_pool(1);
        let mut lease = pool
            .resource
            .acquire(&CancellationToken::root())
            .expect("initial lease");
        lease.poison();
        drop(lease);
        assert_eq!(pool.counts().created, 0);
        assert_eq!(destroyed.load(Ordering::Acquire), 1);
        drop(
            pool.resource
                .acquire(&CancellationToken::root())
                .expect("replacement lease"),
        );
        assert_eq!(created.load(Ordering::Acquire), 2);
        pool.close().expect("pool close");
        assert_eq!(destroyed.load(Ordering::Acquire), 2);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn failed_create_rolls_back_capacity_for_the_next_fifo_ticket() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let pool = OcrPool::new_with_factory(&runtime, config(), 1, Arc::new(CreateFailingFactory))
            .expect("failing pool");
        let error = match pool.resource.acquire(&CancellationToken::root()) {
            Ok(_) => panic!("scripted create unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), VisionErrorKind::ModelNotFound);
        assert_eq!(pool.counts().creating, 0);
        assert_eq!(pool.counts().created, 0);
        pool.close().expect("pool close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn poisoned_process_failure_discards_the_lease_after_native_return() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let native_pool = NativePool::new(
            &runtime,
            crate::NativePoolOptions::new(1, 2).expect("native options"),
        )
        .expect("native pool");
        let destroyed = Arc::new(AtomicUsize::new(0));
        let pool = OcrPool::new_with_factory(
            &runtime,
            config(),
            1,
            Arc::new(ProcessFailingFactory {
                destroyed: Arc::clone(&destroyed),
            }),
        )
        .expect("failing OCR pool");
        let limits = VisionLimits::default();
        let image = Image::new(
            Arc::<[u8]>::from([255_u8]),
            1,
            1,
            1,
            crate::PixelFormat::Gray8,
            &limits,
        )
        .expect("test image");
        let error = pool
            .recognize(&native_pool, &image, &limits, &CancellationToken::root())
            .expect_err("scripted process failure");
        assert_eq!(error.kind(), VisionErrorKind::Native);
        assert_eq!(pool.counts().created, 0);
        assert_eq!(destroyed.load(Ordering::Acquire), 1);
        native_pool.close().expect("native close");
        pool.close().expect("OCR close");
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn close_waits_for_borrowed_engine_and_is_idempotent() {
        let (runtime, pool, _created, destroyed) = fake_pool(1);
        let lease = pool
            .resource
            .acquire(&CancellationToken::root())
            .expect("borrowed lease");
        let closing = pool.clone();
        let (close_started, observed_close_started) = mpsc::channel();
        *lock_recover(&pool.resource.close_observer) = Some(close_started);
        let (closed, observed_closed) = mpsc::channel();
        let closer =
            std::thread::spawn(move || closed.send(closing.close()).expect("close result"));
        observed_close_started.recv().expect("close started");
        assert!(observed_closed.try_recv().is_err());
        drop(lease);
        observed_closed
            .recv()
            .expect("close completion")
            .expect("close");
        closer.join().expect("closer join");
        assert_eq!(destroyed.load(Ordering::Acquire), 1);
        assert_eq!(pool.close(), Ok(()));
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn close_wakes_fifo_waiters_before_waiting_for_the_borrowed_engine() {
        let (runtime, pool, _created, destroyed) = fake_pool(1);
        let lease = pool
            .resource
            .acquire(&CancellationToken::root())
            .expect("borrowed lease");
        let (waiter, observed_waiter) = mpsc::channel();
        *lock_recover(&pool.resource.waiter_observer) = Some(waiter);
        let waiting_pool = Arc::clone(&pool.resource);
        let (result, observed_result) = mpsc::channel();
        let waiter_thread = std::thread::spawn(move || {
            result
                .send(waiting_pool.acquire(&CancellationToken::root()).map(drop))
                .expect("waiter result");
        });
        observed_waiter.recv().expect("waiter admitted");

        let (close_started, observed_close_started) = mpsc::channel();
        *lock_recover(&pool.resource.close_observer) = Some(close_started);
        let closing = pool.clone();
        let (closed, observed_closed) = mpsc::channel();
        let closer =
            std::thread::spawn(move || closed.send(closing.close()).expect("close result"));
        observed_close_started.recv().expect("close started");
        assert_eq!(
            observed_result
                .recv()
                .expect("waiter wake")
                .expect_err("close rejects waiter")
                .kind(),
            VisionErrorKind::PoolClosed
        );
        assert!(observed_closed.try_recv().is_err());
        drop(lease);
        observed_closed
            .recv()
            .expect("close completion")
            .expect("close");
        waiter_thread.join().expect("waiter join");
        closer.join().expect("closer join");
        assert_eq!(destroyed.load(Ordering::Acquire), 1);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn close_waits_for_creation_outside_the_state_lock() {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let (entered, observed_entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let destroyed = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(BlockingFactory {
            entered,
            release: Mutex::new(released),
            destroyed: Arc::clone(&destroyed),
        });
        let pool =
            OcrPool::new_with_factory(&runtime, config(), 1, factory).expect("blocking pool");
        let acquiring = Arc::clone(&pool.resource);
        let (acquired, observed_acquired) = mpsc::channel();
        let acquire = std::thread::spawn(move || {
            acquired
                .send(acquiring.acquire(&CancellationToken::root()).map(drop))
                .expect("acquire result");
        });
        observed_entered.recv().expect("factory entered");
        let closing = pool.clone();
        let (close_started, observed_close_started) = mpsc::channel();
        *lock_recover(&pool.resource.close_observer) = Some(close_started);
        let (closed, observed_closed) = mpsc::channel();
        let closer =
            std::thread::spawn(move || closed.send(closing.close()).expect("close result"));
        observed_close_started.recv().expect("close started");
        assert!(observed_closed.try_recv().is_err());
        release.send(()).expect("release factory");
        assert_eq!(
            observed_acquired
                .recv()
                .expect("acquire result")
                .expect_err("create after close")
                .kind(),
            VisionErrorKind::PoolClosed
        );
        acquire.join().expect("acquire join");
        observed_closed
            .recv()
            .expect("close completion")
            .expect("close");
        closer.join().expect("close join");
        assert_eq!(destroyed.load(Ordering::Acquire), 1);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }
}
