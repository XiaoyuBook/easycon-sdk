#ifndef EASYCON_NATIVE_BRIDGE_INTERNAL_HPP
#define EASYCON_NATIVE_BRIDGE_INTERNAL_HPP

#include "internal/easycon_native_bridge.h"

#include <cstddef>
#include <new>
#include <string_view>
#include <utility>

#include <opencv2/core.hpp>

namespace easycon::native::detail {

easycon_native_status set_error(
    easycon_native_error* out_error,
    easycon_native_status status,
    std::string_view message) noexcept;

std::byte* allocate_bytes(size_t length) noexcept;
void release_bytes(void* data) noexcept;

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
