#include "capture_platform.hpp"

#include <atomic>
#include <cstdint>
#include <cstring>
#include <limits>
#include <memory>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace {

constexpr uint64_t capture_marker = UINT64_C(0x4541535943415033);
constexpr uint64_t interrupt_marker = UINT64_C(0x45415359494E5433);
constexpr uint64_t discovery_marker = UINT64_C(0x4541535944495333);
constexpr uint64_t max_source_bytes = UINT64_C(32768);
constexpr uint64_t max_timeout_ns = UINT64_C(60000000000);
constexpr uint32_t max_capture_frames = UINT32_C(4096);
constexpr size_t max_discovered_sources = 64;

struct CaptureControl {
    std::atomic<bool> stop_requested{false};
};

easycon_native_status fail(
    easycon_native_error* error,
    easycon_native_status status,
    std::string_view message) noexcept;

bool valid_utf8(std::string_view text) noexcept {
    size_t index = 0;
    while (index < text.size()) {
        const auto first = static_cast<uint8_t>(text[index]);
        size_t continuation = 0;
        uint32_t code_point = 0;
        if (first <= UINT8_C(0x7F)) {
            ++index;
            continue;
        }
        if (first >= UINT8_C(0xC2) && first <= UINT8_C(0xDF)) {
            continuation = 1;
            code_point = first & UINT8_C(0x1F);
        } else if (first >= UINT8_C(0xE0) && first <= UINT8_C(0xEF)) {
            continuation = 2;
            code_point = first & UINT8_C(0x0F);
        } else if (first >= UINT8_C(0xF0) && first <= UINT8_C(0xF4)) {
            continuation = 3;
            code_point = first & UINT8_C(0x07);
        } else {
            return false;
        }
        if (index + continuation >= text.size()) {
            return false;
        }
        for (size_t offset = 1; offset <= continuation; ++offset) {
            const auto byte = static_cast<uint8_t>(text[index + offset]);
            if ((byte & UINT8_C(0xC0)) != UINT8_C(0x80)) {
                return false;
            }
            code_point = (code_point << 6U) | (byte & UINT8_C(0x3F));
        }
        const bool overlong =
            (continuation == 1 && code_point < UINT32_C(0x80)) ||
            (continuation == 2 && code_point < UINT32_C(0x800)) ||
            (continuation == 3 && code_point < UINT32_C(0x10000));
        if (overlong || (code_point >= UINT32_C(0xD800) && code_point <= UINT32_C(0xDFFF)) ||
            code_point > UINT32_C(0x10FFFF)) {
            return false;
        }
        index += continuation + 1;
    }
    return true;
}

easycon_native_status copy_discovery_string(
    std::string_view value,
    easycon_native_buffer* output,
    easycon_native_error* error) noexcept {
    auto* data = easycon::native::detail::allocate_bytes(value.size());
    if (data == nullptr) {
        return fail(
            error,
            EASYCON_NATIVE_STATUS_ALLOCATION_FAILED,
            "capture descriptor allocation failed");
    }
    std::memcpy(data, value.data(), value.size());
    output->data = reinterpret_cast<uint8_t*>(data);
    output->length = value.size();
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status fail(
    easycon_native_error* error,
    easycon_native_status status,
    std::string_view message) noexcept {
    return easycon::native::detail::set_error(error, status, message);
}

bool valid_backend(uint32_t backend) noexcept {
    return backend == EASYCON_NATIVE_CAPTURE_BACKEND_FILE ||
           backend == EASYCON_NATIVE_CAPTURE_BACKEND_DIRECTSHOW ||
           backend == EASYCON_NATIVE_CAPTURE_BACKEND_MEDIA_FOUNDATION ||
           backend == EASYCON_NATIVE_CAPTURE_BACKEND_V4L2;
}

#if defined(EASYCON_NATIVE_TESTING)
constexpr int32_t no_capture_failpoint = INT32_C(-1);
std::atomic<int32_t> next_capture_failpoint{no_capture_failpoint};

bool take_capture_failpoint(int32_t failpoint) noexcept {
    auto expected = failpoint;
    return next_capture_failpoint.compare_exchange_strong(
        expected,
        no_capture_failpoint,
        std::memory_order_acq_rel,
        std::memory_order_acquire);
}

void inject_capture_failure(int32_t failpoint) {
    if (!take_capture_failpoint(failpoint)) {
        return;
    }
    if (failpoint == EASYCON_NATIVE_TEST_CAPTURE_OPEN_CV) {
        throw cv::Exception(
            cv::Error::StsError,
            "scripted capture open exception",
            "easycon_native_capture_open",
            __FILE__,
            __LINE__);
    }
    if (failpoint == EASYCON_NATIVE_TEST_CAPTURE_READ_STD) {
        throw std::runtime_error("scripted capture read exception");
    }
    if (failpoint == EASYCON_NATIVE_TEST_CAPTURE_CLOSE_UNKNOWN) {
        throw UINT32_C(17);
    }
}
#endif

}  // namespace

