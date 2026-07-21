#ifndef EASYCON_NATIVE_BRIDGE_INTERNAL_HPP
#define EASYCON_NATIVE_BRIDGE_INTERNAL_HPP

#include "internal/easycon_native_bridge.h"

#include <cstddef>
#include <new>
#include <string_view>
#include <utility>

#include <opencv2/core.hpp>

namespace easycon::native::detail {

struct OcrDebugCounts {
    uint64_t created;
    uint64_t destroyed;
    uint64_t process_calls;
    uint64_t clear_calls;
    uint64_t teardown_exceptions;
};

easycon_native_status set_error(
    easycon_native_error* out_error,
    easycon_native_status status,
    std::string_view message) noexcept;

std::byte* allocate_bytes(size_t length) noexcept;
void release_bytes(void* data) noexcept;
void track_handle_created() noexcept;
void track_handle_destroyed() noexcept;

OcrDebugCounts ocr_debug_counts() noexcept;
bool ocr_engine_is_valid(const easycon_native_ocr_engine* engine) noexcept;
#if defined(EASYCON_NATIVE_TESTING)
bool ocr_test_fail_next(int32_t failpoint) noexcept;
bool ocr_test_invalidate(easycon_native_ocr_engine* engine) noexcept;
#endif

easycon_native_status validate_image_limits(
    const easycon_native_image_limits* limits,
    easycon_native_error* error) noexcept;
easycon_native_status make_image_view(
    const easycon_native_image_view* image,
    const easycon_native_image_limits& limits,
    cv::Mat& out_mat,
    easycon_native_error* error);
easycon_native_status copy_image_mat(
    const cv::Mat& mat,
    const easycon_native_image_limits& limits,
    easycon_native_image* output,
    easycon_native_error* error) noexcept;

template <typename Function>
easycon_native_status guard(easycon_native_error* out_error, Function&& function) noexcept {
    if (out_error == nullptr) {
        return EASYCON_NATIVE_STATUS_INVALID_ARGUMENT;
    }
    *out_error = {};
    try {
        return std::forward<Function>(function)();
    } catch (const cv::Exception&) {
        return set_error(out_error, EASYCON_NATIVE_STATUS_CV_EXCEPTION, "OpenCV exception");
    } catch (const std::bad_alloc&) {
        return set_error(out_error, EASYCON_NATIVE_STATUS_ALLOCATION_FAILED, "allocation failed");
    } catch (const std::exception&) {
        return set_error(out_error, EASYCON_NATIVE_STATUS_STD_EXCEPTION, "standard exception");
    } catch (...) {
        return set_error(
            out_error,
            EASYCON_NATIVE_STATUS_UNKNOWN_EXCEPTION,
            "unknown native exception");
    }
}

}  // namespace easycon::native::detail

#endif
