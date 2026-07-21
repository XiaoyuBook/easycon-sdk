use std::ffi::{c_char, c_void};

pub(crate) const STATUS_OK: i32 = 0;
pub(crate) const STATUS_INVALID_ARGUMENT: i32 = 1;
pub(crate) const STATUS_OUT_OF_RANGE: i32 = 2;
pub(crate) const STATUS_OVERFLOW: i32 = 3;
pub(crate) const STATUS_RESOURCE_EXHAUSTED: i32 = 4;
pub(crate) const STATUS_NOT_FOUND: i32 = 5;
pub(crate) const STATUS_MODEL_NOT_FOUND: i32 = 6;
pub(crate) const STATUS_INVALID_IMAGE: i32 = 7;
pub(crate) const STATUS_NO_FRAME: i32 = 8;
pub(crate) const STATUS_CANCELLED: i32 = 9;
pub(crate) const STATUS_BACKEND_ERROR: i32 = 10;
pub(crate) const STATUS_CV_EXCEPTION: i32 = 11;
pub(crate) const STATUS_STD_EXCEPTION: i32 = 12;
pub(crate) const STATUS_UNKNOWN_EXCEPTION: i32 = 13;
pub(crate) const STATUS_ALLOCATION_FAILED: i32 = 14;
pub(crate) const STATUS_INTERNAL: i32 = 15;

#[repr(C)]
#[derive(Default)]
pub(crate) struct Error {
    pub(crate) code: i32,
    pub(crate) data: *mut c_char,
    pub(crate) length: u64,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct Counts {
    pub(crate) live_handles: u64,
    pub(crate) live_allocations: u64,
}

pub(crate) type DebugHandle = c_void;

unsafe extern "C" {
    pub(crate) fn easycon_native_error_release(error: *mut Error);
    pub(crate) fn easycon_native_debug_counts(
        out_counts: *mut Counts,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_debug_handle_create(
        out_handle: *mut *mut DebugHandle,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_debug_handle_destroy(
        inout_handle: *mut *mut DebugHandle,
        out_error: *mut Error,
    ) -> i32;
}
