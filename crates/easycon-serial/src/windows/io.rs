use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_sys::Win32::Devices::Communication::{
    COMMTIMEOUTS, DCB, GetCommState, NOPARITY, ONESTOPBIT, PURGE_RXABORT, PURGE_RXCLEAR,
    PURGE_TXABORT, PURGE_TXCLEAR, PurgeComm, SetCommState, SetCommTimeouts, SetupComm,
};
use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
};

use crate::{
    ByteIo, ByteIoFactory, ByteIoRequest, SerialError, SerialErrorKind, SerialPortDescriptor,
};

use super::MAX_COM_PORT_NAME_CHARS;
use super::error::{from_code, last_error};

/// Opens Windows COM ports with exclusive overlapped byte I/O.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsByteIoFactory;

impl ByteIoFactory for WindowsByteIoFactory {
    fn open(
        &mut self,
        port: &SerialPortDescriptor,
        baud_rate: u32,
        request: ByteIoRequest,
    ) -> Result<Box<dyn ByteIo>, SerialError> {
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        if baud_rate == 0 {
            return Err(SerialError::new(
                SerialErrorKind::InvalidPort,
                "serial baud rate must be non-zero",
            ));
        }
        let path = port_path(port.port_name())?;
        // SAFETY: path is a NUL-terminated UTF-16 buffer; null security/template pointers and
        // exclusive share mode are documented for serial devices. The returned HANDLE is wrapped
        // immediately and opened with FILE_FLAG_OVERLAPPED for every later operation.
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateFileW(serial port)"));
        }
        let handle = OwnedHandle::new(raw);
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        configure_serial(handle.raw(), baud_rate)?;
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        let close_event = OwnedHandle::manual_event()?;
        Ok(Box::new(WindowsByteIo {
            shared: Arc::new(SharedPort {
                handle,
                close_event,
                closed: AtomicBool::new(false),
            }),
        }))
    }
}

struct WindowsByteIo {
    shared: Arc<SharedPort>,
}

impl ByteIo for WindowsByteIo {
    fn read(&mut self, buffer: &mut [u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        self.transfer(TransferBuffer::Read(buffer), request)
    }

    fn write(&mut self, buffer: &[u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        self.transfer(TransferBuffer::Write(buffer), request)
    }

    fn discard_input(&mut self, request: ByteIoRequest) -> Result<(), SerialError> {
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(disconnected_error("serial port closed before input purge"));
        }
        // SAFETY: shared owns a live serial HANDLE and no byte transfer can run concurrently
        // because ByteIo is exclusively borrowed by one Controller lane.
        if unsafe { PurgeComm(self.shared.handle.raw(), PURGE_RXABORT | PURGE_RXCLEAR) } == 0 {
            return Err(last_error("PurgeComm(discard input)"));
        }
        Ok(())
    }

    fn close(&mut self) {
        self.shared.close();
    }
}

impl WindowsByteIo {
    fn transfer(
        &mut self,
        buffer: TransferBuffer<'_>,
        request: ByteIoRequest,
    ) -> Result<usize, SerialError> {
        if buffer.is_empty() {
            return Err(SerialError::new(
                SerialErrorKind::InvalidPort,
                "serial byte I/O buffer must be non-empty",
            ));
        }
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(disconnected_error("serial port is closed"));
        }

        let completion_event = OwnedHandle::manual_event()?;
        let interrupt_event = Arc::new(OwnedHandle::auto_event()?);
        let mut overlapped = Box::new(OVERLAPPED {
            hEvent: completion_event.raw(),
            ..OVERLAPPED::default()
        });
        let operation_cancel_event = interrupt_event.clone();
        let _operation_cancel = request.cancellation.on_cancel_scoped(move || {
            operation_cancel_event.signal();
        });
        let resource_cancel_event = interrupt_event.clone();
        let _resource_cancel = request.resource_cancellation.on_cancel_scoped(move || {
            resource_cancel_event.signal();
        });
        let clock_event = interrupt_event.clone();
        let _clock_change = request.clock.on_change(Arc::new(move || {
            clock_event.signal();
        }));

