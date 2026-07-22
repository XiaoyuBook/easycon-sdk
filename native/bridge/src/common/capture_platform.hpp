#ifndef EASYCON_CAPTURE_PLATFORM_HPP
#define EASYCON_CAPTURE_PLATFORM_HPP

#include "bridge_internal.hpp"

#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace easycon::native::capture {

struct Descriptor {
    std::string source_id;
    std::string display_name;
};

easycon_native_status platform_discover(
    uint32_t backend,
    std::vector<Descriptor>& output,
    easycon_native_error* error);

easycon_native_status platform_open(
    uint32_t backend,
    std::string_view source,
    const easycon_native_capture_options& options,
    easycon_native_capture_profile* profile,
    easycon_native_error* error);

}  // namespace easycon::native::capture

#endif
