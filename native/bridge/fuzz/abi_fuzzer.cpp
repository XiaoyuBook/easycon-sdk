#include "internal/easycon_native_bridge.h"

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdlib>

namespace {

void release_error(easycon_native_error& error) {
    easycon_native_error_release(&error);
}

void exercise_vision(const uint8_t* data, size_t size, easycon_native_error& error) {
    if (size < 2) {
        return;
    }
    const auto length = static_cast<uint64_t>((std::min)(size - 1, size_t{64}));
    const easycon_native_image_limits limits{64, 64, 64, 4096, 4096, 64};
    const easycon_native_image_view image{
        data + 1,
        length,
        static_cast<uint32_t>(length),
        1,
        length,
        EASYCON_NATIVE_PIXEL_FORMAT_GRAY8,
    };

    easycon_native_match_extrema extrema{};
    const auto template_method = UINT32_C(1) + (data[0] % UINT8_C(3));
    (void)easycon_native_match_template(
        &image,
        &image,
        template_method,
        &limits,
        &extrema,
        &error);
    release_error(error);

    easycon_native_image edge{};
    const auto edge_method = UINT32_C(1) + (data[0] % UINT8_C(2));
    (void)easycon_native_edge_preprocess(
        &image,
        edge_method,
        &limits,
        &edge,
        &error);
    release_error(error);
    easycon_native_image_release(&edge);

    const auto hue_a = static_cast<uint32_t>(data[0] % UINT8_C(180));
    const auto hue_b = static_cast<uint32_t>(data[1] % UINT8_C(180));
    const easycon_native_hsv_range range{hue_a, hue_b, 0, 255, 0, 255};
    easycon_native_color_result color{};
    (void)easycon_native_hsv_count(
        &image,
        0,
        0,
        static_cast<uint32_t>(length),
        1,
        &range,
        &limits,
        &color,
        &error);
    release_error(error);
}

}  // namespace

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
        exercise_vision(data, size, error);
    }

    easycon_native_counts final_counts{};
    if (easycon_native_debug_counts(&final_counts, &error) != EASYCON_NATIVE_STATUS_OK ||
        final_counts.live_handles != baseline.live_handles ||
        final_counts.live_allocations != baseline.live_allocations) {
        std::abort();
    }
    return 0;
}
