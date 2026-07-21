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

#define EASYCON_NATIVE_PIXEL_FORMAT_BGR8 UINT32_C(1)
#define EASYCON_NATIVE_PIXEL_FORMAT_BGRA8 UINT32_C(2)
#define EASYCON_NATIVE_PIXEL_FORMAT_GRAY8 UINT32_C(3)

#define EASYCON_NATIVE_TEMPLATE_SQDIFF_NORMED UINT32_C(1)
#define EASYCON_NATIVE_TEMPLATE_CCORR_NORMED UINT32_C(2)
#define EASYCON_NATIVE_TEMPLATE_CCOEFF_NORMED UINT32_C(3)

#define EASYCON_NATIVE_EDGE_XY UINT32_C(1)
#define EASYCON_NATIVE_EDGE_LAPLACIAN UINT32_C(2)

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

typedef struct easycon_native_debug_handle easycon_native_debug_handle;

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

easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_raise(
    int32_t kind,
    easycon_native_error* out_error) EASYCON_NATIVE_NOEXCEPT;
#endif

#if defined(__cplusplus)
}

static_assert(sizeof(void*) == 8, "Phase 3 native bridge is x64-only");
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
#endif

#endif