struct easycon_native_capture_interrupt {
    uint64_t marker;
    std::shared_ptr<CaptureControl> control;
};

struct easycon_native_capture_discovery {
    uint64_t marker;
    uint32_t backend;
    std::vector<easycon::native::capture::Descriptor> descriptors;
};

struct easycon_native_capture {
    uint64_t marker;
    uint32_t backend;
    std::string source;
    easycon_native_capture_options options;
    std::shared_ptr<CaptureControl> control;
    bool opened;
    bool closed;
};

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_create(
    uint32_t backend,
    const uint8_t* source_utf8,
    uint64_t source_length,
    const easycon_native_capture_options* options,
    easycon_native_capture** out_capture,
    easycon_native_capture_interrupt** out_interrupt,
    easycon_native_error* out_error) noexcept {
    if (out_capture != nullptr) {
        *out_capture = nullptr;
    }
    if (out_interrupt != nullptr) {
        *out_interrupt = nullptr;
    }
    return easycon::native::detail::guard(
        out_error,
        [backend,
         source_utf8,
         source_length,
         options,
         out_capture,
         out_interrupt,
         out_error]() {
            if (out_capture == nullptr || out_interrupt == nullptr || options == nullptr) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "capture create requires options and both owner outputs");
            }
            if (!valid_backend(backend)) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "capture backend is invalid");
            }
            if (source_utf8 == nullptr || source_length == 0 ||
                source_length > max_source_bytes ||
                source_length > static_cast<uint64_t>((std::numeric_limits<size_t>::max)())) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "capture source is empty or exceeds its limit");
            }
            const auto source_size = static_cast<size_t>(source_length);
            const std::string_view source_view(
                reinterpret_cast<const char*>(source_utf8), source_size);
            if (source_view.find('\0') != std::string_view::npos || !valid_utf8(source_view)) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "capture source is not bounded NUL-free UTF-8");
            }
            const auto limit_status =
                easycon::native::detail::validate_image_limits(&options->image_limits, out_error);
            if (limit_status != EASYCON_NATIVE_STATUS_OK) {
                return limit_status;
            }
            if (options->reserved != 0 || options->open_timeout_ns == 0 ||
                options->open_timeout_ns > max_timeout_ns || options->read_timeout_ns == 0 ||
                options->read_timeout_ns > max_timeout_ns || options->max_frames == 0 ||
                options->max_frames > max_capture_frames) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_OUT_OF_RANGE,
                    "capture options exceed fixed bounds");
            }
            std::string source(source_view);
            auto control = std::make_shared<CaptureControl>();
            auto capture = std::make_unique<easycon_native_capture>(easycon_native_capture{
                capture_marker,
                backend,
                std::move(source),
                 *options,
                 control,
                 false,
                 false,
            });
            auto interrupt = std::make_unique<easycon_native_capture_interrupt>(
                easycon_native_capture_interrupt{interrupt_marker, std::move(control)});
            *out_capture = capture.release();
            easycon::native::detail::track_handle_created();
            *out_interrupt = interrupt.release();
            easycon::native::detail::track_handle_created();
            return EASYCON_NATIVE_STATUS_OK;
        });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_open(
    easycon_native_capture* capture,
    easycon_native_capture_profile* out_profile,
    easycon_native_error* out_error) noexcept {
    if (out_profile != nullptr) {
        *out_profile = {};
    }
    return easycon::native::detail::guard(out_error, [capture, out_profile, out_error]() {
        if (out_profile == nullptr) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture profile output is null");
        }
        if (capture == nullptr || capture->marker != capture_marker) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "capture handle is invalid");
        }
#if defined(EASYCON_NATIVE_TESTING)
        inject_capture_failure(EASYCON_NATIVE_TEST_CAPTURE_OPEN_CV);
