#include "capture_platform.hpp"

namespace easycon::native::capture {

easycon_native_status platform_discover(
    uint32_t backend,
    std::vector<Descriptor>& output,
    easycon_native_error* error) {
    static_cast<void>(backend);
    static_cast<void>(output);
    return easycon::native::detail::set_error(
        error,
        EASYCON_NATIVE_STATUS_UNSUPPORTED,
        "macOS capture is an experimental source boundary with no implemented backend");
}

easycon_native_status platform_open(
    uint32_t backend,
    std::string_view source,
    const easycon_native_capture_options& options,
    easycon_native_capture_profile* profile,
    easycon_native_error* error) {
    static_cast<void>(backend);
    static_cast<void>(source);
    static_cast<void>(options);
    static_cast<void>(profile);
    return easycon::native::detail::set_error(
        error,
        EASYCON_NATIVE_STATUS_UNSUPPORTED,
        "macOS capture is build-unverified and has no implemented backend");
}

}  // namespace easycon::native::capture
