use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE,
    ERROR_INVALID_HANDLE, ERROR_OPERATION_ABORTED, ERROR_PATH_NOT_FOUND, ERROR_SEM_TIMEOUT,
    ERROR_SHARING_VIOLATION,
};

use crate::{SerialError, SerialErrorKind};

pub(super) fn last_error(operation: &'static str) -> SerialError {
    // SAFETY: GetLastError has no pointer or ownership preconditions and is sampled immediately
    // after the failed Win32 call on the same thread.
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    from_code(operation, code)
}

pub(super) fn from_code(operation: &'static str, code: u32) -> SerialError {
    let kind = match code {
        ERROR_ACCESS_DENIED => SerialErrorKind::AccessDenied,
        ERROR_SHARING_VIOLATION => SerialErrorKind::PortBusy,
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => SerialErrorKind::NotFound,
        ERROR_DEVICE_NOT_CONNECTED
        | ERROR_GEN_FAILURE
        | ERROR_INVALID_HANDLE
        | ERROR_OPERATION_ABORTED => SerialErrorKind::Disconnected,
        ERROR_SEM_TIMEOUT => SerialErrorKind::DeadlineExceeded,
        _ => SerialErrorKind::Io,
    };
    SerialError::with_os_code(kind, format!("{operation} failed"), code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_open_and_disconnect_codes_have_stable_categories() {
        assert_eq!(
            from_code("open", ERROR_ACCESS_DENIED).kind(),
            SerialErrorKind::AccessDenied
        );
        assert_eq!(
            from_code("open", ERROR_SHARING_VIOLATION).kind(),
            SerialErrorKind::PortBusy
        );
        assert_eq!(
            from_code("read", ERROR_DEVICE_NOT_CONNECTED).kind(),
            SerialErrorKind::Disconnected
        );
        assert_eq!(
            from_code("read", ERROR_OPERATION_ABORTED).kind(),
            SerialErrorKind::Disconnected
        );
    }
}
