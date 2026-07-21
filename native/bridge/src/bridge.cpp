#include "bridge_internal.hpp"

#include <atomic>
#include <cstring>
#include <limits>
#include <new>

struct easycon_native_debug_handle {
    uint64_t marker;
};

namespace {

constexpr uint64_t debug_handle_marker = UINT64_C(0x45415359434F4E33);
std::atomic<uint64_t> live_handles{0};
std::atomic<uint64_t> live_allocations{0};

void clear_error(easycon_native_error* error) noexcept {
    if (error != nullptr) {
        *error = {};
    }
}

}  // namespace

namespace easycon::native::detail {

easycon_native_status set_error(
    easycon_native_error* out_error,
    easycon_native_status status,
    std::string_view message) noexcept {
    clear_error(out_error);
    if (out_error == nullptr || message.empty()) {
        return status;
    }
    if (message.size() > static_cast<size_t>((std::numeric_limits<uint64_t>::max)())) {
        return status;
    }
    auto* data = new (std::nothrow) char[message.size()];
    if (data == nullptr) {
        return status;
    }
    std::memcpy(data, message.data(), message.size());
    live_allocations.fetch_add(1, std::memory_order_relaxed);
    out_error->code = status;
    out_error->data = data;
    out_error->length = static_cast<uint64_t>(message.size());
    return status;
}

}  // namespace easycon::native::detail

extern "C" void EASYCON_NATIVE_CALL easycon_native_error_release(
    easycon_native_error* error) noexcept {
    if (error == nullptr) {
        return;
    }
    if (error->data != nullptr) {
        delete[] error->data;
        live_allocations.fetch_sub(1, std::memory_order_relaxed);
    }
    *error = {};
}

extern "C" void EASYCON_NATIVE_CALL easycon_native_buffer_release(
    easycon_native_buffer* buffer) noexcept {
    if (buffer == nullptr) {
        return;
    }
    if (buffer->data != nullptr) {
        delete[] buffer->data;
        live_allocations.fetch_sub(1, std::memory_order_relaxed);
    }
    *buffer = {};
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_debug_counts(
    easycon_native_counts* out_counts,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [out_counts, out_error]() {
        if (out_counts == nullptr) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "out_counts is null");
        }
        *out_counts = {};
        out_counts->live_handles = live_handles.load(std::memory_order_relaxed);
        out_counts->live_allocations = live_allocations.load(std::memory_order_relaxed);
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_debug_handle_create(
    easycon_native_debug_handle** out_handle,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [out_handle, out_error]() {
        if (out_handle == nullptr) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "out_handle is null");
        }
        *out_handle = nullptr;
        // The enclosing guard maps std::bad_alloc before it can cross the ABI.
        auto* handle = new easycon_native_debug_handle{debug_handle_marker};  // NOLINT(bugprone-unhandled-exception-at-new)
        live_handles.fetch_add(1, std::memory_order_relaxed);
        *out_handle = handle;
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_debug_handle_destroy(
    easycon_native_debug_handle** inout_handle,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [inout_handle, out_error]() {
        if (inout_handle == nullptr) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "inout_handle is null");
        }
        if (*inout_handle == nullptr) {
            return EASYCON_NATIVE_STATUS_OK;
        }
        if ((*inout_handle)->marker != debug_handle_marker) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "debug handle marker is invalid");
        }
        delete *inout_handle;
        *inout_handle = nullptr;
        live_handles.fetch_sub(1, std::memory_order_relaxed);
        return EASYCON_NATIVE_STATUS_OK;
    });
}
