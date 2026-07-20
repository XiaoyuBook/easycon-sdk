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
            return normalize_progress(overlapped_result(
                &self.shared,
                overlapped.as_mut(),
                operation,
                false,
            )?);
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
        normalize_progress(transferred)
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
        (code != ERROR_NOT_FOUND).then(|| from_code("CancelIoEx(serial I/O)", code))
    } else {
        None
    };
    match overlapped_result(shared, overlapped, operation, true) {
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
    let valid = name.len() <= 64
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
    use easycon_runtime::{CancellationToken, VirtualClock};

    use super::*;
    use crate::ByteIoOperation;

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
