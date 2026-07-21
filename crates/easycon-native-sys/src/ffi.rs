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

pub(crate) const TEMPLATE_SQDIFF_NORMED: u32 = 1;
pub(crate) const TEMPLATE_CCORR_NORMED: u32 = 2;
pub(crate) const TEMPLATE_CCOEFF_NORMED: u32 = 3;

pub(crate) const EDGE_XY: u32 = 1;
pub(crate) const EDGE_LAPLACIAN: u32 = 2;

pub(crate) const OCR_ENGINE_DEFAULT: u32 = 1;
pub(crate) const OCR_ENGINE_LSTM_ONLY: u32 = 2;

pub(crate) const OCR_PSM_AUTO: u32 = 1;
pub(crate) const OCR_PSM_SINGLE_BLOCK: u32 = 2;
pub(crate) const OCR_PSM_SINGLE_LINE: u32 = 3;
pub(crate) const OCR_PSM_SINGLE_WORD: u32 = 4;

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

#[repr(C)]
#[derive(Default)]
pub(crate) struct MatchExtrema {
    pub(crate) min_value: f64,
    pub(crate) max_value: f64,
    pub(crate) min_x: i32,
    pub(crate) min_y: i32,
    pub(crate) max_x: i32,
    pub(crate) max_y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct HsvRange {
    pub(crate) h_min: u32,
    pub(crate) h_max: u32,
    pub(crate) s_min: u32,
    pub(crate) s_max: u32,
    pub(crate) v_min: u32,
    pub(crate) v_max: u32,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct ColorResult {
    pub(crate) count: u64,
    pub(crate) bbox_x: u32,
    pub(crate) bbox_y: u32,
    pub(crate) bbox_width: u32,
    pub(crate) bbox_height: u32,
    pub(crate) has_bbox: u32,
}

pub(crate) type DebugHandle = c_void;
pub(crate) type OcrEngine = c_void;

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
    pub(crate) fn easycon_native_match_template(
        search: *const ImageView,
        target: *const ImageView,
        method: u32,
        limits: *const ImageLimits,
        out_result: *mut MatchExtrema,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_edge_preprocess(
        image: *const ImageView,
        method: u32,
        limits: *const ImageLimits,
        out_image: *mut Image,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_hsv_count(
        image: *const ImageView,
        roi_x: u32,
        roi_y: u32,
        roi_width: u32,
        roi_height: u32,
        range: *const HsvRange,
        limits: *const ImageLimits,
        out_result: *mut ColorResult,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_ocr_engine_create(
        model_root_utf8: *const u8,
        model_root_length: u64,
        language_utf8: *const u8,
        language_length: u64,
        engine_mode: u32,
        out_engine: *mut *mut OcrEngine,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_ocr_engine_process(
        engine: *mut OcrEngine,
        image: *const ImageView,
        page_segmentation: u32,
        max_output_bytes: u64,
        out_text: *mut Buffer,
        out_confidence: *mut f64,
        out_error: *mut Error,
    ) -> i32;
    pub(crate) fn easycon_native_ocr_engine_destroy(
        inout_engine: *mut *mut OcrEngine,
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
    use std::mem::{align_of, offset_of, size_of};

    use super::{
        Buffer, ColorResult, Counts, Error, HsvRange, Image, ImageLimits, ImageView, MatchExtrema,
    };

    #[test]
    fn private_image_layout_matches_the_x64_header() {
        assert_eq!(size_of::<Error>(), 24);
        assert_eq!(align_of::<Error>(), 8);
        assert_eq!(offset_of!(Error, code), 0);
        assert_eq!(offset_of!(Error, data), 8);
        assert_eq!(offset_of!(Error, length), 16);
        assert_eq!(size_of::<Buffer>(), 16);
        assert_eq!(align_of::<Buffer>(), 8);
        assert_eq!(offset_of!(Buffer, data), 0);
        assert_eq!(offset_of!(Buffer, length), 8);
        assert_eq!(size_of::<ImageView>(), 40);
        assert_eq!(align_of::<ImageView>(), 8);
        assert_eq!(offset_of!(ImageView, data), 0);
        assert_eq!(offset_of!(ImageView, length), 8);
        assert_eq!(offset_of!(ImageView, width), 16);
        assert_eq!(offset_of!(ImageView, height), 20);
        assert_eq!(offset_of!(ImageView, stride), 24);
        assert_eq!(offset_of!(ImageView, pixel_format), 32);
        assert_eq!(size_of::<Image>(), 40);
        assert_eq!(align_of::<Image>(), 8);
        assert_eq!(offset_of!(Image, data), 0);
        assert_eq!(offset_of!(Image, length), 8);
        assert_eq!(offset_of!(Image, width), 16);
        assert_eq!(offset_of!(Image, height), 20);
        assert_eq!(size_of::<ImageLimits>(), 40);
        assert_eq!(offset_of!(Image, stride), 24);
        assert_eq!(offset_of!(Image, pixel_format), 32);
        assert_eq!(align_of::<ImageLimits>(), 8);
        assert_eq!(offset_of!(ImageLimits, max_encoded_bytes), 0);
        assert_eq!(offset_of!(ImageLimits, max_width), 8);
        assert_eq!(offset_of!(ImageLimits, max_height), 12);
        assert_eq!(offset_of!(ImageLimits, max_pixels), 16);
        assert_eq!(offset_of!(ImageLimits, max_decoded_bytes), 24);
        assert_eq!(offset_of!(ImageLimits, max_stride), 32);
        assert_eq!(size_of::<MatchExtrema>(), 32);
        assert_eq!(align_of::<MatchExtrema>(), 8);
        assert_eq!(offset_of!(MatchExtrema, min_value), 0);
        assert_eq!(offset_of!(MatchExtrema, max_value), 8);
        assert_eq!(offset_of!(MatchExtrema, min_x), 16);
        assert_eq!(offset_of!(MatchExtrema, min_y), 20);
        assert_eq!(offset_of!(MatchExtrema, max_x), 24);
        assert_eq!(offset_of!(MatchExtrema, max_y), 28);
        assert_eq!(size_of::<HsvRange>(), 24);
        assert_eq!(align_of::<HsvRange>(), 4);
        assert_eq!(offset_of!(HsvRange, h_min), 0);
        assert_eq!(offset_of!(HsvRange, h_max), 4);
        assert_eq!(offset_of!(HsvRange, s_min), 8);
        assert_eq!(offset_of!(HsvRange, s_max), 12);
        assert_eq!(offset_of!(HsvRange, v_min), 16);
        assert_eq!(offset_of!(HsvRange, v_max), 20);
        assert_eq!(size_of::<ColorResult>(), 32);
        assert_eq!(align_of::<ColorResult>(), 8);
        assert_eq!(offset_of!(ColorResult, count), 0);
        assert_eq!(offset_of!(ColorResult, bbox_x), 8);
        assert_eq!(offset_of!(ColorResult, bbox_y), 12);
        assert_eq!(offset_of!(ColorResult, bbox_width), 16);
        assert_eq!(offset_of!(ColorResult, bbox_height), 20);
        assert_eq!(offset_of!(ColorResult, has_bbox), 24);
        assert_eq!(size_of::<Counts>(), 16);
        assert_eq!(align_of::<Counts>(), 8);
        assert_eq!(offset_of!(Counts, live_handles), 0);
        assert_eq!(offset_of!(Counts, live_allocations), 8);
    }
}
