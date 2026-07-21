#include "internal/easycon_native_bridge.h"

#include <cstddef>
#include <cstdint>
#include <cstdlib>

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
    easycon_native_counts baseline{};
    easycon_native_error error{};
    if (easycon_native_debug_counts(&baseline, &error) != EASYCON_NATIVE_STATUS_OK) {
        std::abort();
    }

    if (size > 0) {
        easycon_native_debug_handle* handle = nullptr;
        if ((data[0] & UINT8_C(1)) != 0) {
            if (easycon_native_debug_handle_create(&handle, &error) != EASYCON_NATIVE_STATUS_OK) {
                std::abort();
            }
            if (easycon_native_debug_handle_destroy(&handle, &error) != EASYCON_NATIVE_STATUS_OK) {
                std::abort();
            }
        }
        const auto kind = static_cast<int32_t>(data[0] % UINT8_C(3));
        (void)easycon_native_test_raise(kind, &error);
        easycon_native_error_release(&error);
    }

    easycon_native_counts final_counts{};
    if (easycon_native_debug_counts(&final_counts, &error) != EASYCON_NATIVE_STATUS_OK ||
        final_counts.live_handles != baseline.live_handles ||
        final_counts.live_allocations != baseline.live_allocations) {
        std::abort();
    }
    return 0;
}