        let operation = buffer.operation_name();
        let requested = u32::try_from(buffer.len().min(u32::MAX as usize))
            .expect("transfer length was capped at u32");
        // SAFETY: the serial HANDLE was opened for overlapped I/O; buffer and OVERLAPPED remain
        // alive and immovable until completion is observed below. Only the exclusive lane borrow
        // can initiate a transfer on this ByteIo object. Windows 10/11 overlapped calls use a null
        // synchronous byte-count pointer; GetOverlappedResult owns the completion count.
        let started = unsafe {
            match buffer {
                TransferBuffer::Read(bytes) => ReadFile(
                    self.shared.handle.raw(),
                    bytes.as_mut_ptr(),
                    requested,
                    null_mut(),
                    overlapped.as_mut(),
                ),
                TransferBuffer::Write(bytes) => WriteFile(
                    self.shared.handle.raw(),
                    bytes.as_ptr(),
                    requested,
                    null_mut(),
                    overlapped.as_mut(),
                ),
            }
        };
        if started != 0 {
            let transferred = normalize_progress(overlapped_result(
                &self.shared,
                overlapped.as_mut(),
                operation,
                false,
            )?)?;
            request.publish_final_write_acceptance(transferred)?;
            return Ok(transferred);
        }
        // SAFETY: sampled immediately after ReadFile/WriteFile on the same thread.
        let start_error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if start_error != ERROR_IO_PENDING {
            let error = from_code(operation, start_error);
            let interruption = if start_error == ERROR_OPERATION_ABORTED {
                request.interruption()
            } else {
                None
            };
            return Err(prefer_causal_interruption(error, interruption));
        }

        // `Clock` is a safe injectable trait and may panic. The kernel still owns the buffer and
        // OVERLAPPED while this wait is pending, so unwind may resume only after cancellation has
        // been settled synchronously.
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wait_for_overlapped(
                &self.shared,
                &completion_event,
                &interrupt_event,
                overlapped.as_mut(),
                &request,
                operation,
            )
        }));
        let transferred = match waited {
            Ok(result) => result?,
            Err(payload) => resume_unwind_after_cleanup(payload, || {
                let _ = cancel_and_settle(
                    &self.shared,
                    overlapped.as_mut(),
                    SerialError::new(
                        SerialErrorKind::Io,
                        "serial byte I/O wait panicked before completion",
                    ),
                    operation,
                );
            }),
        };
        let transferred = normalize_progress(transferred)?;
        request.publish_final_write_acceptance(transferred)?;
        Ok(transferred)
    }
}

impl Drop for WindowsByteIo {
    fn drop(&mut self) {
        self.close();
    }
}

enum TransferBuffer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
}

impl TransferBuffer<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Self::Read(bytes) => bytes.is_empty(),
            Self::Write(bytes) => bytes.is_empty(),
        }
    }

    const fn operation_name(&self) -> &'static str {
        match self {
            Self::Read(_) => "ReadFile(serial port)",
            Self::Write(_) => "WriteFile(serial port)",
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Read(bytes) => bytes.len(),
            Self::Write(bytes) => bytes.len(),
        }
    }
}

struct SharedPort {
    handle: OwnedHandle,
    close_event: OwnedHandle,
    closed: AtomicBool,
}

impl SharedPort {
    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.close_event.signal();
            // SAFETY: handle remains owned by self while cancellation is requested; null
            // OVERLAPPED intentionally cancels every operation issued by this handle.
            let _ = unsafe { CancelIoEx(self.handle.raw(), null()) };
        }
    }
}

struct OwnedHandle(isize);

impl OwnedHandle {
    fn new(raw: HANDLE) -> Self {
        Self(raw as isize)
    }

    fn auto_event() -> Result<Self, SerialError> {
        Self::event(false)
    }

    fn manual_event() -> Result<Self, SerialError> {
        Self::event(true)
    }

    fn event(manual_reset: bool) -> Result<Self, SerialError> {
        // SAFETY: null security/name pointers and a false initial-state flag are documented; the
        // returned event HANDLE is wrapped immediately and the bool converts to Win32 BOOL.
        let raw = unsafe { CreateEventW(null(), i32::from(manual_reset), 0, null()) };
        if raw.is_null() {
            Err(last_error("CreateEventW(serial I/O)"))
        } else {
            Ok(Self::new(raw))
        }
    }

    fn raw(&self) -> HANDLE {
        self.0 as HANDLE
    }

    fn signal(&self) {
        // SAFETY: self owns a live event HANDLE; SetEvent neither transfers nor extends ownership.
        let _ = unsafe { SetEvent(self.raw()) };
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: OwnedHandle is the unique owner and closes the non-null, non-invalid HANDLE once.
        let _ = unsafe { windows_sys::Win32::Foundation::CloseHandle(self.raw()) };
    }
}

