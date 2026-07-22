#ifndef EASYCON_NATIVE_BRIDGE_H
#define EASYCON_NATIVE_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define EASYCON_NATIVE_CALL __cdecl
#else
#define EASYCON_NATIVE_CALL
#endif

#if defined(__cplusplus)
#define EASYCON_NATIVE_NOEXCEPT noexcept
extern "C" {
#else
#define EASYCON_NATIVE_NOEXCEPT
#endif

typedef int32_t easycon_native_status;

#define EASYCON_NATIVE_STATUS_OK INT32_C(0)
#define EASYCON_NATIVE_STATUS_INVALID_ARGUMENT INT32_C(1)
#define EASYCON_NATIVE_STATUS_OUT_OF_RANGE INT32_C(2)
#define EASYCON_NATIVE_STATUS_OVERFLOW INT32_C(3)
#define EASYCON_NATIVE_STATUS_RESOURCE_EXHAUSTED INT32_C(4)
#define EASYCON_NATIVE_STATUS_NOT_FOUND INT32_C(5)
#define EASYCON_NATIVE_STATUS_MODEL_NOT_FOUND INT32_C(6)
#define EASYCON_NATIVE_STATUS_INVALID_IMAGE INT32_C(7)
#define EASYCON_NATIVE_STATUS_NO_FRAME INT32_C(8)
#define EASYCON_NATIVE_STATUS_CANCELLED INT32_C(9)
#define EASYCON_NATIVE_STATUS_BACKEND_ERROR INT32_C(10)
#define EASYCON_NATIVE_STATUS_CV_EXCEPTION INT32_C(11)
#define EASYCON_NATIVE_STATUS_STD_EXCEPTION INT32_C(12)
#define EASYCON_NATIVE_STATUS_UNKNOWN_EXCEPTION INT32_C(13)
#define EASYCON_NATIVE_STATUS_ALLOCATION_FAILED INT32_C(14)
#define EASYCON_NATIVE_STATUS_INTERNAL INT32_C(15)
#define EASYCON_NATIVE_STATUS_UNSUPPORTED INT32_C(16)

#define EASYCON_NATIVE_PIXEL_FORMAT_BGR8 UINT32_C(1)
#define EASYCON_NATIVE_PIXEL_FORMAT_BGRA8 UINT32_C(2)
#define EASYCON_NATIVE_PIXEL_FORMAT_GRAY8 UINT32_C(3)

#define EASYCON_NATIVE_TEMPLATE_SQDIFF_NORMED UINT32_C(1)
#define EASYCON_NATIVE_TEMPLATE_CCORR_NORMED UINT32_C(2)
#define EASYCON_NATIVE_TEMPLATE_CCOEFF_NORMED UINT32_C(3)

#define EASYCON_NATIVE_EDGE_XY UINT32_C(1)
#define EASYCON_NATIVE_EDGE_LAPLACIAN UINT32_C(2)

#define EASYCON_NATIVE_OCR_ENGINE_DEFAULT UINT32_C(1)
#define EASYCON_NATIVE_OCR_ENGINE_LSTM_ONLY UINT32_C(2)

#define EASYCON_NATIVE_OCR_PSM_AUTO UINT32_C(1)
#define EASYCON_NATIVE_OCR_PSM_SINGLE_BLOCK UINT32_C(2)
#define EASYCON_NATIVE_OCR_PSM_SINGLE_LINE UINT32_C(3)
#define EASYCON_NATIVE_OCR_PSM_SINGLE_WORD UINT32_C(4)

#define EASYCON_NATIVE_CAPTURE_BACKEND_FILE UINT32_C(1)
#define EASYCON_NATIVE_CAPTURE_BACKEND_DIRECTSHOW UINT32_C(2)
#define EASYCON_NATIVE_CAPTURE_BACKEND_MEDIA_FOUNDATION UINT32_C(3)
#define EASYCON_NATIVE_CAPTURE_BACKEND_V4L2 UINT32_C(4)

typedef struct easycon_native_error {
    int32_t code;
    char* data;
    uint64_t length;
} easycon_native_error;

typedef struct easycon_native_buffer {
    uint8_t* data;
    uint64_t length;
} easycon_native_buffer;

typedef struct easycon_native_image_view {
    const uint8_t* data;
    uint64_t length;
    uint32_t width;
    uint32_t height;
    uint64_t stride;
    uint32_t pixel_format;
} easycon_native_image_view;

typedef struct easycon_native_image {
    uint8_t* data;
    uint64_t length;
    uint32_t width;
    uint32_t height;
    uint64_t stride;
    uint32_t pixel_format;
} easycon_native_image;

typedef struct easycon_native_image_limits {
    uint64_t max_encoded_bytes;
    uint32_t max_width;
    uint32_t max_height;
    uint64_t max_pixels;
    uint64_t max_decoded_bytes;
    uint64_t max_stride;
} easycon_native_image_limits;

typedef struct easycon_native_match_extrema {
    double min_value;
    double max_value;
    int32_t min_x;
    int32_t min_y;
    int32_t max_x;
    int32_t max_y;
} easycon_native_match_extrema;

typedef struct easycon_native_hsv_range {
    uint32_t h_min;
    uint32_t h_max;
    uint32_t s_min;
    uint32_t s_max;
    uint32_t v_min;
    uint32_t v_max;
} easycon_native_hsv_range;

typedef struct easycon_native_color_result {
    uint64_t count;
    uint32_t bbox_x;
    uint32_t bbox_y;
    uint32_t bbox_width;
    uint32_t bbox_height;
    uint32_t has_bbox;
} easycon_native_color_result;

typedef struct easycon_native_counts {
    uint64_t live_handles;
    uint64_t live_allocations;
} easycon_native_counts;

typedef struct easycon_native_capture_options {
    easycon_native_image_limits image_limits;
    uint64_t open_timeout_ns;
    uint64_t read_timeout_ns;
    uint32_t max_frames;
    uint32_t reserved;
} easycon_native_capture_options;

typedef struct easycon_native_capture_profile {
    uint32_t backend;
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    uint64_t stride;
    uint64_t frame_interval_ns;
} easycon_native_capture_profile;

typedef struct easycon_native_debug_handle easycon_native_debug_handle;
typedef struct easycon_native_ocr_engine easycon_native_ocr_engine;
typedef struct easycon_native_capture easycon_native_capture;
typedef struct easycon_native_capture_interrupt easycon_native_capture_interrupt;
typedef struct easycon_native_capture_discovery easycon_native_capture_discovery;

void EASYCON_NATIVE_CALL easycon_native_error_release(
    easycon_native_error* error) EASYCON_NATIVE_NOEXCEPT;
void EASYCON_NATIVE_CALL easycon_native_buffer_release(
    easycon_native_buffer* buffer) EASYCON_NATIVE_NOEXCEPT;
void EASYCON_NATIVE_CALL easycon_native_image_release(
    easycon_native_image* image) EASYCON_NATIVE_NOEXCEPT;

easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_decode(
    const uint8_t* encoded,
    uint64_t encoded_length,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_encode_png(
    const easycon_native_image_view* image,
    const easycon_native_image_limits* limits,
    easycon_native_buffer* out_encoded,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_convert(
    const easycon_native_image_view* image,
    uint32_t output_format,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_crop(
    const easycon_native_image_view* image,
    uint32_t x,
    uint32_t y,
    uint32_t width,
    uint32_t height,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;

easycon_native_status EASYCON_NATIVE_CALL easycon_native_match_template(
    const easycon_native_image_view* search,
    const easycon_native_image_view* target,
    uint32_t method,
    const easycon_native_image_limits* limits,
    easycon_native_match_extrema* out_result,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_edge_preprocess(
    const easycon_native_image_view* image,
    uint32_t method,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_hsv_count(
    const easycon_native_image_view* image,
    uint32_t roi_x,
    uint32_t roi_y,
    uint32_t roi_width,
    uint32_t roi_height,
    const easycon_native_hsv_range* range,
    const easycon_native_image_limits* limits,
    easycon_native_color_result* out_result,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;

easycon_native_status EASYCON_NATIVE_CALL easycon_native_ocr_engine_create(
    const uint8_t* model_root_utf8,
    uint64_t model_root_length,
    const uint8_t* language_utf8,
    uint64_t language_length,
    uint32_t engine_mode,
    easycon_native_ocr_engine** out_engine,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_ocr_engine_process(
    easycon_native_ocr_engine* engine,
    const easycon_native_image_view* image,
    uint32_t page_segmentation,
    uint64_t max_output_bytes,
    easycon_native_buffer* out_text,
    double* out_confidence,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_ocr_engine_destroy(
    easycon_native_ocr_engine** inout_engine,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;

easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_create(
    uint32_t backend,
    const uint8_t* source_utf8,
    uint64_t source_length,
    const easycon_native_capture_options* options,
    easycon_native_capture** out_capture,
    easycon_native_capture_interrupt** out_interrupt,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_open(
    easycon_native_capture* capture,
    easycon_native_capture_profile* out_profile,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_read(
    easycon_native_capture* capture,
    easycon_native_image* out_image,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_close(
    easycon_native_capture* capture,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_destroy(
    easycon_native_capture** inout_capture,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_interrupt_request(
    easycon_native_capture_interrupt* interrupt,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_interrupt_destroy(
    easycon_native_capture_interrupt** inout_interrupt,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_create(
    uint32_t backend,
    easycon_native_capture_discovery** out_discovery,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_count(
    easycon_native_capture_discovery* discovery,
    uint32_t* out_count,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_get(
    easycon_native_capture_discovery* discovery,
    uint32_t index,
    easycon_native_buffer* out_source_id,
    easycon_native_buffer* out_display_name,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_destroy(
    easycon_native_capture_discovery** inout_discovery,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;

easycon_native_status EASYCON_NATIVE_CALL easycon_native_debug_counts(
    easycon_native_counts* out_counts,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_debug_handle_create(
    easycon_native_debug_handle** out_handle,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_debug_handle_destroy(
    easycon_native_debug_handle** inout_handle,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;

#if defined(EASYCON_NATIVE_TESTING)
#define EASYCON_NATIVE_TEST_RAISE_CV INT32_C(0)
#define EASYCON_NATIVE_TEST_RAISE_STD INT32_C(1)
#define EASYCON_NATIVE_TEST_RAISE_UNKNOWN INT32_C(2)
#define EASYCON_NATIVE_TEST_OCR_FAIL_MODEL_CHECK_BAD_ALLOC INT32_C(0)
#define EASYCON_NATIVE_TEST_OCR_FAIL_CONFIDENCE_STD INT32_C(1)
#define EASYCON_NATIVE_TEST_OCR_FAIL_DESTROY_UNKNOWN INT32_C(2)
#define EASYCON_NATIVE_TEST_CAPTURE_OPEN_CV INT32_C(0)
#define EASYCON_NATIVE_TEST_CAPTURE_READ_STD INT32_C(1)
#define EASYCON_NATIVE_TEST_CAPTURE_CLOSE_UNKNOWN INT32_C(2)
#define EASYCON_NATIVE_TEST_CAPTURE_DESTROY_BEFORE_CONSUME INT32_C(3)
#define EASYCON_NATIVE_TEST_CAPTURE_CONSUME_WITH_ERROR INT32_C(4)
#define EASYCON_NATIVE_TEST_CAPTURE_DESTROY_OK_NO_ACK INT32_C(5)

typedef struct easycon_native_ocr_test_counts {
    uint64_t created;
    uint64_t destroyed;
    uint64_t process_calls;
    uint64_t clear_calls;
    uint64_t teardown_exceptions;
} easycon_native_ocr_test_counts;

easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_raise(
    int32_t kind,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_raise(
    easycon_native_ocr_engine* engine,
    int32_t kind,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_counts(
    easycon_native_ocr_test_counts* out_counts,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_fail_next(
    int32_t failpoint,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_invalidate(
    easycon_native_ocr_engine* engine,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_capture_fail_next(
    int32_t failpoint,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
#endif

#if defined(__cplusplus)
}

static_assert(sizeof(void*) == 8, "Phase 3 native bridge is 64-bit-only");
static_assert(sizeof(easycon_native_error) == 24);
static_assert(alignof(easycon_native_error) == 8);
static_assert(offsetof(easycon_native_error, code) == 0);
static_assert(offsetof(easycon_native_error, data) == 8);
static_assert(offsetof(easycon_native_error, length) == 16);
static_assert(sizeof(easycon_native_buffer) == 16);
static_assert(alignof(easycon_native_buffer) == 8);
static_assert(offsetof(easycon_native_buffer, data) == 0);
static_assert(offsetof(easycon_native_buffer, length) == 8);
static_assert(sizeof(easycon_native_image_view) == 40);
static_assert(alignof(easycon_native_image_view) == 8);
static_assert(offsetof(easycon_native_image_view, data) == 0);
static_assert(offsetof(easycon_native_image_view, length) == 8);
static_assert(offsetof(easycon_native_image_view, width) == 16);
static_assert(offsetof(easycon_native_image_view, height) == 20);
static_assert(offsetof(easycon_native_image_view, stride) == 24);
static_assert(offsetof(easycon_native_image_view, pixel_format) == 32);
static_assert(sizeof(easycon_native_image) == 40);
static_assert(alignof(easycon_native_image) == 8);
static_assert(offsetof(easycon_native_image, data) == 0);
static_assert(offsetof(easycon_native_image, length) == 8);
static_assert(offsetof(easycon_native_image, width) == 16);
static_assert(offsetof(easycon_native_image, height) == 20);
static_assert(offsetof(easycon_native_image, stride) == 24);
static_assert(offsetof(easycon_native_image, pixel_format) == 32);
static_assert(sizeof(easycon_native_image_limits) == 40);
static_assert(alignof(easycon_native_image_limits) == 8);
static_assert(offsetof(easycon_native_image_limits, max_encoded_bytes) == 0);
static_assert(offsetof(easycon_native_image_limits, max_width) == 8);
static_assert(offsetof(easycon_native_image_limits, max_height) == 12);
static_assert(offsetof(easycon_native_image_limits, max_pixels) == 16);
static_assert(offsetof(easycon_native_image_limits, max_decoded_bytes) == 24);
static_assert(offsetof(easycon_native_image_limits, max_stride) == 32);
static_assert(sizeof(easycon_native_match_extrema) == 32);
static_assert(alignof(easycon_native_match_extrema) == 8);
static_assert(offsetof(easycon_native_match_extrema, min_value) == 0);
static_assert(offsetof(easycon_native_match_extrema, max_value) == 8);
static_assert(offsetof(easycon_native_match_extrema, min_x) == 16);
static_assert(offsetof(easycon_native_match_extrema, min_y) == 20);
static_assert(offsetof(easycon_native_match_extrema, max_x) == 24);
static_assert(offsetof(easycon_native_match_extrema, max_y) == 28);
static_assert(sizeof(easycon_native_hsv_range) == 24);
static_assert(alignof(easycon_native_hsv_range) == 4);
static_assert(offsetof(easycon_native_hsv_range, h_min) == 0);
static_assert(offsetof(easycon_native_hsv_range, h_max) == 4);
static_assert(offsetof(easycon_native_hsv_range, s_min) == 8);
static_assert(offsetof(easycon_native_hsv_range, s_max) == 12);
static_assert(offsetof(easycon_native_hsv_range, v_min) == 16);
static_assert(offsetof(easycon_native_hsv_range, v_max) == 20);
static_assert(sizeof(easycon_native_color_result) == 32);
static_assert(alignof(easycon_native_color_result) == 8);
static_assert(offsetof(easycon_native_color_result, count) == 0);
static_assert(offsetof(easycon_native_color_result, bbox_x) == 8);
static_assert(offsetof(easycon_native_color_result, bbox_y) == 12);
static_assert(offsetof(easycon_native_color_result, bbox_width) == 16);
static_assert(offsetof(easycon_native_color_result, bbox_height) == 20);
static_assert(offsetof(easycon_native_color_result, has_bbox) == 24);
static_assert(sizeof(easycon_native_counts) == 16);
static_assert(alignof(easycon_native_counts) == 8);
static_assert(offsetof(easycon_native_counts, live_handles) == 0);
static_assert(offsetof(easycon_native_counts, live_allocations) == 8);
static_assert(sizeof(easycon_native_capture_options) == 64);
static_assert(alignof(easycon_native_capture_options) == 8);
static_assert(offsetof(easycon_native_capture_options, image_limits) == 0);
static_assert(offsetof(easycon_native_capture_options, open_timeout_ns) == 40);
static_assert(offsetof(easycon_native_capture_options, read_timeout_ns) == 48);
static_assert(offsetof(easycon_native_capture_options, max_frames) == 56);
static_assert(offsetof(easycon_native_capture_options, reserved) == 60);
static_assert(sizeof(easycon_native_capture_profile) == 32);
static_assert(alignof(easycon_native_capture_profile) == 8);
static_assert(offsetof(easycon_native_capture_profile, backend) == 0);
static_assert(offsetof(easycon_native_capture_profile, width) == 4);
static_assert(offsetof(easycon_native_capture_profile, height) == 8);
static_assert(offsetof(easycon_native_capture_profile, pixel_format) == 12);
static_assert(offsetof(easycon_native_capture_profile, stride) == 16);
static_assert(offsetof(easycon_native_capture_profile, frame_interval_ns) == 24);
#if defined(EASYCON_NATIVE_TESTING)
static_assert(sizeof(easycon_native_ocr_test_counts) == 40);
static_assert(alignof(easycon_native_ocr_test_counts) == 8);
static_assert(offsetof(easycon_native_ocr_test_counts, created) == 0);
static_assert(offsetof(easycon_native_ocr_test_counts, destroyed) == 8);
static_assert(offsetof(easycon_native_ocr_test_counts, process_calls) == 16);
static_assert(offsetof(easycon_native_ocr_test_counts, clear_calls) == 24);
static_assert(offsetof(easycon_native_ocr_test_counts, teardown_exceptions) == 32);
#endif
#endif

#endif
