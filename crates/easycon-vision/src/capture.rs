use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use easycon_model::{EasyConError, ErrorCode, ErrorDomain};
use easycon_native_sys::capture as native;
use easycon_runtime::{
    CancellationHookRegistration, CancellationReason, CancellationToken, Clock, ManagedResource,
    Operation, OperationState, OperationValue, ResourceRegistration, Runtime, SupervisedTask,
    SupervisedTaskOutcome, TransitionOutcome,
};

use crate::{Frame, Image, PixelFormat, VisionError, VisionErrorKind, VisionLimits};

const SYNTHETIC_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CAPTURE_SOURCE_BYTES: usize = 4096;
const MAX_CAPTURE_NAME_BYTES: usize = 1024;
const MAX_FIRST_FRAME_TIMEOUT_NS: u64 = 300_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureBackendKind {
    Synthetic,
    File,
    DirectShow,
    MediaFoundation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureProfile {
    source_id: Arc<str>,
    display_name: Arc<str>,
    backend: CaptureBackendKind,
    width: u32,
    height: u32,
    stride: usize,
    pixel_format: PixelFormat,
    frame_interval_ns: Option<u64>,
}

impl CaptureProfile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_id: impl Into<Arc<str>>,
        display_name: impl Into<Arc<str>>,
        backend: CaptureBackendKind,
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
        frame_interval_ns: Option<u64>,
    ) -> Result<Self, VisionError> {
        let stride = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(pixel_format.channels()))
            .ok_or_else(|| VisionError::limit("capture profile row bytes overflow"))?;
        Self::new_with_stride(
            source_id,
            display_name,
            backend,
            width,
            height,
            stride,
            pixel_format,
            frame_interval_ns,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_stride(
        source_id: impl Into<Arc<str>>,
        display_name: impl Into<Arc<str>>,
        backend: CaptureBackendKind,
        width: u32,
        height: u32,
        stride: usize,
        pixel_format: PixelFormat,
        frame_interval_ns: Option<u64>,
    ) -> Result<Self, VisionError> {
        let source_id = source_id.into();
        let display_name = display_name.into();
        if source_id.is_empty()
            || source_id.len() > MAX_CAPTURE_SOURCE_BYTES
            || source_id.contains('\0')
        {
            return Err(VisionError::validation(
                "capture source ID must be non-empty, bounded UTF-8 without NUL",
            ));
        }
        if display_name.is_empty()
            || display_name.len() > MAX_CAPTURE_NAME_BYTES
            || display_name.contains('\0')
        {
            return Err(VisionError::validation(
                "capture display name must be non-empty, bounded UTF-8 without NUL",
            ));
        }
        if width == 0 || height == 0 {
            return Err(VisionError::validation(
                "capture profile dimensions must be non-zero",
            ));
        }
        let row_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(pixel_format.channels()))
            .ok_or_else(|| VisionError::limit("capture profile row bytes overflow"))?;
        if stride < row_bytes {
            return Err(VisionError::validation(
                "capture profile stride is shorter than one pixel row",
            ));
        }
        if frame_interval_ns == Some(0) {
            return Err(VisionError::validation(
                "capture frame interval must be non-zero when present",
            ));
        }
        Ok(Self {
            source_id,
            display_name,
            backend,
            width,
            height,
            stride,
            pixel_format,
            frame_interval_ns,
        })
    }

    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub const fn backend(&self) -> CaptureBackendKind {
        self.backend
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    #[must_use]
    pub const fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    #[must_use]
    pub const fn frame_interval_ns(&self) -> Option<u64> {
        self.frame_interval_ns
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureOptions {
    first_frame_timeout_ns: u64,
    limits: VisionLimits,
}

impl CaptureOptions {
    pub fn new(first_frame_timeout_ns: u64, limits: VisionLimits) -> Result<Self, VisionError> {
        if first_frame_timeout_ns == 0 || first_frame_timeout_ns > MAX_FIRST_FRAME_TIMEOUT_NS {
            return Err(VisionError::limit(format!(
                "first-frame timeout must be within 1..={MAX_FIRST_FRAME_TIMEOUT_NS} ns"
            )));
        }
        Ok(Self {
            first_frame_timeout_ns,
            limits,
        })
    }

    #[must_use]
    pub const fn first_frame_timeout_ns(self) -> u64 {
        self.first_frame_timeout_ns
    }

    #[must_use]
    pub const fn limits(self) -> VisionLimits {
        self.limits
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeCaptureOptions {
    open_timeout_ns: u64,
    read_timeout_ns: u64,
    max_frames: u32,
}

impl NativeCaptureOptions {
    pub fn new(
        open_timeout_ns: u64,
        read_timeout_ns: u64,
        max_frames: u32,
    ) -> Result<Self, VisionError> {
        if open_timeout_ns == 0
            || open_timeout_ns > native::MAX_CAPTURE_TIMEOUT_NS
            || read_timeout_ns == 0
            || read_timeout_ns > native::MAX_CAPTURE_TIMEOUT_NS
            || max_frames == 0
            || max_frames > native::MAX_CAPTURE_FRAMES
        {
            return Err(VisionError::limit(
                "native capture options exceed fixed bounds",
            ));
        }
        Ok(Self {
            open_timeout_ns,
            read_timeout_ns,
            max_frames,
        })
    }

    fn into_native(self, limits: VisionLimits) -> Result<native::CaptureOptions, VisionError> {
        native::CaptureOptions::new(
            limits.native_limits(),
            self.open_timeout_ns,
            self.read_timeout_ns,
            self.max_frames,
        )
        .map_err(VisionError::from_native)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureSourceDescriptor {
    source_id: Arc<str>,
    display_name: Arc<str>,
    backend: CaptureBackendKind,
}

impl CaptureSourceDescriptor {
    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub const fn backend(&self) -> CaptureBackendKind {
        self.backend
    }
}

pub fn discover_capture_sources(
    backend: CaptureBackendKind,
) -> Result<Vec<CaptureSourceDescriptor>, VisionError> {
    let native_backend = native_backend(backend)?;
    if native_backend == native::CaptureBackend::File {
        return Err(VisionError::validation(
            "file capture does not support device discovery",
        ));
    }
    native::discover(native_backend)
        .map_err(VisionError::from_native)?
        .into_iter()
        .map(|descriptor| {
            let backend = vision_backend(descriptor.backend());
            validate_capture_identity(descriptor.source_id(), descriptor.display_name())?;
            Ok(CaptureSourceDescriptor {
                source_id: Arc::from(descriptor.source_id()),
                display_name: Arc::from(descriptor.display_name()),
                backend,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureState {
    Opening,
    Streaming,
    Faulted,
    Stopping,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureSnapshotWait {
    Poll,
    Until(u64),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SyntheticCaptureCounts {
    pub open_calls: usize,
    pub read_calls: usize,
    pub close_calls: usize,
    pub interrupt_calls: usize,
    pub finalize_calls: usize,
}

pub struct SyntheticCapture {
    shared: Arc<SyntheticShared>,
    profile: CaptureProfile,
}

#[derive(Clone)]
pub struct SyntheticCaptureControl {
    shared: Arc<SyntheticShared>,
}

struct SyntheticShared {
    state: Mutex<SyntheticState>,
    changed: Condvar,
}

struct SyntheticState {
    block_open: bool,
    open_released: bool,
    stop_requested: bool,
    open_failure: Option<Arc<str>>,
    close_failure: Option<Arc<str>>,
    interrupt_failure: Option<Arc<str>>,
    finalize_failures: usize,
    panic_read: bool,
    events: VecDeque<SyntheticEvent>,
    counts: SyntheticCaptureCounts,
}

enum SyntheticEvent {
    Frame(Image),
    Fault(Arc<str>),
    End,
}

struct SyntheticInterrupt {
    shared: Arc<SyntheticShared>,
}

struct NativeCaptureBackend {
    handle: Option<native::CaptureHandle>,
    interrupt: Option<Arc<NativeCaptureInterrupt>>,
    source_id: Arc<str>,
    display_name: Arc<str>,
    backend: CaptureBackendKind,
    limits: VisionLimits,
}

struct NativeCaptureInterrupt {
    inner: native::CaptureInterrupt,
}

impl NativeCaptureBackend {
    fn create(
        backend: CaptureBackendKind,
        source_id: Arc<str>,
        display_name: Arc<str>,
        session_options: CaptureOptions,
        native_options: NativeCaptureOptions,
    ) -> Result<Self, VisionError> {
        validate_capture_identity(&source_id, &display_name)?;
        let (handle, interrupt) = native::CaptureHandle::create(
            native_backend(backend)?,
            &source_id,
            native_options.into_native(session_options.limits())?,
        )
        .map_err(VisionError::from_native)?;
        Ok(Self {
            handle: Some(handle),
            interrupt: Some(Arc::new(NativeCaptureInterrupt { inner: interrupt })),
            source_id,
            display_name,
            backend,
            limits: session_options.limits(),
        })
    }

    fn handle_mut(&mut self) -> Result<&mut native::CaptureHandle, VisionError> {
        self.handle
            .as_mut()
            .ok_or_else(|| VisionError::internal("native capture owner is not armed"))
    }
}

impl SyntheticCapture {
    #[must_use]
    pub fn controlled(
        profile: CaptureProfile,
        block_open: bool,
    ) -> (Self, SyntheticCaptureControl) {
        let shared = Arc::new(SyntheticShared {
            state: Mutex::new(SyntheticState {
                block_open,
                open_released: !block_open,
                stop_requested: false,
                open_failure: None,
                close_failure: None,
                interrupt_failure: None,
                finalize_failures: 0,
                panic_read: false,
                events: VecDeque::new(),
                counts: SyntheticCaptureCounts::default(),
            }),
            changed: Condvar::new(),
        });
        (
            Self {
                shared: Arc::clone(&shared),
                profile,
            },
            SyntheticCaptureControl { shared },
        )
    }
}

impl SyntheticCaptureControl {
    pub fn release_open(&self) {
        let mut state = lock_recover(&self.shared.state);
        state.open_released = true;
        self.shared.changed.notify_all();
    }

    pub fn fail_open(&self, message: impl Into<Arc<str>>) -> Result<(), VisionError> {
        let mut state = lock_recover(&self.shared.state);
        if state.counts.open_calls != 0 {
            return Err(VisionError::validation(
                "synthetic open failure must be configured before open",
            ));
        }
        state.open_failure = Some(message.into());
        Ok(())
    }

    pub fn fail_close(&self, message: impl Into<Arc<str>>) {
        lock_recover(&self.shared.state).close_failure = Some(message.into());
    }

    pub fn fail_interrupt(&self, message: impl Into<Arc<str>>) {
        lock_recover(&self.shared.state).interrupt_failure = Some(message.into());
    }

    pub fn fail_finalize(&self, attempts: usize) {
        lock_recover(&self.shared.state).finalize_failures = attempts;
    }

    pub fn push_frame(&self, image: Image) -> Result<(), VisionError> {
        self.push_event(SyntheticEvent::Frame(image))
    }

    pub fn fail_read(&self, message: impl Into<Arc<str>>) -> Result<(), VisionError> {
        self.push_event(SyntheticEvent::Fault(message.into()))
    }

    pub fn panic_read(&self) -> Result<(), VisionError> {
        let mut state = lock_recover(&self.shared.state);
        if state.stop_requested {
            return Err(VisionError::new(
                VisionErrorKind::Closed,
                "synthetic capture is stopping",
            ));
        }
        state.panic_read = true;
        self.shared.changed.notify_all();
        Ok(())
    }

    pub fn end(&self) -> Result<(), VisionError> {
        self.push_event(SyntheticEvent::End)
    }

    fn push_event(&self, event: SyntheticEvent) -> Result<(), VisionError> {
        let mut state = lock_recover(&self.shared.state);
        if state.stop_requested {
            return Err(VisionError::new(
                VisionErrorKind::Closed,
                "synthetic capture is stopping",
            ));
        }
        state.events.push_back(event);
        self.shared.changed.notify_all();
        Ok(())
    }

    pub fn wait_for_open_calls(&self, target: usize) -> Result<(), VisionError> {
        self.wait_for("open calls", |state| state.counts.open_calls >= target)
    }

    pub fn wait_for_read_calls(&self, target: usize) -> Result<(), VisionError> {
        self.wait_for("read calls", |state| state.counts.read_calls >= target)
    }

    pub fn wait_for_close_calls(&self, target: usize) -> Result<(), VisionError> {
        self.wait_for("close calls", |state| state.counts.close_calls >= target)
    }

    pub fn wait_for_interrupt_calls(&self, target: usize) -> Result<(), VisionError> {
        self.wait_for("interrupt calls", |state| {
            state.counts.interrupt_calls >= target
        })
    }

    fn wait_for(
        &self,
        label: &str,
        predicate: impl Fn(&SyntheticState) -> bool,
    ) -> Result<(), VisionError> {
        let state = lock_recover(&self.shared.state);
        let (state, timeout) = self
            .shared
            .changed
            .wait_timeout_while(state, SYNTHETIC_WAIT_TIMEOUT, |state| !predicate(state))
            .unwrap_or_else(|poison| poison.into_inner());
        if timeout.timed_out() && !predicate(&state) {
            return Err(VisionError::internal(format!(
                "timed out waiting for synthetic {label}"
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn counts(&self) -> SyntheticCaptureCounts {
        lock_recover(&self.shared.state).counts
    }
}

trait CaptureInterrupt: Send + Sync + 'static {
    fn request_stop(&self) -> Result<(), VisionError>;
}

trait CaptureBackend: Send + 'static {
    fn open(&mut self, cancellation: &CancellationToken) -> Result<CaptureProfile, VisionError>;
    fn read(&mut self, cancellation: &CancellationToken) -> Result<CaptureRead, VisionError>;
    fn close(&mut self) -> Result<(), VisionError>;
    fn interrupt(&self) -> Arc<dyn CaptureInterrupt>;
    fn finalize(self: Box<Self>) -> BackendFinalize;
}

enum CaptureRead {
    Frame(Image),
    End,
}

enum BackendFinalize {
    Consumed {
        diagnostic: Option<VisionError>,
    },
    Unconsumed {
        backend: Box<dyn CaptureBackend>,
        diagnostic: VisionError,
    },
}

impl CaptureInterrupt for SyntheticInterrupt {
    fn request_stop(&self) -> Result<(), VisionError> {
        let mut state = lock_recover(&self.shared.state);
        state.counts.interrupt_calls += 1;
        state.stop_requested = true;
        self.shared.changed.notify_all();
        match state.interrupt_failure.take() {
            Some(message) => Err(VisionError::new(
                VisionErrorKind::Native,
                message.to_string(),
            )),
            None => Ok(()),
        }
    }
}

impl CaptureInterrupt for NativeCaptureInterrupt {
    fn request_stop(&self) -> Result<(), VisionError> {
        self.inner.request_stop().map_err(VisionError::from_native)
    }
}

impl CaptureBackend for NativeCaptureBackend {
    fn open(&mut self, cancellation: &CancellationToken) -> Result<CaptureProfile, VisionError> {
        if cancellation.is_cancelled() {
            return Err(VisionError::cancelled("native capture open was cancelled"));
        }
        let profile = self
            .handle_mut()?
            .open()
            .map_err(VisionError::from_native)?;
        if vision_backend(profile.backend()) != self.backend {
            return Err(VisionError::internal(
                "native capture opened a different backend than requested",
            ));
        }
        CaptureProfile::new_with_stride(
            Arc::clone(&self.source_id),
            Arc::clone(&self.display_name),
            self.backend,
            profile.width(),
            profile.height(),
            profile.stride(),
            vision_pixel_format(profile.pixel_format()),
            profile.frame_interval_ns(),
        )
    }

    fn read(&mut self, cancellation: &CancellationToken) -> Result<CaptureRead, VisionError> {
        if cancellation.is_cancelled() {
            return Err(VisionError::cancelled("native capture read was cancelled"));
        }
        match self
            .handle_mut()?
            .read()
            .map_err(VisionError::from_native)?
        {
            native::CaptureRead::End => Ok(CaptureRead::End),
            native::CaptureRead::Frame(image) => {
                let width = image.width();
                let height = image.height();
                let stride = image.stride();
                let format = vision_pixel_format(image.format());
                let pixels = Arc::from(image.into_pixels());
                Image::new(pixels, width, height, stride, format, &self.limits)
                    .map(CaptureRead::Frame)
                    .map_err(VisionError::from)
            }
        }
    }

    fn close(&mut self) -> Result<(), VisionError> {
        let result = self.handle_mut()?.close().map_err(VisionError::from_native);
        self.interrupt.take();
        result
    }

    fn interrupt(&self) -> Arc<dyn CaptureInterrupt> {
        self.interrupt
            .as_ref()
            .expect("native capture interrupt owner")
            .clone()
    }

    fn finalize(mut self: Box<Self>) -> BackendFinalize {
        self.interrupt.take();
        let handle = self.handle.take().expect("native capture owner");
        match handle.destroy() {
            native::CaptureDestroyOutcome::Consumed { diagnostic } => BackendFinalize::Consumed {
                diagnostic: diagnostic.map(VisionError::from_native),
            },
            native::CaptureDestroyOutcome::Unconsumed { handle, diagnostic } => {
                self.handle = Some(handle);
                BackendFinalize::Unconsumed {
                    backend: self,
                    diagnostic: VisionError::from_native(diagnostic),
                }
            }
        }
    }
}

impl CaptureBackend for SyntheticCapture {
    fn open(&mut self, cancellation: &CancellationToken) -> Result<CaptureProfile, VisionError> {
        let mut state = lock_recover(&self.shared.state);
        state.counts.open_calls += 1;
        self.shared.changed.notify_all();
        while state.block_open
            && !state.open_released
            && !state.stop_requested
            && !cancellation.is_cancelled()
        {
            state = wait_recover(&self.shared.changed, state);
        }
        if state.stop_requested || cancellation.is_cancelled() {
            return Err(VisionError::cancelled(
                "synthetic capture open was cancelled",
            ));
        }
        if let Some(message) = state.open_failure.take() {
            return Err(VisionError::new(
                VisionErrorKind::Faulted,
                message.to_string(),
            ));
        }
        Ok(self.profile.clone())
    }

    fn read(&mut self, cancellation: &CancellationToken) -> Result<CaptureRead, VisionError> {
        let mut state = lock_recover(&self.shared.state);
        state.counts.read_calls += 1;
        self.shared.changed.notify_all();
        while state.events.is_empty()
            && !state.panic_read
            && !state.stop_requested
            && !cancellation.is_cancelled()
        {
            state = wait_recover(&self.shared.changed, state);
        }
        if state.stop_requested || cancellation.is_cancelled() {
            return Err(VisionError::cancelled(
                "synthetic capture read was cancelled",
            ));
        }
        assert!(
            !std::mem::take(&mut state.panic_read),
            "failpoint:capture.read"
        );
        match state.events.pop_front().expect("non-empty event queue") {
            SyntheticEvent::Frame(image) => Ok(CaptureRead::Frame(image)),
            SyntheticEvent::Fault(message) => Err(VisionError::new(
                VisionErrorKind::Faulted,
                message.to_string(),
            )),
            SyntheticEvent::End => Ok(CaptureRead::End),
        }
    }

    fn close(&mut self) -> Result<(), VisionError> {
        let mut state = lock_recover(&self.shared.state);
        state.counts.close_calls += 1;
        state.stop_requested = true;
        self.shared.changed.notify_all();
        match state.close_failure.take() {
            Some(message) => Err(VisionError::new(
                VisionErrorKind::Native,
                message.to_string(),
            )),
            None => Ok(()),
        }
    }

    fn interrupt(&self) -> Arc<dyn CaptureInterrupt> {
        Arc::new(SyntheticInterrupt {
            shared: Arc::clone(&self.shared),
        })
    }

    fn finalize(self: Box<Self>) -> BackendFinalize {
        let mut state = lock_recover(&self.shared.state);
        if state.finalize_failures != 0 {
            state.finalize_failures -= 1;
            drop(state);
            return BackendFinalize::Unconsumed {
                backend: self,
                diagnostic: VisionError::new(
                    VisionErrorKind::Native,
                    "scripted synthetic finalize failure",
                ),
            };
        }
        state.counts.finalize_calls += 1;
        drop(state);
        BackendFinalize::Consumed { diagnostic: None }
    }
}

struct InterruptCoordinator {
    interrupt: Mutex<Option<Arc<dyn CaptureInterrupt>>>,
    state: Mutex<InterruptState>,
    changed: Condvar,
    panicked: AtomicBool,
}

#[derive(Default)]
struct InterruptState {
    sealed: bool,
    active: usize,
    error: Option<VisionError>,
}

impl InterruptCoordinator {
    fn new(interrupt: Arc<dyn CaptureInterrupt>) -> Self {
        Self {
            interrupt: Mutex::new(Some(interrupt)),
            state: Mutex::new(InterruptState::default()),
            changed: Condvar::new(),
            panicked: AtomicBool::new(false),
        }
    }

    fn request_stop(&self) {
        let interrupt = {
            let mut state = lock_recover(&self.state);
            if state.sealed {
                return;
            }
            state.active += 1;
            lock_recover(&self.interrupt)
                .as_ref()
                .expect("unsealed interrupt coordinator owns its token")
                .clone()
        };
        let result = catch_unwind(AssertUnwindSafe(|| interrupt.request_stop()));
        if result.is_err() {
            self.panicked.store(true, Ordering::Release);
        }
        let mut state = lock_recover(&self.state);
        state.active = state.active.saturating_sub(1);
        if let Ok(Err(error)) = result
            && state.error.is_none()
        {
            state.error = Some(error);
        }
        self.changed.notify_all();
    }

    fn seal_and_drain(&self) -> Result<(), VisionError> {
        let mut state = lock_recover(&self.state);
        state.sealed = true;
        while state.active != 0 {
            state = wait_recover(&self.changed, state);
        }
        let error = state.error.take();
        drop(state);
        lock_recover(&self.interrupt).take();
        if self.panicked.load(Ordering::Acquire) {
            return Err(VisionError::internal("capture interrupt callback panicked"));
        }
        error.map_or(Ok(()), Err)
    }
}

struct CaptureShared {
    state: Mutex<CaptureData>,
    changed: Condvar,
    clock: Arc<dyn Clock>,
    limits: VisionLimits,
    startup_deadline_ns: u64,
}

struct CaptureData {
    state: CaptureState,
    profile: Option<CaptureProfile>,
    latest: Option<Arc<Frame>>,
    fault: Option<VisionError>,
    next_sequence: u64,
    last_timestamp_ns: Option<u64>,
}

pub struct CaptureSession {
    resource: Arc<CaptureResource>,
}

impl std::fmt::Debug for CaptureSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureSession")
            .field("state", &self.state())
            .field("profile", &self.profile())
            .finish_non_exhaustive()
    }
}

struct CaptureResource {
    lifecycle_gate: Mutex<()>,
    shared: Arc<CaptureShared>,
    resource_cancellation: CancellationToken,
    coordinator: Arc<InterruptCoordinator>,
    resource_cancel_hook: Mutex<Option<CancellationHookRegistration>>,
    startup: Operation,
    task: Mutex<Option<SupervisedTask>>,
    exit_receiver: Mutex<Option<Receiver<WorkerExit>>>,
    fallback: Arc<Mutex<Option<WorkerExit>>>,
    unresolved: Mutex<Option<PendingFinalize>>,
    registration: Mutex<Option<ResourceRegistration>>,
    retention: Mutex<Option<Arc<CaptureResource>>>,
    close_result: Mutex<Option<Result<(), VisionError>>>,
}

struct StartGate {
    signal: Mutex<StartSignal>,
    changed: Condvar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartSignal {
    Pending,
    Run,
    Abort,
}

struct WorkerExit {
    backend: Box<dyn CaptureBackend>,
    coordinator: Arc<InterruptCoordinator>,
    close_error: Option<VisionError>,
    reason: WorkerExitReason,
}

struct PendingFinalize {
    backend: Box<dyn CaptureBackend>,
    evidence: CleanupEvidence,
}

struct CleanupEvidence {
    close_error: Option<VisionError>,
    interrupt_error: Option<VisionError>,
    worker_reason: WorkerExitReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerExitReason {
    Aborted,
    Stopped,
    Faulted,
    Panicked,
}

struct WorkerOwnerGuard {
    backend: Option<Box<dyn CaptureBackend>>,
    coordinator: Option<Arc<InterruptCoordinator>>,
    exit_sender: Option<Sender<WorkerExit>>,
    fallback: Arc<Mutex<Option<WorkerExit>>>,
    handed_off: bool,
}

struct WorkerLaunch {
    backend: Box<dyn CaptureBackend>,
    shared: Arc<CaptureShared>,
    cancellation: CancellationToken,
    start_gate: Arc<StartGate>,
    coordinator: Arc<InterruptCoordinator>,
    exit_sender: Sender<WorkerExit>,
    fallback: Arc<Mutex<Option<WorkerExit>>>,
    startup: Operation,
}

impl WorkerLaunch {
    fn into_backend(self) -> Box<dyn CaptureBackend> {
        self.backend
    }
}

impl CaptureSession {
    pub fn open_synthetic(
        runtime: &Runtime,
        backend: SyntheticCapture,
        options: CaptureOptions,
    ) -> Result<Self, VisionError> {
        Self::open_backend(runtime, Box::new(backend), options)
    }

    pub fn open_file(
        runtime: &Runtime,
        source: &Path,
        options: CaptureOptions,
        native_options: NativeCaptureOptions,
    ) -> Result<Self, VisionError> {
        if !source.is_absolute() {
            return Err(VisionError::validation(
                "file capture source must be absolute",
            ));
        }
        let source_id = source
            .to_str()
            .ok_or_else(|| VisionError::validation("file capture source is not UTF-8"))?;
        let display_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("File capture sequence");
        let backend = NativeCaptureBackend::create(
            CaptureBackendKind::File,
            Arc::from(source_id),
            Arc::from(display_name),
            options,
            native_options,
        )?;
        Self::open_backend(runtime, Box::new(backend), options)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_device(
        runtime: &Runtime,
        backend: CaptureBackendKind,
        source_id: impl Into<Arc<str>>,
        display_name: impl Into<Arc<str>>,
        options: CaptureOptions,
        native_options: NativeCaptureOptions,
    ) -> Result<Self, VisionError> {
        if !matches!(
            backend,
            CaptureBackendKind::DirectShow | CaptureBackendKind::MediaFoundation
        ) {
            return Err(VisionError::validation(
                "device capture requires DirectShow or Media Foundation",
            ));
        }
        let backend = NativeCaptureBackend::create(
            backend,
            source_id.into(),
            display_name.into(),
            options,
            native_options,
        )?;
        Self::open_backend(runtime, Box::new(backend), options)
    }

    fn open_backend(
        runtime: &Runtime,
        backend: Box<dyn CaptureBackend>,
        options: CaptureOptions,
    ) -> Result<Self, VisionError> {
        let clock = runtime.clock();
        let startup_deadline_ns = match clock.now_ns().checked_add(options.first_frame_timeout_ns) {
            Some(deadline) => deadline,
            None => {
                return Err(vision_error_after_finalize(
                    VisionError::limit("first-frame deadline overflows"),
                    backend,
                ));
            }
        };
        let resource_cancellation = runtime.child_cancellation_token();
        let startup = match runtime.create_operation(Some(startup_deadline_ns)) {
            Ok(startup) => startup,
            Err(error) => {
                return Err(construction_error_after_finalize(error, backend));
            }
        };
        if startup.start() != TransitionOutcome::Applied {
            let snapshot = startup.snapshot();
            let error = match snapshot.cancellation_reason {
                Some(CancellationReason::Deadline) => VisionError::new(
                    VisionErrorKind::Deadline,
                    "capture construction deadline elapsed",
                ),
                Some(CancellationReason::Requested) => {
                    VisionError::cancelled("capture construction was cancelled")
                }
                Some(CancellationReason::ParentClose) => VisionError::new(
                    VisionErrorKind::Closed,
                    "Runtime closed during capture construction",
                ),
                None => VisionError::internal("capture startup operation could not enter Running"),
            };
            let error = vision_error_after_finalize(error, backend);
            settle_startup_after_construction_failure(&startup, &vision_to_runtime_error(&error));
            return Err(error);
        }

        let coordinator = Arc::new(InterruptCoordinator::new(backend.interrupt()));
        let shared = Arc::new(CaptureShared {
            state: Mutex::new(CaptureData {
                state: CaptureState::Opening,
                profile: None,
                latest: None,
                fault: None,
                next_sequence: 1,
                last_timestamp_ns: None,
            }),
            changed: Condvar::new(),
            clock,
            limits: options.limits,
            startup_deadline_ns,
        });
        install_startup_cancel_hook(&startup, &shared, &coordinator, &resource_cancellation);
        let cancel_shared = Arc::downgrade(&shared);
        let cancel_coordinator = Arc::clone(&coordinator);
        let resource_cancel_hook = resource_cancellation.on_cancel_scoped(move || {
            if let Some(shared) = cancel_shared.upgrade() {
                begin_stopping_shared(&shared);
            }
            cancel_coordinator.request_stop();
        });

        let start_gate = Arc::new(StartGate {
            signal: Mutex::new(StartSignal::Pending),
            changed: Condvar::new(),
        });
        let fallback = Arc::new(Mutex::new(None));
        let (exit_sender, exit_receiver) = mpsc::channel();
        let resource = Arc::new(CaptureResource {
            lifecycle_gate: Mutex::new(()),
            shared: Arc::clone(&shared),
            resource_cancellation: resource_cancellation.clone(),
            coordinator: Arc::clone(&coordinator),
            resource_cancel_hook: Mutex::new(Some(resource_cancel_hook)),
            startup: startup.clone(),
            task: Mutex::new(None),
            exit_receiver: Mutex::new(Some(exit_receiver)),
            fallback: Arc::clone(&fallback),
            unresolved: Mutex::new(None),
            registration: Mutex::new(None),
            retention: Mutex::new(None),
            close_result: Mutex::new(None),
        });

        let worker_shared = Arc::clone(&shared);
        let worker_cancellation = resource_cancellation.clone();
        let worker_start_gate = Arc::clone(&start_gate);
        let worker_coordinator = Arc::clone(&coordinator);
        let worker_fallback = Arc::clone(&fallback);
        let worker_startup = startup.clone();
        let launch = WorkerLaunch {
            backend,
            shared: worker_shared,
            cancellation: worker_cancellation,
            start_gate: worker_start_gate,
            coordinator: worker_coordinator,
            exit_sender,
            fallback: worker_fallback,
            startup: worker_startup,
        };
        let launch_slot = Arc::new(Mutex::new(Some(launch)));
        let worker_launch_slot = Arc::clone(&launch_slot);
        let task = match runtime.spawn_supervised("easycon-capture-read", move || {
            let launch = lock_recover(&worker_launch_slot)
                .take()
                .expect("capture worker launch owner");
            capture_worker(launch);
        }) {
            Ok(task) => task,
            Err(error) => {
                let launch = lock_recover(&launch_slot)
                    .take()
                    .expect("failed spawn preserves capture worker launch owner");
                let cleanup = resource
                    .finish_never_started_construction(launch.into_backend())
                    .err();
                settle_startup_after_construction_failure(&startup, &error);
                return Err(construction_error_with_cleanup(
                    VisionError::from_runtime(error),
                    cleanup,
                ));
            }
        };
        *lock_recover(&resource.task) = Some(task.clone());

        let lifecycle = lock_recover(&resource.lifecycle_gate);
        let managed: Arc<dyn ManagedResource> = resource.clone();
        let registration = match runtime.register_resource(managed) {
            Ok(registration) => registration,
            Err(error) => {
                signal_start(&start_gate, StartSignal::Abort);
                drop(lifecycle);
                let task_error = match task.join() {
                    Ok(SupervisedTaskOutcome::Completed) => None,
                    Ok(SupervisedTaskOutcome::Panicked) => Some(VisionError::internal(
                        "capture construction worker panicked while aborting",
                    )),
                    Err(_) => Some(VisionError::internal(
                        "capture construction worker attempted to join itself",
                    )),
                };
                let cleanup = resource.finish_unregistered_construction().err();
                settle_startup_after_construction_failure(&startup, &error);
                return Err(construction_error_with_cleanup(
                    VisionError::from_runtime(error),
                    combine_diagnostics([task_error, cleanup]),
                ));
            }
        };
        *lock_recover(&resource.registration) = Some(registration);
        *lock_recover(&resource.retention) = Some(Arc::clone(&resource));
        signal_start(&start_gate, StartSignal::Run);
        drop(lifecycle);
        Ok(Self { resource })
    }

    #[must_use]
    pub fn state(&self) -> CaptureState {
        lock_recover(&self.resource.shared.state).state
    }

    #[must_use]
    pub fn profile(&self) -> Option<CaptureProfile> {
        lock_recover(&self.resource.shared.state).profile.clone()
    }

    #[must_use]
    pub fn startup_operation(&self) -> Operation {
        self.resource.startup.clone()
    }

    pub fn snapshot(
        &self,
        cancellation: &CancellationToken,
        wait: CaptureSnapshotWait,
    ) -> Result<Arc<Frame>, VisionError> {
        let weak = Arc::downgrade(&self.resource.shared);
        let _cancel_hook = cancellation.on_cancel_scoped(move || {
            if let Some(shared) = weak.upgrade() {
                shared.changed.notify_all();
            }
        });
        let _clock_registration = match wait {
            CaptureSnapshotWait::Poll => None,
            CaptureSnapshotWait::Until(_) => {
                let weak = Arc::downgrade(&self.resource.shared);
                Some(self.resource.shared.clock.on_change(Arc::new(move || {
                    if let Some(shared) = weak.upgrade() {
                        shared.changed.notify_all();
                    }
                })))
            }
        };
        let mut state = lock_recover(&self.resource.shared.state);
        loop {
            match state.state {
                CaptureState::Streaming => {
                    return state.latest.clone().ok_or_else(|| {
                        VisionError::internal("streaming capture has no latest Frame")
                    });
                }
                CaptureState::Faulted => {
                    return Err(state.fault.clone().unwrap_or_else(|| {
                        VisionError::new(
                            VisionErrorKind::Faulted,
                            "capture entered Faulted without a diagnostic",
                        )
                    }));
                }
                CaptureState::Stopping => {
                    return Err(VisionError::new(
                        VisionErrorKind::Closed,
                        "capture is stopping",
                    ));
                }
                CaptureState::Closed => {
                    return Err(VisionError::new(
                        VisionErrorKind::Closed,
                        "capture is closed",
                    ));
                }
                CaptureState::Opening => {}
            }
            if cancellation.is_cancelled() {
                return Err(VisionError::cancelled("capture snapshot was cancelled"));
            }
            match wait {
                CaptureSnapshotWait::Poll => {
                    return Err(VisionError::new(
                        VisionErrorKind::NoFrame,
                        "capture has not published its first Frame",
                    ));
                }
                CaptureSnapshotWait::Until(deadline_ns) => {
                    let now_ns = self.resource.shared.clock.now_ns();
                    if now_ns >= deadline_ns {
                        return Err(VisionError::new(
                            VisionErrorKind::Deadline,
                            "capture snapshot deadline elapsed",
                        ));
                    }
                    match self.resource.shared.clock.real_wait_duration(deadline_ns) {
                        Some(duration) => {
                            let (next, _) = self
                                .resource
                                .shared
                                .changed
                                .wait_timeout(state, duration)
                                .unwrap_or_else(|poison| poison.into_inner());
                            state = next;
                        }
                        None => {
                            state = wait_recover(&self.resource.shared.changed, state);
                        }
                    }
                }
            }
        }
    }

    pub fn close(&self) -> Result<(), VisionError> {
        self.resource.close_inner()
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.resource.begin_stop();
    }
}

impl CaptureResource {
    fn begin_stop(&self) {
        let cancel_startup = begin_stopping_shared(&self.shared);
        self.resource_cancellation.cancel();
        if cancel_startup {
            let _ = self.startup.request_cancel(CancellationReason::Requested);
        }
        self.coordinator.request_stop();
        self.shared.changed.notify_all();
    }

    fn close_inner(&self) -> Result<(), VisionError> {
        let _gate = lock_recover(&self.lifecycle_gate);
        if let Some(result) = lock_recover(&self.close_result).clone() {
            return result;
        }
        self.begin_stop();

        let mut pending = lock_recover(&self.unresolved).take();
        if pending.is_none() {
            let task = lock_recover(&self.task).clone().ok_or_else(|| {
                VisionError::internal("capture close has no supervised worker handle")
            })?;
            let outcome = task
                .join()
                .map_err(|_| VisionError::internal("capture worker attempted to join itself"))?;
            *lock_recover(&self.task) = None;
            let exit = self.take_worker_exit()?;
            let close_error = exit.close_error;
            let mut worker_reason = exit.reason;
            drop(exit.coordinator);
            if outcome == SupervisedTaskOutcome::Panicked {
                worker_reason = WorkerExitReason::Panicked;
            }

            lock_recover(&self.resource_cancel_hook).take();
            let interrupt_error = self.coordinator.seal_and_drain().err();
            pending = Some(PendingFinalize {
                backend: exit.backend,
                evidence: CleanupEvidence {
                    close_error,
                    interrupt_error,
                    worker_reason,
                },
            });
        }

        let PendingFinalize { backend, evidence } =
            pending.expect("worker exit or unresolved slot supplies backend owner");
        match backend.finalize() {
            BackendFinalize::Consumed { diagnostic } => {
                let panic_error = (evidence.worker_reason == WorkerExitReason::Panicked)
                    .then(|| VisionError::internal("capture worker panicked during cleanup"));
                let result = combine_diagnostics([
                    evidence.close_error,
                    evidence.interrupt_error,
                    diagnostic,
                    panic_error,
                ])
                .map_or(Ok(()), Err);
                self.complete_closed(result.clone());
                result
            }
            BackendFinalize::Unconsumed {
                backend,
                diagnostic,
            } => {
                *lock_recover(&self.unresolved) = Some(PendingFinalize { backend, evidence });
                Err(diagnostic)
            }
        }
    }

    fn take_worker_exit(&self) -> Result<WorkerExit, VisionError> {
        if let Some(receiver) = lock_recover(&self.exit_receiver).take()
            && let Ok(exit) = receiver.try_recv()
        {
            return Ok(exit);
        }
        lock_recover(&self.fallback).take().ok_or_else(|| {
            VisionError::internal("capture worker exited without handing off its backend owner")
        })
    }

    fn finish_never_started_construction(
        &self,
        backend: Box<dyn CaptureBackend>,
    ) -> Result<(), VisionError> {
        lock_recover(&self.resource_cancel_hook).take();
        let interrupt_error = self.coordinator.seal_and_drain().err();
        let finalize_error = finalize_construction_backend(backend).err();
        interrupt_error.or(finalize_error).map_or(Ok(()), Err)
    }

    fn finish_unregistered_construction(&self) -> Result<(), VisionError> {
        let exit = self.take_worker_exit()?;
        drop(exit.coordinator);
        lock_recover(&self.resource_cancel_hook).take();
        let interrupt_error = self.coordinator.seal_and_drain().err();
        let finalize_error = finalize_construction_backend(exit.backend).err();
        exit.close_error
            .or(interrupt_error)
            .or(finalize_error)
            .map_or(Ok(()), Err)
    }

    fn complete_closed(&self, result: Result<(), VisionError>) {
        {
            let mut state = lock_recover(&self.shared.state);
            state.state = CaptureState::Closed;
            state.latest = None;
            self.shared.changed.notify_all();
        }
        *lock_recover(&self.close_result) = Some(result);
        if let Some(registration) = lock_recover(&self.registration).take() {
            registration.unregister();
        }
        lock_recover(&self.retention).take();
    }
}

impl ManagedResource for CaptureResource {
    fn close(&self) {
        let _ = self.close_inner();
    }
}

fn capture_worker(launch: WorkerLaunch) {
    let mut owner = WorkerOwnerGuard {
        backend: Some(launch.backend),
        coordinator: Some(launch.coordinator),
        exit_sender: Some(launch.exit_sender),
        fallback: launch.fallback,
        handed_off: false,
    };
    let run = catch_unwind(AssertUnwindSafe(|| {
        run_capture_loop(
            owner.backend.as_deref_mut().expect("worker backend owner"),
            &launch.shared,
            &launch.cancellation,
            &launch.start_gate,
            &launch.startup,
        )
    }));
    if run.is_err() {
        set_fault(
            &launch.shared,
            VisionError::internal("capture worker panicked"),
        );
    }
    let reason = match &run {
        Ok(reason) => *reason,
        Err(_) => WorkerExitReason::Panicked,
    };
    let mut close_panicked = false;
    if matches!(
        reason,
        WorkerExitReason::Faulted | WorkerExitReason::Panicked
    ) && matches!(
        launch.startup.snapshot().state,
        OperationState::Pending | OperationState::Running
    ) {
        let error = lock_recover(&launch.shared.state)
            .fault
            .clone()
            .unwrap_or_else(|| {
                VisionError::new(
                    VisionErrorKind::Faulted,
                    format!("capture worker ended before first Frame: {reason:?}"),
                )
            });
        let outcome = launch
            .startup
            .fail_after_cleanup(vision_to_runtime_error(&error), || {
                close_panicked = owner.handoff(reason);
            });
        if !matches!(
            outcome,
            TransitionOutcome::Applied | TransitionOutcome::CleanupFailed
        ) {
            close_panicked = owner.handoff(reason);
        }
    } else {
        close_panicked = owner.handoff(reason);
    }
    settle_startup_after_worker(&launch.startup, &launch.shared, reason);
    if let Err(payload) = run {
        resume_unwind(payload);
    }
    assert!(!close_panicked, "capture backend close panicked");
}

fn run_capture_loop(
    backend: &mut dyn CaptureBackend,
    shared: &CaptureShared,
    cancellation: &CancellationToken,
    start_gate: &StartGate,
    startup: &Operation,
) -> WorkerExitReason {
    match wait_for_start(start_gate) {
        StartSignal::Abort => return WorkerExitReason::Aborted,
        StartSignal::Run => {}
        StartSignal::Pending => unreachable!("start wait cannot return Pending"),
    }
    let profile = match backend.open(cancellation) {
        Ok(profile) => profile,
        Err(error) => {
            if !cancellation.is_cancelled() && current_state(shared) != CaptureState::Faulted {
                set_fault(shared, normalize_capture_fault(error));
            }
            return if current_state(shared) == CaptureState::Faulted {
                WorkerExitReason::Faulted
            } else {
                WorkerExitReason::Stopped
            };
        }
    };
    if let Err(error) = validate_profile(&profile, &shared.limits) {
        set_fault(shared, error);
        return WorkerExitReason::Faulted;
    }
    {
        let mut state = lock_recover(&shared.state);
        if state.state != CaptureState::Opening {
            return if state.state == CaptureState::Faulted {
                WorkerExitReason::Faulted
            } else {
                WorkerExitReason::Stopped
            };
        }
        state.profile = Some(profile.clone());
        shared.changed.notify_all();
    }

    loop {
        if cancellation.is_cancelled() {
            return WorkerExitReason::Stopped;
        }
        match backend.read(cancellation) {
            Ok(CaptureRead::Frame(image)) => {
                if let Err(error) = publish_frame(shared, &profile, image, startup) {
                    set_fault(shared, error);
                    return WorkerExitReason::Faulted;
                }
            }
            Ok(CaptureRead::End) => {
                set_fault(
                    shared,
                    VisionError::new(VisionErrorKind::Faulted, "capture stream ended"),
                );
                return WorkerExitReason::Faulted;
            }
            Err(error) => {
                if cancellation.is_cancelled()
                    || matches!(
                        current_state(shared),
                        CaptureState::Stopping | CaptureState::Closed
                    )
                {
                    return WorkerExitReason::Stopped;
                }
                if current_state(shared) != CaptureState::Faulted {
                    set_fault(shared, normalize_capture_fault(error));
                }
                return WorkerExitReason::Faulted;
            }
        }
    }
}

fn publish_frame(
    shared: &CaptureShared,
    profile: &CaptureProfile,
    image: Image,
    startup: &Operation,
) -> Result<(), VisionError> {
    if image.width() != profile.width
        || image.height() != profile.height
        || image.stride() != profile.stride
        || image.format() != profile.pixel_format
    {
        return Err(VisionError::new(
            VisionErrorKind::Faulted,
            "capture frame does not match the opened profile",
        ));
    }
    let timestamp_ns = shared.clock.now_ns();
    let mut state = lock_recover(&shared.state);
    if let Some(previous) = state.last_timestamp_ns
        && timestamp_ns < previous
    {
        return Err(VisionError::internal(
            "capture clock moved backwards while publishing a Frame",
        ));
    }
    if !matches!(state.state, CaptureState::Opening | CaptureState::Streaming) {
        return Ok(());
    }
    let sequence = state.next_sequence;
    let frame = Arc::new(Frame::new(image, sequence, timestamp_ns).map_err(VisionError::from)?);
    let first = state.state == CaptureState::Opening;
    if first {
        let outcome = startup.succeed(OperationValue::Unit);
        if outcome != TransitionOutcome::Applied {
            let snapshot = startup.snapshot();
            let kind = match snapshot.cancellation_reason {
                Some(CancellationReason::Deadline) => VisionErrorKind::Deadline,
                Some(CancellationReason::Requested | CancellationReason::ParentClose) => {
                    VisionErrorKind::Cancelled
                }
                None => VisionErrorKind::Internal,
            };
            return Err(VisionError::new(
                kind,
                format!(
                    "capture first-frame startup lost publication race: {outcome:?} ({:?})",
                    snapshot.state
                ),
            ));
        }
    }
    state.next_sequence = sequence
        .checked_add(1)
        .ok_or_else(|| VisionError::internal("capture Frame sequence exhausted"))?;
    state.state = CaptureState::Streaming;
    state.last_timestamp_ns = Some(timestamp_ns);
    state.latest = Some(frame);
    shared.changed.notify_all();
    Ok(())
}

impl WorkerOwnerGuard {
    fn handoff(&mut self, reason: WorkerExitReason) -> bool {
        if self.handed_off {
            return false;
        }
        let mut close_panicked = false;
        let close_error = if reason == WorkerExitReason::Aborted {
            None
        } else {
            match catch_unwind(AssertUnwindSafe(|| {
                self.backend
                    .as_deref_mut()
                    .expect("worker backend owner")
                    .close()
            })) {
                Ok(result) => result.err(),
                Err(_) => {
                    close_panicked = true;
                    Some(VisionError::internal("capture backend close panicked"))
                }
            }
        };
        let exit = WorkerExit {
            backend: self.backend.take().expect("worker backend owner"),
            coordinator: self
                .coordinator
                .take()
                .expect("worker interrupt coordinator token"),
            close_error,
            reason: if close_panicked {
                WorkerExitReason::Panicked
            } else {
                reason
            },
        };
        self.handed_off = true;
        if let Some(sender) = self.exit_sender.take()
            && let Err(error) = sender.send(exit)
        {
            place_fallback(&self.fallback, error.0);
        }
        close_panicked
    }
}

impl Drop for WorkerOwnerGuard {
    fn drop(&mut self) {
        if !self.handed_off {
            let _ = self.handoff(WorkerExitReason::Panicked);
        }
    }
}

fn install_startup_cancel_hook(
    startup: &Operation,
    shared: &Arc<CaptureShared>,
    coordinator: &Arc<InterruptCoordinator>,
    resource_cancellation: &CancellationToken,
) {
    let weak = Arc::downgrade(shared);
    let coordinator = Arc::clone(coordinator);
    let resource_cancellation = resource_cancellation.clone();
    startup.on_cancel(move || {
        let transitioned = weak.upgrade().is_some_and(|shared| {
            if resource_cancellation.is_cancelled() {
                begin_stopping_if_opening(&shared)
            } else {
                let kind = if shared.clock.now_ns() >= shared.startup_deadline_ns {
                    VisionErrorKind::Deadline
                } else {
                    VisionErrorKind::Cancelled
                };
                set_fault_if_opening(
                    &shared,
                    VisionError::new(kind, "capture first-frame startup was cancelled"),
                )
            }
        });
        if transitioned {
            coordinator.request_stop();
        }
    });
}

fn settle_startup_after_worker(
    startup: &Operation,
    shared: &CaptureShared,
    reason: WorkerExitReason,
) {
    if reason == WorkerExitReason::Aborted {
        return;
    }
    match startup.snapshot().state {
        OperationState::Cancelling => {
            let _ = startup.finish_cancelled();
        }
        OperationState::Pending | OperationState::Running => {
            let error = lock_recover(&shared.state)
                .fault
                .clone()
                .unwrap_or_else(|| {
                    VisionError::new(
                        VisionErrorKind::Faulted,
                        format!("capture worker ended before first Frame: {reason:?}"),
                    )
                });
            let _ = startup.fail(vision_to_runtime_error(&error));
        }
        OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled => {}
    }
}

fn settle_startup_after_construction_failure(startup: &Operation, error: &EasyConError) {
    match startup.snapshot().state {
        OperationState::Cancelling => {
            let _ = startup.finish_cancelled();
        }
        OperationState::Pending | OperationState::Running => {
            let _ = startup.fail(error.clone());
        }
        OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled => {}
    }
}

fn validate_profile(profile: &CaptureProfile, limits: &VisionLimits) -> Result<(), VisionError> {
    if profile.width > limits.max_width() || profile.height > limits.max_height() {
        return Err(VisionError::limit(
            "capture profile dimensions exceed Vision limits",
        ));
    }
    let pixels = u64::from(profile.width)
        .checked_mul(u64::from(profile.height))
        .ok_or_else(|| VisionError::limit("capture profile pixel count overflows"))?;
    if pixels > limits.max_pixels() {
        return Err(VisionError::limit(
            "capture profile pixel count exceeds Vision limits",
        ));
    }
    let row_bytes = usize::try_from(profile.width)
        .ok()
        .and_then(|width| width.checked_mul(profile.pixel_format.channels()))
        .ok_or_else(|| VisionError::limit("capture profile row bytes overflow"))?;
    let decoded_bytes = profile
        .stride
        .checked_mul(
            usize::try_from(profile.height)
                .map_err(|_| VisionError::limit("capture profile height overflows"))?,
        )
        .ok_or_else(|| VisionError::limit("capture profile decoded bytes overflow"))?;
    if profile.stride < row_bytes {
        return Err(VisionError::validation(
            "capture profile stride is shorter than one pixel row",
        ));
    }
    if profile.stride > limits.max_stride() {
        return Err(VisionError::limit(
            "capture profile stride exceeds Vision limits",
        ));
    }
    if decoded_bytes > limits.max_decoded_bytes() {
        return Err(VisionError::limit(
            "capture profile decoded bytes exceed Vision limits",
        ));
    }
    Ok(())
}

fn normalize_capture_fault(error: VisionError) -> VisionError {
    if matches!(
        error.kind(),
        VisionErrorKind::Deadline | VisionErrorKind::Cancelled
    ) {
        error
    } else {
        error.reclassify(VisionErrorKind::Faulted)
    }
}

fn native_backend(backend: CaptureBackendKind) -> Result<native::CaptureBackend, VisionError> {
    match backend {
        CaptureBackendKind::File => Ok(native::CaptureBackend::File),
        CaptureBackendKind::DirectShow => Ok(native::CaptureBackend::DirectShow),
        CaptureBackendKind::MediaFoundation => Ok(native::CaptureBackend::MediaFoundation),
        CaptureBackendKind::Synthetic => Err(VisionError::validation(
            "synthetic capture has no native backend",
        )),
    }
}

const fn vision_backend(backend: native::CaptureBackend) -> CaptureBackendKind {
    match backend {
        native::CaptureBackend::File => CaptureBackendKind::File,
        native::CaptureBackend::DirectShow => CaptureBackendKind::DirectShow,
        native::CaptureBackend::MediaFoundation => CaptureBackendKind::MediaFoundation,
    }
}

const fn vision_pixel_format(format: easycon_native_sys::codec::PixelFormat) -> PixelFormat {
    match format {
        easycon_native_sys::codec::PixelFormat::Bgr8 => PixelFormat::Bgr8,
        easycon_native_sys::codec::PixelFormat::Bgra8 => PixelFormat::Bgra8,
        easycon_native_sys::codec::PixelFormat::Gray8 => PixelFormat::Gray8,
    }
}

fn validate_capture_identity(source_id: &str, display_name: &str) -> Result<(), VisionError> {
    if source_id.is_empty()
        || source_id.len() > MAX_CAPTURE_SOURCE_BYTES
        || source_id.contains('\0')
    {
        return Err(VisionError::validation(
            "capture source ID must be non-empty, bounded UTF-8 without NUL",
        ));
    }
    if display_name.is_empty()
        || display_name.len() > MAX_CAPTURE_NAME_BYTES
        || display_name.contains('\0')
    {
        return Err(VisionError::validation(
            "capture display name must be non-empty, bounded UTF-8 without NUL",
        ));
    }
    Ok(())
}

fn construction_error_after_finalize(
    error: EasyConError,
    backend: Box<dyn CaptureBackend>,
) -> VisionError {
    vision_error_after_finalize(VisionError::from_runtime(error), backend)
}

fn vision_error_after_finalize(
    primary: VisionError,
    backend: Box<dyn CaptureBackend>,
) -> VisionError {
    let cleanup = finalize_construction_backend(backend).err();
    construction_error_with_cleanup(primary, cleanup)
}

fn construction_error_with_cleanup(
    primary: VisionError,
    cleanup: Option<VisionError>,
) -> VisionError {
    match cleanup {
        None => primary,
        Some(cleanup) => primary.append_diagnostic(VisionError::new(
            cleanup.kind(),
            format!("backend cleanup failed: {cleanup}"),
        )),
    }
}

fn combine_diagnostics<const N: usize>(
    diagnostics: [Option<VisionError>; N],
) -> Option<VisionError> {
    diagnostics
        .into_iter()
        .flatten()
        .reduce(VisionError::append_diagnostic)
}

fn finalize_construction_backend(backend: Box<dyn CaptureBackend>) -> Result<(), VisionError> {
    match backend.finalize() {
        BackendFinalize::Consumed { diagnostic } => diagnostic.map_or(Ok(()), Err),
        BackendFinalize::Unconsumed {
            backend,
            diagnostic,
        } => match backend.finalize() {
            BackendFinalize::Consumed { diagnostic: later } => Err(later.unwrap_or(diagnostic)),
            BackendFinalize::Unconsumed {
                diagnostic: later, ..
            } => Err(VisionError::internal(format!(
                "capture construction destroy remained unconsumed after retry: {diagnostic}; {later}"
            ))),
        },
    }
}

fn vision_to_runtime_error(error: &VisionError) -> EasyConError {
    let (domain, code) = match error.kind() {
        VisionErrorKind::Validation | VisionErrorKind::Limit => {
            (ErrorDomain::Validation, ErrorCode::InvalidArgument)
        }
        VisionErrorKind::Deadline => (ErrorDomain::Runtime, ErrorCode::DeadlineExceeded),
        VisionErrorKind::Cancelled | VisionErrorKind::Closed => {
            (ErrorDomain::Runtime, ErrorCode::Cancelled)
        }
        VisionErrorKind::Faulted | VisionErrorKind::NoFrame | VisionErrorKind::Native => {
            (ErrorDomain::Io, ErrorCode::Transport)
        }
        VisionErrorKind::InvalidImage
        | VisionErrorKind::ModelNotFound
        | VisionErrorKind::PoolClosed
        | VisionErrorKind::Internal => (ErrorDomain::Internal, ErrorCode::Internal),
    };
    EasyConError::new(domain, code, error.message())
}

fn set_fault(shared: &CaptureShared, error: VisionError) {
    let mut state = lock_recover(&shared.state);
    if matches!(state.state, CaptureState::Opening | CaptureState::Streaming) {
        state.state = CaptureState::Faulted;
        state.fault = Some(error);
        shared.changed.notify_all();
    }
}

fn set_fault_if_opening(shared: &CaptureShared, error: VisionError) -> bool {
    let mut state = lock_recover(&shared.state);
    if state.state != CaptureState::Opening {
        return false;
    }
    state.state = CaptureState::Faulted;
    state.fault = Some(error);
    shared.changed.notify_all();
    true
}

fn begin_stopping_if_opening(shared: &CaptureShared) -> bool {
    let mut state = lock_recover(&shared.state);
    if state.state != CaptureState::Opening {
        return false;
    }
    state.state = CaptureState::Stopping;
    shared.changed.notify_all();
    true
}

fn begin_stopping_shared(shared: &CaptureShared) -> bool {
    let mut state = lock_recover(&shared.state);
    let opening = state.state == CaptureState::Opening;
    if state.state != CaptureState::Closed {
        state.state = CaptureState::Stopping;
        shared.changed.notify_all();
    }
    opening
}

fn current_state(shared: &CaptureShared) -> CaptureState {
    lock_recover(&shared.state).state
}

fn wait_for_start(start_gate: &StartGate) -> StartSignal {
    let mut signal = lock_recover(&start_gate.signal);
    while *signal == StartSignal::Pending {
        signal = wait_recover(&start_gate.changed, signal);
    }
    *signal
}

fn signal_start(start_gate: &StartGate, signal: StartSignal) {
    let mut current = lock_recover(&start_gate.signal);
    if *current == StartSignal::Pending {
        *current = signal;
        start_gate.changed.notify_all();
    }
}

fn place_fallback(fallback: &Mutex<Option<WorkerExit>>, exit: WorkerExit) {
    let mut slot = lock_recover(fallback);
    if slot.is_none() {
        *slot = Some(exit);
    } else {
        std::mem::forget(exit);
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait_recover<'a, T>(changed: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    changed
        .wait(guard)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use easycon_runtime::VirtualClock;

    #[test]
    fn aborted_worker_leaves_startup_terminal_ownership_with_the_constructor() {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let startup = runtime
            .create_operation(None)
            .expect("startup operation admission");
        assert_eq!(startup.start(), TransitionOutcome::Applied);
        let shared = CaptureShared {
            state: Mutex::new(CaptureData {
                state: CaptureState::Opening,
                profile: None,
                latest: None,
                fault: None,
                next_sequence: 1,
                last_timestamp_ns: None,
            }),
            changed: Condvar::new(),
            clock,
            limits: VisionLimits::try_for_images(4096, 64, 64, 4096, 64 * 1024, 256)
                .expect("capture limits"),
            startup_deadline_ns: 1_000,
        };

        settle_startup_after_worker(&startup, &shared, WorkerExitReason::Aborted);
        assert_eq!(startup.snapshot().state, OperationState::Running);

        let parent_close = EasyConError::new(
            ErrorDomain::Runtime,
            ErrorCode::Cancelled,
            "Runtime closed during capture construction",
        );
        settle_startup_after_construction_failure(&startup, &parent_close);
        let snapshot = startup.snapshot();
        assert_eq!(snapshot.state, OperationState::Failed);
        assert_eq!(snapshot.error, Some(parent_close));
        assert_eq!(runtime.close(), Ok(easycon_runtime::CloseOutcome::Closed));
    }
}