fn configure_serial(handle: HANDLE, baud_rate: u32) -> Result<(), SerialError> {
    // SAFETY: handle is a live serial HANDLE uniquely owned by the opening ByteIo factory.
    if unsafe { SetupComm(handle, 4096, 4096) } == 0 {
        return Err(last_error("SetupComm"));
    }
    let mut dcb = DCB {
        DCBlength: u32::try_from(size_of::<DCB>()).expect("DCB size fits u32"),
        ..DCB::default()
    };
    // SAFETY: dcb is initialized with the required length and writable for this live serial HANDLE.
    if unsafe { GetCommState(handle, &mut dcb) } == 0 {
        return Err(last_error("GetCommState"));
    }
    dcb.BaudRate = baud_rate;
    dcb._bitfield = 1; // fBinary=1; parity, software flow control, DTR, and RTS flow are disabled.
    dcb.ByteSize = 8;
    dcb.Parity = NOPARITY;
    dcb.StopBits = ONESTOPBIT;
    // SAFETY: dcb contains a complete 8N1 configuration and handle remains live and exclusive.
    if unsafe { SetCommState(handle, &dcb) } == 0 {
        return Err(last_error("SetCommState"));
    }
    let timeouts = COMMTIMEOUTS::default();
    // SAFETY: timeouts points to a fully initialized structure; zero driver timeouts leave the
    // overlapped operation pending so the SDK's absolute deadline/cancellation owns completion.
    if unsafe { SetCommTimeouts(handle, &timeouts) } == 0 {
        return Err(last_error("SetCommTimeouts"));
    }
    // SAFETY: handle is live and no application I/O has started; purge establishes a clean stream.
    if unsafe {
        PurgeComm(
            handle,
            PURGE_RXABORT | PURGE_RXCLEAR | PURGE_TXABORT | PURGE_TXCLEAR,
        )
    } == 0
    {
        return Err(last_error("PurgeComm(open)"));
    }
    Ok(())
}

fn wait_for_overlapped(
    shared: &SharedPort,
    completion_event: &OwnedHandle,
    interrupt_event: &OwnedHandle,
    overlapped: &mut OVERLAPPED,
    request: &ByteIoRequest,
    operation: &'static str,
) -> Result<u32, SerialError> {
    let handles = [
        completion_event.raw(),
        interrupt_event.raw(),
        shared.close_event.raw(),
    ];
    loop {
        if let Some(error) = request.interruption() {
            return cancel_and_settle(shared, overlapped, error, operation);
        }
        if shared.closed.load(Ordering::Acquire) {
            return cancel_and_settle(
                shared,
                overlapped,
                disconnected_error("serial port closed during byte I/O"),
                operation,
            );
        }
        let timeout_ms = wait_timeout_ms(request);
        // SAFETY: all three HANDLEs are live for this stack frame; the slice has the exact count
        // supplied, and no handle is closed until the wait and overlapped settlement finish.
        let wait = unsafe {
            WaitForMultipleObjects(
                u32::try_from(handles.len()).expect("wait handle count fits u32"),
                handles.as_ptr(),
                0,
                timeout_ms,
            )
        };
        match wait {
            WAIT_OBJECT_0 => {
                return overlapped_result(shared, overlapped, operation, false).map_err(|error| {
                    let interruption = if error.os_code() == Some(ERROR_OPERATION_ABORTED) {
                        request.interruption().or_else(|| {
                            shared
                                .closed
                                .load(Ordering::Acquire)
                                .then(|| disconnected_error("serial port closed during byte I/O"))
                        })
                    } else {
                        None
                    };
                    prefer_causal_interruption(error, interruption)
                });
            }
            value if value == WAIT_OBJECT_0 + 1 => continue,
            value if value == WAIT_OBJECT_0 + 2 => {
                return cancel_and_settle(
                    shared,
                    overlapped,
                    disconnected_error("serial port closed during byte I/O"),
                    operation,
                );
            }
            WAIT_TIMEOUT => continue,
            WAIT_FAILED => {
                let error = last_error("WaitForMultipleObjects(serial I/O)");
                return cancel_and_settle(shared, overlapped, error, operation);
            }
            _ => {
                return cancel_and_settle(
                    shared,
                    overlapped,
                    SerialError::new(
                        SerialErrorKind::Io,
                        "WaitForMultipleObjects returned an unexpected status",
                    ),
                    operation,
                );
            }
        }
    }
}