#endif
        if (capture->opened || capture->closed) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture open is not valid in the current native state");
        }
        if (capture->control->stop_requested.load(std::memory_order_acquire)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_CANCELLED, "capture open was interrupted");
        }
        if (capture->backend == EASYCON_NATIVE_CAPTURE_BACKEND_FILE) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_UNSUPPORTED,
                "path-backed native file capture is unavailable; use prevalidated Rust-owned images");
        }
        const auto status = easycon::native::capture::platform_open(
            capture->backend,
            capture->source,
            capture->options,
            out_profile,
            out_error);
        if (status == EASYCON_NATIVE_STATUS_OK) {
            capture->opened = true;
        }
        return status;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_read(
    easycon_native_capture* capture,
    easycon_native_image* out_image,
    easycon_native_error* out_error) noexcept {
    if (out_image != nullptr) {
        *out_image = {};
    }
    return easycon::native::detail::guard(out_error, [capture, out_image, out_error]() {
        if (out_image == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "capture image output is null");
        }
        if (capture == nullptr || capture->marker != capture_marker) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "capture handle is invalid");
        }
#if defined(EASYCON_NATIVE_TESTING)
        inject_capture_failure(EASYCON_NATIVE_TEST_CAPTURE_READ_STD);
#endif
        if (!capture->opened || capture->closed) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture read is not valid before open or after close");
        }
        if (capture->control->stop_requested.load(std::memory_order_acquire)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_CANCELLED, "capture read was interrupted");
        }
        return fail(
            out_error,
            EASYCON_NATIVE_STATUS_UNSUPPORTED,
            "capture read has no qualified backend");
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_close(
    easycon_native_capture* capture,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [capture, out_error]() {
        if (capture == nullptr || capture->marker != capture_marker) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "capture handle is invalid");
        }
#if defined(EASYCON_NATIVE_TESTING)
        inject_capture_failure(EASYCON_NATIVE_TEST_CAPTURE_CLOSE_UNKNOWN);
#endif
        if (capture->closed) {
            return EASYCON_NATIVE_STATUS_OK;
        }
        capture->control->stop_requested.store(true, std::memory_order_release);
        capture->opened = false;
        capture->closed = true;
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_destroy(
    easycon_native_capture** inout_capture,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [inout_capture, out_error]() {
        if (inout_capture == nullptr) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture owner pointer is null");
        }
        if (*inout_capture == nullptr) {
            return EASYCON_NATIVE_STATUS_OK;
        }
        if ((*inout_capture)->marker != capture_marker) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "capture handle is invalid");
        }
#if defined(EASYCON_NATIVE_TESTING)
        if (take_capture_failpoint(EASYCON_NATIVE_TEST_CAPTURE_DESTROY_BEFORE_CONSUME)) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_BACKEND_ERROR,
                "scripted capture destroy-before-consume failure");
        }
        if (take_capture_failpoint(EASYCON_NATIVE_TEST_CAPTURE_DESTROY_OK_NO_ACK)) {
            return EASYCON_NATIVE_STATUS_OK;
        }
        const bool consume_with_error =
            take_capture_failpoint(EASYCON_NATIVE_TEST_CAPTURE_CONSUME_WITH_ERROR);
#else
        constexpr bool consume_with_error = false;
