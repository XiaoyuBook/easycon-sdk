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

pub(crate) const PIXEL_FORMAT_BGR8: u32 = 1;
pub(crate) const PIXEL_FORMAT_BGRA8: u32 = 2;
pub(crate) const PIXEL_FORMAT_GRAY8: u32 = 3;

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

#[repr(C)]
#[derive(Default)]
pub(crate) struct Buffer {
    pub(crate) data: *mut u8,
    pub(crate) length: u64,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct ImageView {
    pub(crate) data: *const u8,
    pub(crate) length: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) stride: u64,
    pub(crate) pixel_format: u32,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct Image {
    pub(crate) data: *mut u8,
    pub(crate) length: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) stride: u64,
    pub(crate) pixel_format: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct ImageLimits {
    pub(crate) max_encoded_bytes: u64,
    pub(crate) max_width: u32,
    pub(crate) max_height: u32,
    pub(crate) max_pixels: u64,
    pub(crate) max_decoded_bytes: u64,
    pub(crate) max_stride: u64,
}

pub(crate) type DebugHandle = c_void;

unsafe extern "C" {
    pub(crate) fn easycon_native_error_release(error: *mut Error);
    pub(crate) fn easycon_native_buffer_release(buffer: *mut Buffer);
    pub(crate) fn easycon_native_image_release(image: *mut Image);
    pub(crate) fn easycon_native_image_decode(
        encoded: *const u8,
        encoded_length: u64,
        limits: *const ImageLimits,
        out_image: *mut Image,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_image_encode_png(
        image: *const ImageView,
        limits: *const ImageLimits,
        out_encoded: *mut Buffer,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_image_convert(
        image: *const ImageView,
        output_format: u32,
        limits: *const ImageLimits,
        out_image: *mut Image,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_image_crop(
        image: *const ImageView,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        limits: *const ImageLimits,
        out_image: *mut Image,
        out_error: *mut Error,
    ) -> i32;
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

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::{Buffer, Error, Image, ImageLimits, ImageView};

    #[test]
    fn private_image_layout_matches_the_x64_header() {
        assert_eq!(size_of::<Error>(), 24);
        assert_eq!(size_of::<Buffer>(), 16);
        assert_eq!(size_of::<ImageView>(), 40);
        assert_eq!(size_of::<Image>(), 40);
        assert_eq!(size_of::<ImageLimits>(), 40);
        assert_eq!(offset_of!(Image, stride), 24);
        assert_eq!(offset_of!(ImageLimits, max_pixels), 16);
    }
}