fn cancel_and_settle(
    shared: &SharedPort,
    overlapped: &mut OVERLAPPED,
    interruption: SerialError,
    operation: &'static str,
) -> Result<u32, SerialError> {
    // SAFETY: overlapped is the exact pending request issued on this live serial HANDLE and remains
    // allocated until GetOverlappedResult observes its terminal completion.
    let cancel_error = if unsafe { CancelIoEx(shared.handle.raw(), overlapped) } == 0 {
        // SAFETY: sampled immediately after CancelIoEx on the same thread.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        cancel_request_error(code)
    } else {
        None
    };
    settle_cancelled_completion(
        cancel_error,
        overlapped_result(shared, overlapped, operation, true),
        interruption,
    )
}

fn cancel_request_error(code: u32) -> Option<SerialError> {
    (code != ERROR_NOT_FOUND).then(|| from_code("CancelIoEx(serial I/O)", code))
}

fn settle_cancelled_completion(
    cancel_error: Option<SerialError>,
    completion: Result<u32, SerialError>,
    interruption: SerialError,
) -> Result<u32, SerialError> {
    match completion {
        Ok(transferred) => Ok(transferred),
        Err(error) if error.os_code() == Some(ERROR_OPERATION_ABORTED) => Err(interruption),
        Err(error) => Err(cancel_error.unwrap_or(error)),
    }
}

fn overlapped_result(
    shared: &SharedPort,
    overlapped: &mut OVERLAPPED,
    operation: &'static str,
    wait: bool,
) -> Result<u32, SerialError> {
    let mut transferred = 0_u32;
    // SAFETY: overlapped belongs to an operation issued on shared.handle and remains live; the
    // output count is writable. A true wait is used only after cancellation to settle ownership.
    if unsafe {
        GetOverlappedResult(
            shared.handle.raw(),
            overlapped,
            &mut transferred,
            i32::from(wait),
        )
    } == 0
    {
        Err(last_error(operation))
    } else {
        Ok(transferred)
    }
}

fn wait_timeout_ms(request: &ByteIoRequest) -> u32 {
    request
        .clock
        .real_wait_duration(request.deadline_ns)
        .map(duration_to_wait_ms)
        .unwrap_or(INFINITE)
}

fn duration_to_wait_ms(duration: Duration) -> u32 {
    if duration.is_zero() {
        return 0;
    }
    let millis = duration.as_nanos().div_ceil(1_000_000);
    u32::try_from(millis.min(u128::from(INFINITE - 1))).expect("wait milliseconds fit u32")
}

fn normalize_progress(transferred: u32) -> Result<usize, SerialError> {
    if transferred == 0 {
        Err(SerialError::new(
            SerialErrorKind::ZeroProgress,
            "Windows serial I/O completed without byte progress",
        ))
    } else {
        Ok(usize::try_from(transferred).expect("u32 fits usize"))
    }
}

fn prefer_causal_interruption(
    error: SerialError,
    interruption: Option<SerialError>,
) -> SerialError {
    if error.os_code() == Some(ERROR_OPERATION_ABORTED) {
        interruption.unwrap_or(error)
    } else {
        error
    }
}

fn resume_unwind_after_cleanup(
    payload: Box<dyn std::any::Any + Send>,
    cleanup: impl FnOnce(),
) -> ! {
    cleanup();
    std::panic::resume_unwind(payload)
}

fn disconnected_error(message: &'static str) -> SerialError {
    SerialError::new(SerialErrorKind::Disconnected, message)
}