#endif
        auto* const capture = *inout_capture;
        *inout_capture = nullptr;
        capture->marker = 0;
        delete capture;
        easycon::native::detail::track_handle_destroyed();
        if (consume_with_error) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_BACKEND_ERROR,
                "scripted capture consumed destroy failure");
        }
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_interrupt_request(
    easycon_native_capture_interrupt* interrupt,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [interrupt, out_error]() {
        if (interrupt == nullptr || interrupt->marker != interrupt_marker || !interrupt->control) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture interrupt handle is invalid");
        }
        interrupt->control->stop_requested.store(true, std::memory_order_release);
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_interrupt_destroy(
    easycon_native_capture_interrupt** inout_interrupt,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [inout_interrupt, out_error]() {
        if (inout_interrupt == nullptr) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture interrupt owner pointer is null");
        }
        if (*inout_interrupt == nullptr) {
            return EASYCON_NATIVE_STATUS_OK;
        }
        if ((*inout_interrupt)->marker != interrupt_marker) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture interrupt handle is invalid");
        }
        auto* const interrupt = *inout_interrupt;
        *inout_interrupt = nullptr;
        interrupt->marker = 0;
        delete interrupt;
        easycon::native::detail::track_handle_destroyed();
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_create(
    uint32_t backend,
    easycon_native_capture_discovery** out_discovery,
    easycon_native_error* out_error) noexcept {
    if (out_discovery != nullptr) {
        *out_discovery = nullptr;
    }
    return easycon::native::detail::guard(out_error, [backend, out_discovery, out_error]() {
        if (out_discovery == nullptr) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture discovery output is null");
        }
        if (backend == EASYCON_NATIVE_CAPTURE_BACKEND_FILE || !valid_backend(backend)) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture discovery requires a device adapter");
        }
        std::vector<easycon::native::capture::Descriptor> descriptors;
        descriptors.reserve(max_discovered_sources);
        const auto status =
            easycon::native::capture::platform_discover(backend, descriptors, out_error);
        if (status != EASYCON_NATIVE_STATUS_OK) {
            return status;
        }
        auto discovery = std::make_unique<easycon_native_capture_discovery>(
            easycon_native_capture_discovery{
                discovery_marker,
                backend,
                std::move(descriptors),
            });
        *out_discovery = discovery.release();
        easycon::native::detail::track_handle_created();
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_count(
    easycon_native_capture_discovery* discovery,
    uint32_t* out_count,
    easycon_native_error* out_error) noexcept {
    if (out_count != nullptr) {
        *out_count = 0;
    }
    return easycon::native::detail::guard(out_error, [discovery, out_count, out_error]() {
        if (out_count == nullptr) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture discovery count output is null");
        }
        if (discovery == nullptr || discovery->marker != discovery_marker) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture discovery handle is invalid");
        }
        *out_count = static_cast<uint32_t>(discovery->descriptors.size());
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_get(
    easycon_native_capture_discovery* discovery,
    uint32_t index,
    easycon_native_buffer* out_source_id,
    easycon_native_buffer* out_display_name,
    easycon_native_error* out_error) noexcept {
    if (out_source_id != nullptr) {
        *out_source_id = {};
    }
    if (out_display_name != nullptr) {
        *out_display_name = {};
    }
    return easycon::native::detail::guard(
        out_error,
        [discovery, index, out_source_id, out_display_name, out_error]() {
            if (out_source_id == nullptr || out_display_name == nullptr) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "capture descriptor outputs are null");
            }
            if (discovery == nullptr || discovery->marker != discovery_marker) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "capture discovery handle is invalid");
            }
            if (index >= discovery->descriptors.size()) {
                return fail(
                    out_error,
                    EASYCON_NATIVE_STATUS_OUT_OF_RANGE,
                    "capture descriptor index is out of range");
            }
            const auto& descriptor = discovery->descriptors[index];
            auto status =
                copy_discovery_string(descriptor.source_id, out_source_id, out_error);
            if (status != EASYCON_NATIVE_STATUS_OK) {
                return status;
            }
            status = copy_discovery_string(
                descriptor.display_name, out_display_name, out_error);
            if (status != EASYCON_NATIVE_STATUS_OK) {
                easycon_native_buffer_release(out_source_id);
                return status;
            }
            return EASYCON_NATIVE_STATUS_OK;
        });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_capture_discovery_destroy(
    easycon_native_capture_discovery** inout_discovery,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [inout_discovery, out_error]() {
        if (inout_discovery == nullptr) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture discovery owner pointer is null");
        }
        if (*inout_discovery == nullptr) {
            return EASYCON_NATIVE_STATUS_OK;
        }
        if ((*inout_discovery)->marker != discovery_marker) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture discovery handle is invalid");
        }
        auto* const discovery = *inout_discovery;
        *inout_discovery = nullptr;
        discovery->marker = 0;
        delete discovery;
        easycon::native::detail::track_handle_destroyed();
        return EASYCON_NATIVE_STATUS_OK;
    });
}

#if defined(EASYCON_NATIVE_TESTING)
extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_capture_fail_next(
    int32_t failpoint,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [failpoint, out_error]() {
        if (failpoint < EASYCON_NATIVE_TEST_CAPTURE_OPEN_CV ||
            failpoint > EASYCON_NATIVE_TEST_CAPTURE_DESTROY_OK_NO_ACK) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "capture failpoint is invalid");
        }
        auto expected = no_capture_failpoint;
        if (!next_capture_failpoint.compare_exchange_strong(
                expected,
                failpoint,
                std::memory_order_acq_rel,
                std::memory_order_acquire)) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "a capture failpoint is already armed");
        }
        return EASYCON_NATIVE_STATUS_OK;
    });
}
#endif
