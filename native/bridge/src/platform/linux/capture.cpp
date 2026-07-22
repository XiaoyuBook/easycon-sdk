#include "capture_platform.hpp"

namespace {

easycon_native_status unavailable(
    easycon_native_error* error,
    std::string_view operation) noexcept {
    return easycon::native::detail::set_error(
        error,
        EASYCON_NATIVE_STATUS_UNSUPPORTED,
        operation);
}

}  // namespace

namespace easycon::native::capture {

easycon_native_status platform_discover(
    uint32_t backend,
    std::vector<Descriptor>& output,
    easycon_native_error* error) {
    static_cast<void>(output);
    if (backend == EASYCON_NATIVE_CAPTURE_BACKEND_V4L2) {
        return unavailable(
            error,
            "V4L2 discovery is unavailable until Linux hardware qualification");
    }
    return unavailable(error, "capture discovery adapter is unavailable on Linux");
}

easycon_native_status platform_open(
    uint32_t backend,
    std::string_view source,
    const easycon_native_capture_options& options,
    easycon_native_capture_profile* profile,
    easycon_native_error* error) {
    static_cast<void>(source);
    static_cast<void>(options);
    static_cast<void>(profile);
    if (backend == EASYCON_NATIVE_CAPTURE_BACKEND_V4L2) {
        return unavailable(
            error,
            "V4L2 capture is unavailable until bounded I/O and hardware qualification");
    }
    return unavailable(error, "capture open adapter is unavailable on Linux");
}

}  // namespace easycon::native::capture
