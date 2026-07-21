#include "internal/easycon_native_bridge.h"

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <fstream>
#include <iostream>
#include <string>
#include <string_view>
#include <vector>

namespace {

int failures = 0;

void expect(bool condition, std::string_view message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << '\n';
        ++failures;
    }
}

easycon_native_counts counts() {
    easycon_native_counts value{};
    easycon_native_error error{};
    const auto status = easycon_native_debug_counts(&value, &error);
    expect(status == EASYCON_NATIVE_STATUS_OK, "debug counts succeeds");
    expect(error.data == nullptr && error.length == 0, "success leaves error empty");
    easycon_native_error_release(&error);
    return value;
}

int hex_digit(char value) {
    if (value >= '0' && value <= '9') {
        return value - '0';
    }
    if (value >= 'a' && value <= 'f') {
        return value - 'a' + 10;
    }
    return -1;
}

std::vector<uint8_t> read_hex(std::string_view name) {
    std::ifstream input(std::string("fixtures/codec/") + std::string(name));
    expect(input.good(), "codec fixture opens");
    std::string text;
    input >> text;
    expect((text.size() % 2) == 0, "codec fixture has complete hex bytes");

    std::vector<uint8_t> bytes;
    bytes.reserve(text.size() / 2);
    for (size_t index = 0; index + 1 < text.size(); index += 2) {
        const auto high = hex_digit(text[index]);
        const auto low = hex_digit(text[index + 1]);
        expect(high >= 0 && low >= 0, "codec fixture is lowercase hexadecimal");
        if (high < 0 || low < 0) {
            return {};
        }
        bytes.push_back(static_cast<uint8_t>((high << 4) | low));
    }
    return bytes;
}

easycon_native_image_limits image_limits() {
    return easycon_native_image_limits{
        UINT64_C(4096),
        UINT32_C(64),
        UINT32_C(64),
        UINT64_C(4096),
        UINT64_C(16384),
        UINT64_C(1024),
    };
}

easycon_native_image_view view_of(const easycon_native_image& image) {
    return easycon_native_image_view{
        image.data,
        image.length,
        image.width,
        image.height,
        image.stride,
        image.pixel_format,
    };
}

bool is_zero(const easycon_native_image& image) {
    return image.data == nullptr && image.length == 0 && image.width == 0 &&
           image.height == 0 && image.stride == 0 && image.pixel_format == 0;
}

bool is_zero(const easycon_native_buffer& buffer) {
    return buffer.data == nullptr && buffer.length == 0;
}

void expect_pixels(
    const easycon_native_image& image,
    const std::vector<uint8_t>& expected,
    std::string_view label) {
    const auto same = image.length == expected.size() &&
                      std::equal(expected.begin(), expected.end(), image.data);
    expect(same, label);
}

void test_handle_lifecycle() {
    const auto baseline = counts();
    easycon_native_debug_handle* handle = nullptr;
    easycon_native_error error{};

    expect(
        easycon_native_debug_handle_create(&handle, &error) == EASYCON_NATIVE_STATUS_OK,
        "debug handle create succeeds");
    expect(handle != nullptr, "debug handle create returns an owner");
    expect(error.data == nullptr && error.length == 0, "create success leaves error empty");
    expect(counts().live_handles == baseline.live_handles + 1, "create increments handle count");

    expect(
        easycon_native_debug_handle_destroy(&handle, &error) == EASYCON_NATIVE_STATUS_OK,
        "debug handle destroy succeeds");
    expect(handle == nullptr, "destroy consumes and clears the handle");
    expect(counts().live_handles == baseline.live_handles, "destroy restores handle count");
    expect(
        easycon_native_debug_handle_destroy(&handle, &error) == EASYCON_NATIVE_STATUS_OK,
        "destroy is idempotent for a null handle");
    easycon_native_error_release(&error);
}

