use std::ptr::NonNull;

use crate::ffi;
use crate::{NativeError, NativeErrorKind, NativeResourceCounts};

pub(super) mod image;
pub(super) mod ocr;
pub(super) mod vision;

pub(crate) fn counts() -> Result<NativeResourceCounts, NativeError> {
    let mut counts = ffi::Counts::default();
    let mut error = ffi::Error::default();
    // SAFETY: both out pointers refer to initialized, uniquely borrowed repr(C) values for the call.
    let status = unsafe { ffi::easycon_native_debug_counts(&mut counts, &mut error) };
    finish(status, error)?;
    Ok(NativeResourceCounts {
        live_handles: counts.live_handles,
        live_allocations: counts.live_allocations,
    })
}

pub(crate) fn create_debug_handle() -> Result<NonNull<ffi::DebugHandle>, NativeError> {
    let mut handle = std::ptr::null_mut();
    let mut error = ffi::Error::default();
    // SAFETY: out_handle and out_error are valid unique out pointers and native zeroes failure outputs.
    let status = unsafe { ffi::easycon_native_debug_handle_create(&mut handle, &mut error) };
    finish(status, error)?;
    NonNull::new(handle)
        .ok_or_else(|| NativeError::internal("native create returned a null handle"))
}

pub(crate) fn destroy_debug_handle(handle: NonNull<ffi::DebugHandle>) {
    let mut raw = handle.as_ptr();
    let mut error = ffi::Error::default();
    // SAFETY: raw is the unique native owner and both mutable pointers remain valid for the call.
    let status = unsafe { ffi::easycon_native_debug_handle_destroy(&mut raw, &mut error) };
    let result = finish(status, error);
    debug_assert!(result.is_ok(), "debug handle destroy must be infallible");
    debug_assert!(raw.is_null(), "debug handle destroy must consume its owner");
}

pub(super) fn finish(status: i32, error: ffi::Error) -> Result<(), NativeError> {
    let owned = ErrorOwner { raw: error };
    if status == ffi::STATUS_OK {
        if owned.raw.code != 0 || !owned.raw.data.is_null() || owned.raw.length != 0 {
            return Err(NativeError::internal(
                "native success returned a non-empty error payload",
            ));
        }
        return Ok(());
    }

    let kind = NativeErrorKind::from_status(status);
    if owned.raw.code != 0 && owned.raw.code != status {
        return Err(NativeError::internal(
            "native error code does not match returned status",
        ));
    }
    let message = owned
        .message()
        .unwrap_or_else(|| "native call failed".to_owned());
    Err(NativeError { kind, message })
}

struct ErrorOwner {
    raw: ffi::Error,
}

impl ErrorOwner {
    fn message(&self) -> Option<String> {
        if self.raw.data.is_null() {
            return (self.raw.length == 0).then(String::new);
        }
        let length = usize::try_from(self.raw.length).ok()?;
        // SAFETY: native owns a readable allocation of exactly length bytes until this guard drops.
        let bytes = unsafe { std::slice::from_raw_parts(self.raw.data.cast::<u8>(), length) };
        String::from_utf8(bytes.to_vec()).ok()
    }
}

impl Drop for ErrorOwner {
    fn drop(&mut self) {
        // SAFETY: this guard is the unique owner of the bridge error allocation.
        unsafe { ffi::easycon_native_error_release(&mut self.raw) };
    }
}