fn port_path(port_name: &str) -> Result<Vec<u16>, SerialError> {
    let name = port_name.strip_prefix(r"\\.\").unwrap_or(port_name);
    let valid = name.len() <= MAX_COM_PORT_NAME_CHARS
        && name
            .get(..3)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("COM"))
        && name.get(3..).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        });
    if !valid || name.contains('\0') {
        return Err(SerialError::new(
            SerialErrorKind::InvalidPort,
            "Windows serial port name must be COM followed by decimal digits",
        ));
    }
    Ok(format!(r"\\.\{name}")
        .encode_utf16()
        .chain(Some(0))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::{Pin, pin};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::task::{Context, Poll, Wake, Waker};

    use easycon_controller::{
        AutomationLeaseAcquireOutcome, AutomationLeaseReleaseOutcome, ConnectOptions,
        ControllerAction, ControllerOptions, ControllerSession, HANDSHAKE_REPLY, HANDSHAKE_REQUEST,
        WriteKind,
    };
    use easycon_model::Button;
    use easycon_runtime::{
        CancellationReason, CancellationToken, CloseOutcome, Operation, OperationState, Runtime,
        VirtualClock, WaitResult, WaitTimeout,
    };

    use super::*;
    use crate::{
        ByteIo, ByteIoFactory, ByteIoOperation, ByteIoRequest, SerialControllerTransport,
        SerialPortDescriptor,
    };

    const TEST_WAIT: Duration = Duration::from_secs(2);

    #[derive(Default)]
    struct CompletionBoundaryState {
        admission_resolution_waiting: bool,
        allow_admission_resolution: bool,
        full_completion_waiting: bool,
        interrupt_observed: bool,
        allow_completion_consumption: bool,
        cancel_not_found_consumed: bool,
        report_writes: usize,
        neutral_writes: usize,
    }

    #[derive(Default)]
    struct CompletionBoundary {
        state: Mutex<CompletionBoundaryState>,
        changed: Condvar,
    }

    impl CompletionBoundary {
        fn wait_for(&self, predicate: impl Fn(&CompletionBoundaryState) -> bool, message: &str) {
            let state = self.state.lock().expect("completion boundary lock");
            let (state, timeout) = self
                .changed
                .wait_timeout_while(state, TEST_WAIT, |state| !predicate(state))
                .expect("completion boundary wait");
            assert!(!timeout.timed_out() && predicate(&state), "{message}");
        }

        fn allow_completion_consumption(&self) {
            let mut state = self.state.lock().expect("completion boundary lock");
            state.allow_completion_consumption = true;
            self.changed.notify_all();
        }

        fn admission_resolution_hook(&self) {
            let mut state = self.state.lock().expect("completion boundary lock");
            state.admission_resolution_waiting = true;
            self.changed.notify_all();
            while !state.allow_admission_resolution {
                state = self
                    .changed
                    .wait(state)
                    .expect("completion boundary admission wait");
            }
        }

        fn allow_admission_resolution(&self) {
            let mut state = self.state.lock().expect("completion boundary lock");
            state.allow_admission_resolution = true;
            self.changed.notify_all();
        }

        fn counts(&self) -> (usize, usize, bool) {
            let state = self.state.lock().expect("completion boundary lock");
            (
                state.report_writes,
                state.neutral_writes,
                state.cancel_not_found_consumed,
            )
        }
    }

    struct CompletionFactory {
        boundary: Arc<CompletionBoundary>,
    }

    impl ByteIoFactory for CompletionFactory {
        fn open(
            &mut self,
            _port: &SerialPortDescriptor,
            baud_rate: u32,
            request: ByteIoRequest,
        ) -> Result<Box<dyn ByteIo>, SerialError> {
            assert_eq!(baud_rate, 115_200);
            assert_eq!(request.operation, ByteIoOperation::Open);
            assert!(request.interruption().is_none());
            Ok(Box::new(CompletionIo {
                boundary: Arc::clone(&self.boundary),
            }))
        }
    }

    struct CompletionIo {
        boundary: Arc<CompletionBoundary>,
    }

    impl ByteIo for CompletionIo {
        fn read(
            &mut self,
            buffer: &mut [u8],
            request: ByteIoRequest,
        ) -> Result<usize, SerialError> {
            if request.operation != ByteIoOperation::HandshakeRead || buffer.is_empty() {
                return Err(SerialError::new(
                    SerialErrorKind::Protocol,
                    "scripted Windows completion received an unexpected read",
                ));
            }
            buffer[0] = HANDSHAKE_REPLY;
            Ok(1)
        }

        fn write(&mut self, buffer: &[u8], request: ByteIoRequest) -> Result<usize, SerialError> {
            match request.operation {
                ByteIoOperation::HandshakeWrite => {
                    assert_eq!(buffer, HANDSHAKE_REQUEST);
                    Ok(buffer.len())
                }
                ByteIoOperation::ControllerWrite(context) if context.kind == WriteKind::Report => {
                    let boundary = Arc::clone(&self.boundary);
                    let wake_boundary = Arc::clone(&boundary);
                    let _cancel_wake = request
                        .cancellation
                        .on_cancel_scoped(move || wake_boundary.changed.notify_all());
                    let mut state = boundary.state.lock().expect("completion boundary lock");
                    state.full_completion_waiting = true;
                    boundary.changed.notify_all();
                    while !state.allow_completion_consumption
                        || !request.cancellation.is_cancelled()
                    {
                        if request.cancellation.is_cancelled() && !state.interrupt_observed {
                            state.interrupt_observed = true;
                            boundary.changed.notify_all();
                        }
                        state = boundary
                            .changed
                            .wait(state)
                            .expect("completion boundary wait");
                    }
                    state.interrupt_observed = true;
                    boundary.changed.notify_all();
                    drop(state);

                    let interruption = request
                        .interruption()
                        .expect("generation release requests the Windows interrupt");
                    let transferred = settle_cancelled_completion(
                        cancel_request_error(ERROR_NOT_FOUND),
                        Ok(u32::try_from(buffer.len()).expect("test report fits u32")),
                        interruption,
                    )?;
                    {
                        let mut state = boundary.state.lock().expect("completion boundary lock");
                        state.cancel_not_found_consumed = true;
                    }
                    let transferred = usize::try_from(transferred).expect("u32 fits usize");
                    request.publish_final_write_acceptance(transferred)?;
                    let mut state = boundary.state.lock().expect("completion boundary lock");
                    state.report_writes += 1;
                    boundary.changed.notify_all();
                    Ok(transferred)
                }
                ByteIoOperation::ControllerWrite(context)
                    if context.kind == WriteKind::Neutralize =>
                {
                    request.publish_final_write_acceptance(buffer.len())?;
                    let mut state = self
                        .boundary
                        .state
                        .lock()
                        .expect("completion boundary lock");
                    state.neutral_writes += 1;
                    self.boundary.changed.notify_all();
                    Ok(buffer.len())
                }
                _ => Err(SerialError::new(
                    SerialErrorKind::Protocol,
                    "scripted Windows completion received an unexpected write",
                )),
            }
        }

        fn discard_input(&mut self, _request: ByteIoRequest) -> Result<(), SerialError> {
            Ok(())
        }

        fn close(&mut self) {
            self.boundary.changed.notify_all();
        }
    }

    struct ChannelWake(mpsc::SyncSender<()>);

    impl Wake for ChannelWake {
        fn wake(self: Arc<Self>) {
            let _ = self.0.try_send(());
        }

        fn wake_by_ref(self: &Arc<Self>) {
            let _ = self.0.try_send(());
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
        let waker = Waker::from(Arc::new(ChannelWake(wake_sender)));
        let mut context = Context::from_waker(&waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => wake_receiver
                    .recv_timeout(TEST_WAIT)
                    .expect("future did not wake before the bounded test deadline"),
            }
        }
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
        let waker = Waker::from(Arc::new(ChannelWake(wake_sender)));
        let mut context = Context::from_waker(&waker);
        future.poll(&mut context)
    }

    fn wait_terminal(operation: &Operation) {
        assert!(matches!(
            operation.wait(WaitTimeout::For(TEST_WAIT)),
            WaitResult::Completed(_)
        ));
    }

    // conformance: phase2a.serial.safe-open-path
    #[test]
    fn port_paths_are_canonical_and_do_not_open_arbitrary_devices() {
        assert_eq!(
            String::from_utf16_lossy(&port_path("COM12").expect("COM path")),
            "\\\\.\\COM12\0"
        );
        assert!(port_path(r"\\.\com4").is_ok());
        assert_eq!(
            port_path("COM-test").expect_err("invalid port").kind(),
            SerialErrorKind::InvalidPort
        );
        assert!(port_path(r"\\.\PhysicalDrive0").is_err());
    }

    #[test]
    fn deadline_wait_rounds_up_without_using_infinite_for_finite_values() {
        assert_eq!(duration_to_wait_ms(Duration::ZERO), 0);
        assert_eq!(duration_to_wait_ms(Duration::from_nanos(1)), 1);
        assert_eq!(duration_to_wait_ms(Duration::from_millis(1)), 1);
        assert_eq!(duration_to_wait_ms(Duration::MAX), INFINITE - 1);
    }

    // conformance: phase2a.serial.abort-causality
    #[test]
    fn explicit_interruption_overrides_only_a_native_aborted_completion() {
        let aborted = from_code("read", ERROR_OPERATION_ABORTED);
        let cancelled = SerialError::new(SerialErrorKind::Cancelled, "operation cancelled");
        assert_eq!(
            prefer_causal_interruption(aborted.clone(), Some(cancelled)).kind(),
            SerialErrorKind::Cancelled
        );
        assert_eq!(
            prefer_causal_interruption(aborted, None).kind(),
            SerialErrorKind::Disconnected
        );

        let io = SerialError::new(SerialErrorKind::Io, "unrelated failure");
        let deadline = SerialError::new(SerialErrorKind::DeadlineExceeded, "deadline");
        assert_eq!(
            prefer_causal_interruption(io, Some(deadline)).kind(),
            SerialErrorKind::Io
        );
    }

    // conformance: controller.lease.generation-release-windows-full-completion
    #[test]
    fn generation_release_preserves_windows_full_completion_after_cancel_not_found() {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let boundary = Arc::new(CompletionBoundary::default());
        let port = SerialPortDescriptor::new("ROOT\\TEST\\R", "COM1").expect("test port");
        let transport = SerialControllerTransport::new(
            clock.clone(),
            port,
            Box::new(CompletionFactory {
                boundary: Arc::clone(&boundary),
            }),
        );
        let controller = ControllerSession::new(
            &runtime,
            Box::new(transport),
            ControllerOptions {
                minimum_report_interval_ns: 1,
                ..ControllerOptions::default()
            },
        )
        .expect("controller");
        let connect = controller
            .connect(ConnectOptions::default())
            .expect("connect");
        wait_terminal(&connect);
        assert_eq!(connect.snapshot().state, OperationState::Succeeded);

        let cancellation = CancellationToken::root();
        let lease = match block_on(controller.acquire_automation_lease(&cancellation, None)) {
            AutomationLeaseAcquireOutcome::Granted(lease) => lease,
            _ => panic!("Automation lease was not granted"),
        };
        let action = controller
            .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
            .expect("generation action");
        boundary.wait_for(
            |state| state.full_completion_waiting,
            "backend did not reach the full Windows completion boundary",
        );

        let mut release = pin!(lease.neutralize_and_release());
        boundary.wait_for(
            |state| state.interrupt_observed,
            "generation release did not request the Windows interrupt",
        );
        let sealed_snapshot = action.snapshot();
        let release_was_pending = matches!(poll_once(release.as_mut()), Poll::Pending);
        boundary.allow_completion_consumption();

        wait_terminal(&action);
        let action_snapshot = action.snapshot();
        clock.advance_to(1);
        let release_outcome = block_on(release.as_mut());
        let before_close = boundary.counts();
        clock.advance_to(2);
        controller.close();
        let runtime_close = runtime.close();
        let after_close = boundary.counts();

        assert_eq!(sealed_snapshot.state, OperationState::Running);
        assert_eq!(
            sealed_snapshot.cancellation_reason,
            Some(CancellationReason::ParentClose)
        );
        assert!(
            release_was_pending,
            "release remains nonterminal until backend settlement"
        );
        assert_eq!(action_snapshot.state, OperationState::Succeeded);
        assert_eq!(
            action_snapshot.cancellation_reason,
            Some(CancellationReason::ParentClose)
        );
        assert_eq!(
            release_outcome,
            Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
        );
        assert_eq!(before_close, (1, 1, true));
        assert_eq!(after_close, (1, 2, true));
        assert_eq!(runtime_close, Ok(CloseOutcome::Closed));
    }

    #[cfg(debug_assertions)]
    // conformance: controller.lease.admission-resolution-windows-full-completion
    #[test]
    fn admission_resolution_preserves_outstanding_windows_full_completion_after_seal() {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let boundary = Arc::new(CompletionBoundary::default());
        let port = SerialPortDescriptor::new("ROOT\\TEST\\ADMISSION", "COM1").expect("test port");
        let transport = SerialControllerTransport::new(
            clock.clone(),
            port,
            Box::new(CompletionFactory {
                boundary: Arc::clone(&boundary),
            }),
        );
        let controller = ControllerSession::new(
            &runtime,
            Box::new(transport),
            ControllerOptions {
                minimum_report_interval_ns: 1,
                ..ControllerOptions::default()
            },
        )
        .expect("controller");
        let connect = controller
            .connect(ConnectOptions::default())
            .expect("connect");
        wait_terminal(&connect);
        assert_eq!(connect.snapshot().state, OperationState::Succeeded);

        let cancellation = CancellationToken::root();
        let lease = match block_on(controller.acquire_automation_lease(&cancellation, None)) {
            AutomationLeaseAcquireOutcome::Granted(lease) => lease,
            _ => panic!("Automation lease was not granted"),
        };
        let hook_boundary = Arc::clone(&boundary);
        lease.install_admission_resolution_hook_for_test(Arc::new(move || {
            hook_boundary.admission_resolution_hook();
        }));

        let (submitted, sealed_snapshot, release_was_pending, release_outcome) =
            std::thread::scope(|scope| {
                let submit_controller = controller.clone();
                let submit_lease = &lease;
                let submit = scope.spawn(move || {
                    submit_controller
                        .direct_with_lease(submit_lease, ControllerAction::ButtonDown(Button::A))
                });
                boundary.wait_for(
                    |state| state.admission_resolution_waiting,
                    "submitter did not stop before generation admission resolution",
                );
                boundary.wait_for(
                    |state| state.full_completion_waiting,
                    "backend did not reach the full Windows completion boundary",
                );

                let mut release = pin!(lease.request_cleanup_for_test());
                boundary.allow_admission_resolution();
                let submitted = submit.join().expect("Automation admission thread");
                boundary.wait_for(
                    |state| state.interrupt_observed,
                    "post-seal admission did not request the Windows interrupt",
                );
                let sealed_snapshot = submitted.as_ref().ok().map(Operation::snapshot);
                let release_was_pending = matches!(poll_once(release.as_mut()), Poll::Pending);
                boundary.allow_completion_consumption();

                if let Ok(action) = &submitted {
                    wait_terminal(action);
                }
                clock.advance_to(1);
                let release_outcome = block_on(release.as_mut());
                (
                    submitted,
                    sealed_snapshot,
                    release_was_pending,
                    release_outcome,
                )
            });

        let action = submitted.expect(
            "backend dispatch must make the post-seal admission return its truthful operation",
        );
        let action_snapshot = action.snapshot();
        let before_close = boundary.counts();
        clock.advance_to(2);
        controller.close();
        let runtime_close = runtime.close();
        let after_close = boundary.counts();

        let sealed_snapshot = sealed_snapshot.expect("dispatched admission returns an operation");
        assert_eq!(sealed_snapshot.state, OperationState::Running);
        assert_eq!(
            sealed_snapshot.cancellation_reason,
            Some(CancellationReason::ParentClose)
        );
        assert!(
            release_was_pending,
            "release remains nonterminal until backend settlement"
        );
        assert_eq!(action_snapshot.state, OperationState::Succeeded);
        assert_eq!(
            action_snapshot.cancellation_reason,
            Some(CancellationReason::ParentClose)
        );
        assert_eq!(
            release_outcome,
            Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
        );
        assert_eq!(before_close, (1, 1, true));
        assert_eq!(after_close, (1, 2, true));
        assert_eq!(runtime_close, Ok(CloseOutcome::Closed));
    }

    // conformance: controller.lease.serial-cancel-not-found
    #[test]
    fn cancel_not_found_still_consumes_a_full_overlapped_completion() {
        let interruption = SerialError::new(SerialErrorKind::Cancelled, "close requested");
        assert!(cancel_request_error(ERROR_NOT_FOUND).is_none());
        assert_eq!(
            settle_cancelled_completion(cancel_request_error(ERROR_NOT_FOUND), Ok(8), interruption)
                .expect("final eight-byte completion wins over late cancellation"),
            8
        );
    }

    // conformance: phase2a.serial.unwind-settle
    #[test]
    fn unwind_cleanup_runs_before_the_original_panic_is_resumed() {
        let cleanup_ran = std::cell::Cell::new(false);
        let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let payload =
                std::panic::catch_unwind(|| panic!("clock panic")).expect_err("test panic payload");
            resume_unwind_after_cleanup(payload, || cleanup_ran.set(true));
        }));

        assert!(resumed.is_err());
        assert!(cleanup_ran.get());
    }

    #[test]
    fn invalid_descriptor_is_rejected_before_a_system_open() {
        let descriptor =
            SerialPortDescriptor::new("ROOT\\TEST\\0", "COM-test").expect("structured descriptor");
        let clock = Arc::new(VirtualClock::new(1));
        let request = ByteIoRequest {
            operation: ByteIoOperation::Open,
            clock,
            deadline_ns: 2,
            cancellation: CancellationToken::root(),
            resource_cancellation: CancellationToken::root(),
            final_write_settlement: None,
        };

        let error = match WindowsByteIoFactory.open(&descriptor, 115_200, request) {
            Ok(_) => panic!("invalid name unexpectedly opened"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), SerialErrorKind::InvalidPort);
        assert_eq!(error.os_code(), None);
    }

    // conformance: phase2a.serial.win32-close-wake
    #[test]
    fn close_signal_is_idempotent_and_wakes_waiters() {
        let shared = SharedPort {
            handle: OwnedHandle::auto_event().expect("stand-in owned handle"),
            close_event: OwnedHandle::manual_event().expect("close event"),
            closed: AtomicBool::new(false),
        };

        shared.close();
        shared.close();

        assert!(shared.closed.load(Ordering::Acquire));
        // SAFETY: close_event is a live event HANDLE owned by shared for the full zero-time wait.
        assert_eq!(
            unsafe {
                windows_sys::Win32::System::Threading::WaitForSingleObject(
                    shared.close_event.raw(),
                    0,
                )
            },
            WAIT_OBJECT_0
        );
    }
}