void test_status_error_and_zero_buffer() {
    easycon_native_error error{};
    const auto status = easycon_native_debug_counts(nullptr, &error);
    expect(status == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "invalid output is rejected");
    expect(error.code == status, "validation error code mirrors status");
    expect(error.data != nullptr && error.length > 0, "validation error owns a diagnostic");
    easycon_native_error_release(&error);

    easycon_native_buffer buffer{};
    easycon_native_buffer_release(&buffer);
    expect(buffer.data == nullptr && buffer.length == 0, "zero buffer release is idempotent");
    easycon_native_buffer_release(&buffer);
}

void test_exception(int kind, easycon_native_status expected, std::string_view label) {
    const auto baseline = counts();
    easycon_native_error error{};
    const auto status = easycon_native_test_raise(kind, &error);

    expect(status == expected, label);
    expect(error.code == expected, "error code mirrors status");
    expect(error.data != nullptr && error.length > 0, "exception returns an owned diagnostic");
    expect(
        counts().live_allocations == baseline.live_allocations + 1,
        "diagnostic increments allocation count");

    easycon_native_error_release(&error);
    expect(error.code == 0 && error.data == nullptr && error.length == 0, "release zeroes error");
    expect(
        counts().live_allocations == baseline.live_allocations,
        "diagnostic release restores allocation count");
    easycon_native_error_release(&error);
}

