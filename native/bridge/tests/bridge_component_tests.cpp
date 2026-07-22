#include "internal/easycon_native_bridge.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <filesystem>
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

std::vector<uint8_t> read_hex_at(std::string_view directory, std::string_view name) {
    std::ifstream input(
        std::string("fixtures/") + std::string(directory) + "/" + std::string(name));
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

std::vector<uint8_t> read_hex(std::string_view name) {
    return read_hex_at("codec", name);
}

std::vector<uint8_t> read_operation_hex(std::string_view name) {
    return read_hex_at("operations", name);
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

bool is_zero(const easycon_native_match_extrema& result) {
    return result.min_value == 0.0 && result.max_value == 0.0 && result.min_x == 0 &&
           result.min_y == 0 && result.max_x == 0 && result.max_y == 0;
}

bool is_zero(const easycon_native_color_result& result) {
    return result.count == 0 && result.bbox_x == 0 && result.bbox_y == 0 &&
           result.bbox_width == 0 && result.bbox_height == 0 && result.has_bbox == 0;
}

easycon_native_capture_options capture_options() {
    return easycon_native_capture_options{
        image_limits(), UINT64_C(1000000000), UINT64_C(100000000), UINT32_C(8), 0};
}

void expect_pixels(
    const easycon_native_image& image,
    const std::vector<uint8_t>& expected,
    std::string_view label) {
    const auto same = image.length == expected.size() &&
                      std::equal(expected.begin(), expected.end(), image.data);
    if (!same) {
        const auto compared = (std::min)(static_cast<size_t>(image.length), expected.size());
        size_t index = 0;
        while (index < compared && expected[index] == image.data[index]) {
            ++index;
        }
        std::cerr << label << " lengths actual=" << image.length << " expected=" << expected.size();
        if (index < compared) {
            std::cerr << " first mismatch at " << index << " actual="
                      << static_cast<unsigned>(image.data[index]) << " expected="
                      << static_cast<unsigned>(expected[index]);
        }
        std::cerr << '\n';
    }
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

easycon_native_image_view gray_view(
    const std::vector<uint8_t>& pixels,
    uint32_t width,
    uint32_t height) {
    return easycon_native_image_view{
        pixels.data(),
        pixels.size(),
        width,
        height,
        width,
        EASYCON_NATIVE_PIXEL_FORMAT_GRAY8,
    };
}

std::vector<uint8_t> replicate_bgr(const std::vector<uint8_t>& gray) {
    std::vector<uint8_t> bgr;
    bgr.reserve(gray.size() * 3);
    for (const auto value : gray) {
        bgr.insert(bgr.end(), {value, value, value});
    }
    return bgr;
}

void test_normalized_template_extrema() {
    const auto search = read_operation_hex("template-search-gray.hex");
    const auto target = read_operation_hex("template-target-gray.hex");
    const auto search_view = gray_view(search, 6, 5);
    const auto target_view = gray_view(target, 3, 3);
    const auto limits = image_limits();
    easycon_native_error error{};
    const struct {
        uint32_t method;
        double raw;
        bool use_min;
    } cases[] = {
        {EASYCON_NATIVE_TEMPLATE_SQDIFF_NORMED, 0.005045493599027395, true},
        {EASYCON_NATIVE_TEMPLATE_CCORR_NORMED, 0.9981008172035217, false},
        {EASYCON_NATIVE_TEMPLATE_CCOEFF_NORMED, 0.9942602515220642, false},
    };
    for (const auto& item : cases) {
        easycon_native_match_extrema result{};
        expect(
            easycon_native_match_template(
                &search_view,
                &target_view,
                item.method,
                &limits,
                &result,
                &error) == EASYCON_NATIVE_STATUS_OK,
            "normalized template match succeeds");
        const auto raw = item.use_min ? result.min_value : result.max_value;
        const auto x = item.use_min ? result.min_x : result.max_x;
        const auto y = item.use_min ? result.min_y : result.max_y;
        expect(std::abs(raw - item.raw) <= 0.00001, "template raw extremum matches fixture");
        expect(x == 2 && y == 1, "template extremum location matches fixture");
    }

    easycon_native_match_extrema output{};
    const auto constant = std::vector<uint8_t>(16, 7);
    const auto constant_view = gray_view(constant, 4, 4);
    expect(
        easycon_native_match_template(
            &constant_view,
            &constant_view,
            EASYCON_NATIVE_TEMPLATE_CCOEFF_NORMED,
            &limits,
            &output,
            &error) == EASYCON_NATIVE_STATUS_BACKEND_ERROR,
        "undefined constant normalized coefficient is rejected");
    easycon_native_error_release(&error);
}

void test_edge_preprocess_and_match_fixtures() {
    const auto search_gray = read_operation_hex("edge-search-gray.hex");
    const auto target_gray = read_operation_hex("edge-target-gray.hex");
    const auto search = replicate_bgr(search_gray);
    const auto target = replicate_bgr(target_gray);
    const easycon_native_image_view search_view{
        search.data(), search.size(), 18, 17, 54, EASYCON_NATIVE_PIXEL_FORMAT_BGR8};
    const easycon_native_image_view target_view{
        target.data(), target.size(), 11, 11, 33, EASYCON_NATIVE_PIXEL_FORMAT_BGR8};
    const auto limits = image_limits();
    easycon_native_error error{};
    const struct {
        uint32_t method;
        std::string_view search_expected;
        std::string_view target_expected;
    } cases[] = {
        {EASYCON_NATIVE_EDGE_XY, "edge-search-xy-gray.hex", "edge-target-xy-gray.hex"},
        {EASYCON_NATIVE_EDGE_LAPLACIAN,
         "edge-search-laplacian-gray.hex",
         "edge-target-laplacian-gray.hex"},
    };
    for (const auto& item : cases) {
        easycon_native_image search_edge{};
        easycon_native_image target_edge{};
        expect(
            easycon_native_edge_preprocess(
                &search_view, item.method, &limits, &search_edge, &error) ==
                EASYCON_NATIVE_STATUS_OK,
            "search edge preprocess succeeds");
        expect(
            easycon_native_edge_preprocess(
                &target_view, item.method, &limits, &target_edge, &error) ==
                EASYCON_NATIVE_STATUS_OK,
            "target edge preprocess succeeds");
        expect_pixels(
            search_edge,
            read_operation_hex(item.search_expected),
            item.method == EASYCON_NATIVE_EDGE_XY
                ? "XY search edge pixels match independent fixture"
                : "Laplacian search edge pixels match independent fixture");
        expect_pixels(
            target_edge,
            read_operation_hex(item.target_expected),
            item.method == EASYCON_NATIVE_EDGE_XY
                ? "XY target edge pixels match independent fixture"
                : "Laplacian target edge pixels match independent fixture");

        const auto search_edge_view = view_of(search_edge);
        const auto target_edge_view = view_of(target_edge);
        easycon_native_match_extrema result{};
        expect(
            easycon_native_match_template(
                &search_edge_view,
                &target_edge_view,
                EASYCON_NATIVE_TEMPLATE_CCOEFF_NORMED,
                &limits,
                &result,
                &error) == EASYCON_NATIVE_STATUS_OK,
            "edge template match succeeds");
        expect(result.max_x == 4 && result.max_y == 3, "edge match location is exact");
        expect(std::abs(result.max_value - 1.0) <= 0.00001, "edge raw match is exact");
        easycon_native_image_release(&search_edge);
        easycon_native_image_release(&target_edge);
    }
}

void test_hsv_count_wrap_and_bbox() {
    const auto bgr = read_operation_hex("hsv-bgr-5x3.hex");
    const easycon_native_image_view image{
        bgr.data(), bgr.size(), 5, 3, 15, EASYCON_NATIVE_PIXEL_FORMAT_BGR8};
    const auto limits = image_limits();
    easycon_native_error error{};
    const struct {
        easycon_native_hsv_range range;
        uint64_t count;
        uint32_t x;
        uint32_t y;
        uint32_t width;
        uint32_t height;
        uint32_t has_bbox;
    } cases[] = {
        {{20, 100, 200, 255, 200, 255}, 3, 1, 0, 3, 1, 1},
        {{170, 10, 100, 255, 100, 255}, 5, 1, 1, 3, 2, 1},
        {{0, 179, 0, 255, 0, 255}, 12, 0, 0, 4, 3, 1},
        {{101, 110, 200, 255, 200, 255}, 0, 0, 0, 0, 0, 0},
    };
    for (const auto& item : cases) {
        easycon_native_color_result result{};
        expect(
            easycon_native_hsv_count(
                &image, 1, 0, 4, 3, &item.range, &limits, &result, &error) ==
                EASYCON_NATIVE_STATUS_OK,
            "HSV statistics succeed");
        expect(result.count == item.count, "HSV count matches fixture");
        expect(
            result.has_bbox == item.has_bbox && result.bbox_x == item.x &&
                result.bbox_y == item.y && result.bbox_width == item.width &&
                result.bbox_height == item.height,
            "HSV relative bounding box matches fixture");
    }

    easycon_native_color_result result{};
    const easycon_native_hsv_range reversed{0, 179, 200, 100, 0, 255};
    expect(
        easycon_native_hsv_count(
            &image, 1, 0, 4, 3, &reversed, &limits, &result, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "reversed saturation range is rejected");
    easycon_native_error_release(&error);
    expect(
        easycon_native_hsv_count(
            &image, 1, 0, 0, 3, &cases[0].range, &limits, &result, &error) ==
            EASYCON_NATIVE_STATUS_OUT_OF_RANGE,
        "empty HSV ROI is rejected");
    easycon_native_error_release(&error);
}

void test_vision_ops_formats_validation_and_output_zeroing() {
    const auto bgr = read_operation_hex("hsv-bgr-5x3.hex");
    std::vector<uint8_t> bgra;
    bgra.reserve(size_t{5} * 3 * 4);
    for (size_t index = 0; index < bgr.size(); index += 3) {
        bgra.insert(bgra.end(), {bgr[index], bgr[index + 1], bgr[index + 2], UINT8_C(77)});
    }
    const easycon_native_image_view bgra_view{
        bgra.data(), bgra.size(), 5, 3, 20, EASYCON_NATIVE_PIXEL_FORMAT_BGRA8};
    const auto gray = read_operation_hex("template-search-gray.hex");
    const auto gray_image = gray_view(gray, 6, 5);
    const auto limits = image_limits();
    const easycon_native_hsv_range full{0, 179, 0, 255, 0, 255};
    easycon_native_error error{};
    easycon_native_color_result color{};
    expect(
        easycon_native_hsv_count(
            &bgra_view, 0, 0, 5, 3, &full, &limits, &color, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "BGRA converts to BGR before HSV");
    expect(color.count == 15, "BGRA full HSV range counts every pixel");
    expect(
        easycon_native_hsv_count(
            &gray_image, 0, 0, 6, 5, &full, &limits, &color, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "Gray converts to BGR before HSV");
    expect(color.count == 30, "Gray full HSV range counts every pixel");

    const auto search = read_operation_hex("template-search-gray.hex");
    const auto target = read_operation_hex("template-target-gray.hex");
    const auto search_view = gray_view(search, 6, 5);
    const auto target_view = gray_view(target, 3, 3);
    easycon_native_match_extrema extrema{};
    expect(
        easycon_native_match_template(
            &search_view, &target_view, 99, &limits, &extrema, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "unknown template mode is rejected");
    expect(is_zero(extrema), "unknown template mode leaves result zero");
    easycon_native_error_release(&error);

    auto* const sentinel = reinterpret_cast<uint8_t*>(UINTPTR_MAX);
    extrema = easycon_native_match_extrema{1.0, 1.0, 1, 1, 1, 1};
    expect(
        easycon_native_match_template(
            &search_view,
            &target_view,
            EASYCON_NATIVE_TEMPLATE_CCORR_NORMED,
            &limits,
            &extrema,
            nullptr) == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "template match requires error storage");
    expect(is_zero(extrema), "template match zeroes output before error storage validation");

    easycon_native_image edge{sentinel, 1, 1, 1, 1, 1};
    expect(
        easycon_native_edge_preprocess(
            &search_view, EASYCON_NATIVE_EDGE_XY, &limits, &edge, nullptr) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "edge preprocess requires error storage");
    expect(is_zero(edge), "edge preprocess zeroes output before error storage validation");

    color = easycon_native_color_result{1, 1, 1, 1, 1, 1};
    expect(
        easycon_native_hsv_count(
            &bgra_view, 0, 0, 5, 3, &full, &limits, &color, nullptr) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "HSV count requires error storage");
    expect(is_zero(color), "HSV count zeroes output before error storage validation");
}

void test_capture_file_admission_interrupt_and_ownership() {
    const auto baseline = counts();
    const auto source =
        std::filesystem::absolute("fixtures/no-file-access/frame-%02d.bmp").u8string();
    const auto options = capture_options();
    easycon_native_capture* capture = nullptr;
    easycon_native_capture_interrupt* interrupt = nullptr;
    easycon_native_error error{};

    expect(
        easycon_native_capture_create(
            EASYCON_NATIVE_CAPTURE_BACKEND_FILE,
            reinterpret_cast<const uint8_t*>(source.data()),
            source.size(),
            &options,
            &capture,
            &interrupt,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "file capture owner and interrupt are created");
    expect(capture != nullptr && interrupt != nullptr, "capture create returns both owners");
    expect(counts().live_handles == baseline.live_handles + 2, "capture owners are counted");

    easycon_native_capture_profile profile{1, 1, 1, 1, 1, 1};
    expect(
        easycon_native_capture_open(capture, &profile, &error) ==
            EASYCON_NATIVE_STATUS_UNSUPPORTED,
        "file capture is rejected before unbounded path access");
    expect(
        profile.backend == 0 && profile.width == 0 && profile.height == 0 && profile.stride == 0 &&
            profile.pixel_format == 0 && profile.frame_interval_ns == 0,
        "unsupported file capture leaves the profile empty");
    easycon_native_error_release(&error);

    expect(
        easycon_native_capture_interrupt_request(interrupt, &error) == EASYCON_NATIVE_STATUS_OK,
        "capture interrupt is lock-free from the reader owner");
    expect(
        easycon_native_capture_close(capture, &error) == EASYCON_NATIVE_STATUS_OK,
        "capture closes on its owner thread");
    expect(
        easycon_native_capture_destroy(&capture, &error) == EASYCON_NATIVE_STATUS_OK,
        "capture destroy consumes the owner");
    expect(capture == nullptr, "capture destroy acknowledges consumption");
    expect(
        easycon_native_capture_interrupt_destroy(&interrupt, &error) == EASYCON_NATIVE_STATUS_OK,
        "interrupt destroy consumes the token");
    expect(interrupt == nullptr, "interrupt destroy acknowledges consumption");
    expect(counts().live_handles == baseline.live_handles, "capture handles return to baseline");
    expect(counts().live_allocations == baseline.live_allocations, "capture allocations return to baseline");
}

void test_capture_hardware_admission_and_destroy_outcomes() {
    const auto baseline = counts();
    const std::string source = "dshow:0";
    const auto options = capture_options();
    easycon_native_capture* capture = nullptr;
    easycon_native_capture_interrupt* interrupt = nullptr;
    easycon_native_capture_profile profile{};
    easycon_native_error error{};

    expect(
        easycon_native_capture_create(
            EASYCON_NATIVE_CAPTURE_BACKEND_DIRECTSHOW,
            reinterpret_cast<const uint8_t*>(source.data()),
            source.size(),
            &options,
            &capture,
            &interrupt,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "DShow capture request is represented without opening hardware");
    expect(
        easycon_native_capture_open(capture, &profile, &error) ==
            EASYCON_NATIVE_STATUS_UNSUPPORTED,
        "DShow is rejected before OpenCV open without bounded timeout capability");
    expect(profile.width == 0 && profile.height == 0, "unsupported open leaves profile zero");
    easycon_native_error_release(&error);

    expect(
        easycon_native_test_capture_fail_next(
            EASYCON_NATIVE_TEST_CAPTURE_DESTROY_BEFORE_CONSUME, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "destroy-before-consume failpoint arms");
    expect(
        easycon_native_capture_destroy(&capture, &error) ==
            EASYCON_NATIVE_STATUS_BACKEND_ERROR,
        "destroy-before-consume returns a diagnostic");
    expect(capture != nullptr, "destroy-before-consume preserves the owner");
    easycon_native_error_release(&error);

    expect(
        easycon_native_test_capture_fail_next(
            EASYCON_NATIVE_TEST_CAPTURE_CONSUME_WITH_ERROR, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "consume-with-error failpoint arms");
    expect(
        easycon_native_capture_destroy(&capture, &error) ==
            EASYCON_NATIVE_STATUS_BACKEND_ERROR,
        "consume-with-error returns its diagnostic");
    expect(capture == nullptr, "consume-with-error still acknowledges ownership consumption");
    easycon_native_error_release(&error);
    expect(
        easycon_native_capture_interrupt_destroy(&interrupt, &error) == EASYCON_NATIVE_STATUS_OK,
        "hardware interrupt owner is released");

    const std::string mf_source = "msmf:0";
    expect(
        easycon_native_capture_create(
            EASYCON_NATIVE_CAPTURE_BACKEND_MEDIA_FOUNDATION,
            reinterpret_cast<const uint8_t*>(mf_source.data()),
            mf_source.size(),
            &options,
            &capture,
            &interrupt,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "MSMF capture request is represented without opening hardware");
    expect(
        easycon_native_test_capture_fail_next(
            EASYCON_NATIVE_TEST_CAPTURE_DESTROY_OK_NO_ACK, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "OK-without-ack failpoint arms");
    expect(
        easycon_native_capture_destroy(&capture, &error) == EASYCON_NATIVE_STATUS_OK,
        "protocol no-ack can accompany OK status");
    expect(capture != nullptr, "OK without pointer clear preserves the only owner");
    expect(
        easycon_native_capture_destroy(&capture, &error) == EASYCON_NATIVE_STATUS_OK,
        "retry consumes a no-ack owner");
    expect(capture == nullptr, "retry acknowledges capture consumption");
    expect(
        easycon_native_capture_interrupt_destroy(&interrupt, &error) == EASYCON_NATIVE_STATUS_OK,
        "MSMF interrupt owner is released");
    expect(counts().live_handles == baseline.live_handles, "capture failpoints preserve handle symmetry");
    expect(counts().live_allocations == baseline.live_allocations, "capture diagnostics are released");
}

void test_capture_discovery_is_bounded_and_owned() {
    const auto baseline = counts();
    for (const auto backend : {
             EASYCON_NATIVE_CAPTURE_BACKEND_DIRECTSHOW,
             EASYCON_NATIVE_CAPTURE_BACKEND_MEDIA_FOUNDATION,
         }) {
        easycon_native_capture_discovery* discovery = nullptr;
        easycon_native_error error{};
        expect(
            easycon_native_capture_discovery_create(backend, &discovery, &error) ==
                EASYCON_NATIVE_STATUS_OK,
            "Windows capture discovery succeeds even when no device is present");
        expect(discovery != nullptr, "capture discovery returns an owner");
        uint32_t length = 0;
        expect(
            easycon_native_capture_discovery_count(discovery, &length, &error) ==
                EASYCON_NATIVE_STATUS_OK,
            "capture discovery count succeeds");
        expect(length <= 64, "capture discovery is bounded");
        for (uint32_t index = 0; index < length; ++index) {
            easycon_native_buffer source{};
            easycon_native_buffer name{};
            expect(
                easycon_native_capture_discovery_get(
                    discovery, index, &source, &name, &error) == EASYCON_NATIVE_STATUS_OK,
                "capture descriptor copies through bridge-owned buffers");
            expect(source.data != nullptr && source.length > 0 && source.length <= 4096,
                   "capture source ID is bounded UTF-8");
            expect(name.data != nullptr && name.length > 0 && name.length <= 1024,
                   "capture display name is bounded UTF-8");
            easycon_native_buffer_release(&source);
            easycon_native_buffer_release(&name);
        }
        expect(
            easycon_native_capture_discovery_destroy(&discovery, &error) ==
                EASYCON_NATIVE_STATUS_OK,
            "capture discovery destroy succeeds");
        expect(discovery == nullptr, "capture discovery destroy consumes its owner");
    }
    expect(counts().live_handles == baseline.live_handles, "discovery handles return to baseline");
    expect(counts().live_allocations == baseline.live_allocations,
           "discovery buffers return to baseline");
}

void test_capture_exception_isolation_and_retry() {
    const auto baseline = counts();
    const auto source =
        std::filesystem::absolute("fixtures/no-file-access/frame-%02d.bmp").u8string();
    const auto options = capture_options();
    easycon_native_capture* capture = nullptr;
    easycon_native_capture_interrupt* interrupt = nullptr;
    easycon_native_capture_profile profile{1, 1, 1, 1, 1, 1};
    easycon_native_error error{};
    expect(
        easycon_native_capture_create(
            EASYCON_NATIVE_CAPTURE_BACKEND_FILE,
            reinterpret_cast<const uint8_t*>(source.data()),
            source.size(),
            &options,
            &capture,
            &interrupt,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "capture exception test owners are created");

    expect(
        easycon_native_test_capture_fail_next(EASYCON_NATIVE_TEST_CAPTURE_OPEN_CV, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "capture open cv failpoint arms");
    expect(
        easycon_native_capture_open(capture, &profile, &error) ==
            EASYCON_NATIVE_STATUS_CV_EXCEPTION,
        "capture open isolates cv::Exception");
    expect(profile.width == 0 && profile.height == 0, "open exception zeroes profile");
    easycon_native_error_release(&error);
    expect(
        easycon_native_capture_open(capture, &profile, &error) ==
            EASYCON_NATIVE_STATUS_UNSUPPORTED,
        "capture open returns the stable unqualified result after isolated exception");
    easycon_native_error_release(&error);

    expect(
        easycon_native_test_capture_fail_next(EASYCON_NATIVE_TEST_CAPTURE_READ_STD, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "capture read std failpoint arms");
    easycon_native_image frame{reinterpret_cast<uint8_t*>(UINTPTR_MAX), 1, 1, 1, 1, 1};
    expect(
        easycon_native_capture_read(capture, &frame, &error) ==
            EASYCON_NATIVE_STATUS_STD_EXCEPTION,
        "capture read isolates std::exception");
    expect(is_zero(frame), "read exception zeroes image output");
    easycon_native_error_release(&error);
    expect(
        easycon_native_capture_read(capture, &frame, &error) ==
            EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "capture read remains invalid after unsupported open");
    expect(is_zero(frame), "invalid capture read leaves image empty");
    easycon_native_error_release(&error);

    expect(
        easycon_native_test_capture_fail_next(EASYCON_NATIVE_TEST_CAPTURE_CLOSE_UNKNOWN, &error) ==
            EASYCON_NATIVE_STATUS_OK,
        "capture close unknown failpoint arms");
    expect(
        easycon_native_capture_close(capture, &error) ==
            EASYCON_NATIVE_STATUS_UNKNOWN_EXCEPTION,
        "capture close isolates unknown exception");
    easycon_native_error_release(&error);
    expect(
        easycon_native_capture_close(capture, &error) == EASYCON_NATIVE_STATUS_OK,
        "capture close retries after isolated exception");
    expect(
        easycon_native_capture_destroy(&capture, &error) == EASYCON_NATIVE_STATUS_OK,
        "capture exception owner is destroyed");
    expect(
        easycon_native_capture_interrupt_destroy(&interrupt, &error) == EASYCON_NATIVE_STATUS_OK,
        "capture exception interrupt is destroyed");
    expect(counts().live_handles == baseline.live_handles,
           "capture exception handles return to baseline");
    expect(counts().live_allocations == baseline.live_allocations,
           "capture exception diagnostics return to baseline");
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
    test_normalized_template_extrema();
    test_edge_preprocess_and_match_fixtures();
    test_hsv_count_wrap_and_bbox();
    test_vision_ops_formats_validation_and_output_zeroing();
    test_capture_file_admission_interrupt_and_ownership();
    test_capture_hardware_admission_and_destroy_outcomes();
    test_capture_discovery_is_bounded_and_owned();
    test_capture_exception_isolation_and_retry();

    const auto final_counts = counts();
    expect(final_counts.live_handles == 0, "all native handles are released");
    expect(final_counts.live_allocations == 0, "all native allocations are released");
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
