#include "bridge_internal.hpp"

#include <cstdint>
#include <stdexcept>

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_raise(
    int32_t kind,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [kind, out_error]() -> easycon_native_status {
        switch (kind) {
            case EASYCON_NATIVE_TEST_RAISE_CV:
                throw cv::Exception(
                    cv::Error::StsError,
                    "scripted cv exception",
                    "easycon_native_test_raise",
                    __FILE__,
                    __LINE__);
            case EASYCON_NATIVE_TEST_RAISE_STD:
                throw std::runtime_error("scripted std exception");
            case EASYCON_NATIVE_TEST_RAISE_UNKNOWN:
                throw UINT32_C(7);
            default:
                return easycon::native::detail::set_error(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "unknown exception test kind");
        }
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_raise(
    easycon_native_ocr_engine* engine,
    int32_t kind,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [engine, kind, out_error]() -> easycon_native_status {
        if (!easycon::native::detail::ocr_engine_is_valid(engine)) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "OCR engine is invalid");
        }
        switch (kind) {
            case EASYCON_NATIVE_TEST_RAISE_CV:
                throw cv::Exception(
                    cv::Error::StsError,
                    "scripted OCR cv exception",
                    "easycon_native_test_ocr_raise",
                    __FILE__,
                    __LINE__);
            case EASYCON_NATIVE_TEST_RAISE_STD:
                throw std::runtime_error("scripted OCR std exception");
            case EASYCON_NATIVE_TEST_RAISE_UNKNOWN:
                throw UINT32_C(11);
            default:
                return easycon::native::detail::set_error(
                    out_error,
                    EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                    "unknown OCR exception test kind");
        }
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_counts(
    easycon_native_ocr_test_counts* out_counts,
    easycon_native_error* out_error) noexcept {
    if (out_counts != nullptr) {
        *out_counts = {};
    }
    return easycon::native::detail::guard(out_error, [out_counts, out_error]() {
        if (out_counts == nullptr) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "out_counts is null");
        }
        const auto counts = easycon::native::detail::ocr_debug_counts();
        out_counts->created = counts.created;
        out_counts->destroyed = counts.destroyed;
        out_counts->process_calls = counts.process_calls;
        out_counts->clear_calls = counts.clear_calls;
        out_counts->teardown_exceptions = counts.teardown_exceptions;
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_fail_next(
    int32_t failpoint,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [failpoint, out_error]() {
        if (!easycon::native::detail::ocr_test_fail_next(failpoint)) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "OCR failpoint is invalid or already armed");
        }
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_test_ocr_invalidate(
    easycon_native_ocr_engine* engine,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [engine, out_error]() {
        if (!easycon::native::detail::ocr_test_invalidate(engine)) {
            return easycon::native::detail::set_error(
                out_error,
                EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
                "OCR engine cannot be invalidated");
        }
        return EASYCON_NATIVE_STATUS_OK;
    });
}
