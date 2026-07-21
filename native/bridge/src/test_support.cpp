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