void test_codec_round_trip_conversion_and_roi() {
    const auto baseline = counts();
    const auto bmp = read_hex("bgr-2x2.bmp.hex");
    const auto png = read_hex("bgr-2x2.png.hex");
    const auto expected_bgr = read_hex("expected-bgr.hex");
    const auto limits = image_limits();
    easycon_native_error error{};

    easycon_native_image decoded_bmp{};
    expect(
        easycon_native_image_decode(
            bmp.data(), bmp.size(), &limits, &decoded_bmp, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "BMP decodes");
    expect(
        decoded_bmp.width == 2 && decoded_bmp.height == 2 && decoded_bmp.stride == 6 &&
            decoded_bmp.pixel_format == EASYCON_NATIVE_PIXEL_FORMAT_BGR8,
        "BMP metadata is tight BGR8");
    expect_pixels(decoded_bmp, expected_bgr, "BMP pixels match fixture");

    easycon_native_image decoded_png{};
    expect(
        easycon_native_image_decode(
            png.data(), png.size(), &limits, &decoded_png, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "PNG decodes");
    expect_pixels(decoded_png, expected_bgr, "PNG pixels match fixture");

    easycon_native_buffer encoded{};
    const auto bmp_view = view_of(decoded_bmp);
    expect(
        easycon_native_image_encode_png(&bmp_view, &limits, &encoded, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "BGR image encodes as PNG");
    expect(encoded.data != nullptr && encoded.length > 0, "PNG encode owns bytes");

    easycon_native_image round_trip{};
    expect(
        easycon_native_image_decode(
            encoded.data, encoded.length, &limits, &round_trip, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "encoded PNG decodes");
    expect_pixels(round_trip, expected_bgr, "PNG round trip is lossless");

    easycon_native_image bgra{};
    expect(
        easycon_native_image_convert(
            &bmp_view, EASYCON_NATIVE_PIXEL_FORMAT_BGRA8, &limits, &bgra, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "BGR converts to BGRA");
    const std::vector<uint8_t> expected_bgra{
        0, 0, 255, 255, 0, 255, 0, 255,
        255, 0, 0, 255, 255, 255, 255, 255,
    };
    expect_pixels(bgra, expected_bgra, "added alpha is opaque");

    easycon_native_image gray_from_bgr{};
    expect(
        easycon_native_image_convert(
            &bmp_view, EASYCON_NATIVE_PIXEL_FORMAT_GRAY8, &limits, &gray_from_bgr, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "BGR converts to Gray");
    expect_pixels(gray_from_bgr, std::vector<uint8_t>{76, 150, 29, 255}, "Gray values match OpenCV");

    const auto gray_view = view_of(gray_from_bgr);
    easycon_native_image bgr_from_gray{};
    expect(
        easycon_native_image_convert(
            &gray_view, EASYCON_NATIVE_PIXEL_FORMAT_BGR8, &limits, &bgr_from_gray, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "Gray converts to BGR");
    easycon_native_image bgra_from_gray{};
    expect(
        easycon_native_image_convert(
            &gray_view, EASYCON_NATIVE_PIXEL_FORMAT_BGRA8, &limits, &bgra_from_gray, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "Gray converts to BGRA");

    const auto bgra_view = view_of(bgra);
    easycon_native_image bgr_from_bgra{};
    expect(
        easycon_native_image_convert(
            &bgra_view, EASYCON_NATIVE_PIXEL_FORMAT_BGR8, &limits, &bgr_from_bgra, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "BGRA converts to BGR");
    expect_pixels(bgr_from_bgra, expected_bgr, "BGRA to BGR removes only alpha");
    easycon_native_image gray_from_bgra{};
    expect(
        easycon_native_image_convert(
            &bgra_view, EASYCON_NATIVE_PIXEL_FORMAT_GRAY8, &limits, &gray_from_bgra, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "BGRA converts to Gray");
    expect_pixels(gray_from_bgra, std::vector<uint8_t>{76, 150, 29, 255}, "BGRA Gray values match");

    easycon_native_image cropped{};
    expect(
        easycon_native_image_crop(
            &bmp_view, 1, 0, 1, 2, &limits, &cropped, &error) == EASYCON_NATIVE_STATUS_OK,
        "ROI crops");
    expect(
        cropped.width == 1 && cropped.height == 2 && cropped.stride == 3,
        "ROI output is tight");
    expect_pixels(cropped, std::vector<uint8_t>{0, 255, 0, 255, 255, 255}, "ROI pixels match");

    const std::vector<uint8_t> padded_pixels{
        0, 0, 255, 0, 255, 0, 9, 9,
        255, 0, 0, 255, 255, 255, 8, 8,
    };
    const easycon_native_image_view padded_view{
        padded_pixels.data(),
        padded_pixels.size(),
        UINT32_C(2),
        UINT32_C(2),
        UINT64_C(8),
        EASYCON_NATIVE_PIXEL_FORMAT_BGR8,
    };
    easycon_native_buffer padded_encoded{};
    expect(
        easycon_native_image_encode_png(
            &padded_view, &limits, &padded_encoded, &error) == EASYCON_NATIVE_STATUS_OK,
        "padded BGR rows encode");
    easycon_native_image padded_round_trip{};
    expect(
        easycon_native_image_decode(
            padded_encoded.data,
            padded_encoded.length,
            &limits,
            &padded_round_trip,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "padded BGR PNG decodes");
    expect_pixels(padded_round_trip, expected_bgr, "padded rows exclude padding bytes");

    easycon_native_image rejected_crop{};
    expect(
        easycon_native_image_crop(
            &bmp_view, 2, 0, 1, 1, &limits, &rejected_crop, &error) ==
            EASYCON_NATIVE_STATUS_OUT_OF_RANGE,
        "out-of-bounds ROI is rejected");
    expect(
        rejected_crop.data == nullptr && rejected_crop.length == 0,
        "rejected ROI leaves output zero");
    easycon_native_error_release(&error);

    easycon_native_buffer_release(&encoded);
    easycon_native_buffer_release(&padded_encoded);
    for (auto* image : {
             &decoded_bmp,
             &decoded_png,
             &round_trip,
             &bgra,
             &gray_from_bgr,
             &bgr_from_gray,
             &bgra_from_gray,
             &bgr_from_bgra,
             &gray_from_bgra,
             &cropped,
             &padded_round_trip,
         }) {
        easycon_native_image_release(image);
        easycon_native_image_release(image);
    }
    easycon_native_error_release(&error);
    expect(counts().live_allocations == baseline.live_allocations, "codec allocations return to baseline");
}

void test_codec_rejects_invalid_truncated_and_oversized_input() {
    const auto baseline = counts();
    const auto bmp = read_hex("bgr-2x2.bmp.hex");
    const auto png = read_hex("bgr-2x2.png.hex");
    const auto expected_bgr = read_hex("expected-bgr.hex");
    const auto gray_alpha = read_hex("gray-alpha-1x1.png.hex");
    const auto palette = read_hex("palette-1x1.png.hex");
    auto limits = image_limits();
    easycon_native_error error{};
    easycon_native_image output{};

    expect(
        easycon_native_image_decode(
            gray_alpha.data(), gray_alpha.size(), &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "GrayAlpha PNG decodes through OpenCV's BGRA normalization");
    expect(
        output.width == 1 && output.height == 1 && output.stride == 4 &&
            output.pixel_format == EASYCON_NATIVE_PIXEL_FORMAT_BGRA8,
        "GrayAlpha PNG maps to tight BGRA8");
    expect_pixels(output, std::vector<uint8_t>{127, 127, 127, 255}, "GrayAlpha pixels match");
    easycon_native_image_release(&output);
    easycon_native_error_release(&error);

    expect(
        easycon_native_image_decode(
            palette.data(), palette.size(), &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_IMAGE,
        "palette PNG channels are rejected by preflight");
    expect(is_zero(output), "unsupported encoded channels leave output zero");
    easycon_native_error_release(&error);

    expect(
        easycon_native_image_decode(nullptr, 0, &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "empty encoded image is rejected");
    easycon_native_error_release(&error);

    expect(
        easycon_native_image_decode(
            bmp.data(), bmp.size() - 10, &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_IMAGE,
        "truncated BMP pixels are rejected");
    easycon_native_error_release(&error);

    expect(
        easycon_native_image_decode(
            png.data(), 20, &limits, &output, &error) == EASYCON_NATIVE_STATUS_INVALID_IMAGE,
        "truncated PNG is rejected");
    expect(output.data == nullptr && output.length == 0, "decode failure leaves output zero");
    easycon_native_error_release(&error);

    expect(
        easycon_native_image_decode(
            png.data(), 40, &limits, &output, &error) == EASYCON_NATIVE_STATUS_INVALID_IMAGE,
        "PNG truncated after a complete IHDR is rejected");
    easycon_native_error_release(&error);

    auto unsupported_depth = png;
    unsupported_depth[24] = 16;
    expect(
        easycon_native_image_decode(
            unsupported_depth.data(), unsupported_depth.size(), &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_IMAGE,
        "unsupported PNG depth is rejected before decode");
    easycon_native_error_release(&error);

    auto zero_width = png;
    std::fill(zero_width.begin() + 16, zero_width.begin() + 20, uint8_t{0});
    expect(
        easycon_native_image_decode(
            zero_width.data(), zero_width.size(), &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_IMAGE,
        "zero PNG width is rejected before decode");
    easycon_native_error_release(&error);

    auto decoded_limited = limits;
    decoded_limited.max_decoded_bytes = UINT64_C(11);
    expect(
        easycon_native_image_decode(
            png.data(), png.size(), &decoded_limited, &output, &error) ==
            EASYCON_NATIVE_STATUS_OUT_OF_RANGE,
        "decoded byte ceiling is enforced before decode");
    easycon_native_error_release(&error);

    const easycon_native_image_view invalid_format{
        expected_bgr.data(),
        expected_bgr.size(),
        UINT32_C(2),
        UINT32_C(2),
        UINT64_C(6),
        UINT32_C(99),
    };
    expect(
        easycon_native_image_convert(
            &invalid_format,
            EASYCON_NATIVE_PIXEL_FORMAT_BGR8,
            &limits,
            &output,
            &error) == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "unsupported input format is rejected");
    easycon_native_error_release(&error);

    auto oversized_view = view_of(easycon_native_image{
        const_cast<uint8_t*>(expected_bgr.data()),
        limits.max_decoded_bytes + 1,
        UINT32_C(2),
        UINT32_C(2),
        UINT64_C(6),
        EASYCON_NATIVE_PIXEL_FORMAT_BGR8,
    });
    expect(
        easycon_native_image_convert(
            &oversized_view,
            EASYCON_NATIVE_PIXEL_FORMAT_BGR8,
            &limits,
            &output,
            &error) == EASYCON_NATIVE_STATUS_OUT_OF_RANGE,
        "declared input length cannot exceed the decoded byte ceiling");
    easycon_native_image_release(&output);
    easycon_native_error_release(&error);

    auto oversized = png;
    oversized[16] = 0;
    oversized[17] = 1;
    oversized[18] = 0;
    oversized[19] = 0;
    expect(
        easycon_native_image_decode(
            oversized.data(), oversized.size(), &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_OUT_OF_RANGE,
        "oversized PNG header is rejected before decode");
    easycon_native_error_release(&error);

    const std::vector<uint8_t> invalid{1, 2, 3, 4};
    expect(
        easycon_native_image_decode(
            invalid.data(), invalid.size(), &limits, &output, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_IMAGE,
        "unknown encoded format is rejected");
    easycon_native_error_release(&error);
    easycon_native_image_release(&output);
    expect(counts().live_allocations == baseline.live_allocations, "codec failures do not leak");
}

void test_codec_zeroes_outputs_before_requiring_error_storage() {
    const auto limits = image_limits();
    auto* const sentinel = reinterpret_cast<uint8_t*>(UINTPTR_MAX);

    easycon_native_image decoded{sentinel, 1, 1, 1, 1, 1};
    expect(
        easycon_native_image_decode(nullptr, 0, &limits, &decoded, nullptr) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "decode requires error storage");
    expect(is_zero(decoded), "decode zeroes output before validating error storage");

    easycon_native_buffer encoded{sentinel, 1};
    expect(
        easycon_native_image_encode_png(nullptr, &limits, &encoded, nullptr) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "encode requires error storage");
    expect(is_zero(encoded), "encode zeroes output before validating error storage");

    easycon_native_image converted{sentinel, 1, 1, 1, 1, 1};
    expect(
        easycon_native_image_convert(
            nullptr,
            EASYCON_NATIVE_PIXEL_FORMAT_BGR8,
            &limits,
            &converted,
            nullptr) == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "convert requires error storage");
    expect(is_zero(converted), "convert zeroes output before validating error storage");

    easycon_native_image cropped{sentinel, 1, 1, 1, 1, 1};
    expect(
        easycon_native_image_crop(
            nullptr, 0, 0, 1, 1, &limits, &cropped, nullptr) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "crop requires error storage");
    expect(is_zero(cropped), "crop zeroes output before validating error storage");
}

}  // namespace

int main() {
    static_assert(sizeof(void*) == 8, "Phase 3 native bridge is x64-only");

    test_handle_lifecycle();
    test_status_error_and_zero_buffer();
    test_exception(
        EASYCON_NATIVE_TEST_RAISE_CV,
        EASYCON_NATIVE_STATUS_CV_EXCEPTION,
        "cv::Exception is isolated");
    test_exception(
        EASYCON_NATIVE_TEST_RAISE_STD,
        EASYCON_NATIVE_STATUS_STD_EXCEPTION,
        "std::exception is isolated");
    test_exception(
        EASYCON_NATIVE_TEST_RAISE_UNKNOWN,
        EASYCON_NATIVE_STATUS_UNKNOWN_EXCEPTION,
        "unknown exception is isolated");
    test_codec_round_trip_conversion_and_roi();
    test_codec_rejects_invalid_truncated_and_oversized_input();
    test_codec_zeroes_outputs_before_requiring_error_storage();

    const auto final_counts = counts();
    expect(final_counts.live_handles == 0, "all native handles are released");
    expect(final_counts.live_allocations == 0, "all native allocations are released");
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
