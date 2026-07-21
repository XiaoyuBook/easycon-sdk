#include "bridge_internal.hpp"

#include <climits>
#include <cstdint>
#include <cstring>
#include <limits>
#include <vector>

#include <opencv2/imgcodecs.hpp>
#include <opencv2/imgproc.hpp>
#include <zlib.h>

namespace {

constexpr uint64_t max_encoded_ceiling = UINT64_C(64) * 1024 * 1024;
constexpr uint32_t max_width_ceiling = UINT32_C(16384);
constexpr uint32_t max_height_ceiling = UINT32_C(16384);
constexpr uint64_t max_pixels_ceiling = UINT64_C(67108864);
constexpr uint64_t max_decoded_ceiling = UINT64_C(256) * 1024 * 1024;
constexpr uint64_t max_stride_ceiling = UINT64_C(65536);
constexpr uint64_t png_chunk_payload = UINT64_C(8192);

easycon_native_status fail(
    easycon_native_error* error,
    easycon_native_status status,
    std::string_view message) noexcept {
    return easycon::native::detail::set_error(error, status, message);
}

bool checked_add(uint64_t left, uint64_t right, uint64_t& result) noexcept {
    if (left > (std::numeric_limits<uint64_t>::max)() - right) {
        return false;
    }
    result = left + right;
    return true;
}

bool checked_multiply(uint64_t left, uint64_t right, uint64_t& result) noexcept {
    if (left != 0 && right > (std::numeric_limits<uint64_t>::max)() / left) {
        return false;
    }
    result = left * right;
    return true;
}

uint16_t read_u16_le(const uint8_t* data) noexcept {
    return static_cast<uint16_t>(data[0]) |
           static_cast<uint16_t>(static_cast<uint16_t>(data[1]) << 8U);
}

uint32_t read_u32_le(const uint8_t* data) noexcept {
    return static_cast<uint32_t>(data[0]) |
           (static_cast<uint32_t>(data[1]) << 8U) |
           (static_cast<uint32_t>(data[2]) << 16U) |
           (static_cast<uint32_t>(data[3]) << 24U);
}

uint32_t read_u32_be(const uint8_t* data) noexcept {
    return (static_cast<uint32_t>(data[0]) << 24U) |
           (static_cast<uint32_t>(data[1]) << 16U) |
           (static_cast<uint32_t>(data[2]) << 8U) |
           static_cast<uint32_t>(data[3]);
}

int32_t read_i32_le(const uint8_t* data) noexcept {
    const auto value = read_u32_le(data);
    int32_t result{};
    static_assert(sizeof(result) == sizeof(value));
    std::memcpy(&result, &value, sizeof(result));
    return result;
}

easycon_native_status validate_limits(
    const easycon_native_image_limits* limits,
    easycon_native_error* error) noexcept {
    if (limits == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "image limits are null");
    }
    if (limits->max_encoded_bytes == 0 || limits->max_width == 0 || limits->max_height == 0 ||
        limits->max_pixels == 0 || limits->max_decoded_bytes == 0 || limits->max_stride == 0) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "image limits must be non-zero");
    }
    if (limits->max_encoded_bytes > max_encoded_ceiling || limits->max_width > max_width_ceiling ||
        limits->max_height > max_height_ceiling || limits->max_pixels > max_pixels_ceiling ||
        limits->max_decoded_bytes > max_decoded_ceiling || limits->max_stride > max_stride_ceiling) {
        return fail(error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "image limits exceed hard ceilings");
    }
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status validate_dimensions(
    uint32_t width,
    uint32_t height,
    uint32_t channels,
    const easycon_native_image_limits& limits,
    easycon_native_error* error) noexcept {
    if (width == 0 || height == 0) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "image dimensions are zero");
    }
    if (width > limits.max_width || height > limits.max_height || width > INT_MAX ||
        height > INT_MAX) {
        return fail(error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "image dimensions exceed limits");
    }
    uint64_t pixels{};
    uint64_t row_bytes{};
    uint64_t decoded_bytes{};
    if (!checked_multiply(width, height, pixels) || !checked_multiply(width, channels, row_bytes) ||
        !checked_multiply(row_bytes, height, decoded_bytes)) {
        return fail(error, EASYCON_NATIVE_STATUS_OVERFLOW, "image dimensions overflow");
    }
    if (pixels > limits.max_pixels || row_bytes > limits.max_stride ||
        decoded_bytes > limits.max_decoded_bytes) {
        return fail(error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "decoded image exceeds limits");
    }
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status preflight_bmp(
    const uint8_t* encoded,
    uint64_t length,
    const easycon_native_image_limits& limits,
    easycon_native_error* error) noexcept {
    if (length < 54) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "BMP header is truncated");
    }
    const auto file_size = read_u32_le(encoded + 2);
    const auto pixel_offset = read_u32_le(encoded + 10);
    const auto dib_size = read_u32_le(encoded + 14);
    if (dib_size < 40 || static_cast<uint64_t>(dib_size) + 14 > length || pixel_offset > length ||
        pixel_offset < static_cast<uint64_t>(dib_size) + 14) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "BMP layout is invalid");
    }
    const auto signed_width = read_i32_le(encoded + 18);
    const auto signed_height = read_i32_le(encoded + 22);
    if (signed_width <= 0 || signed_height == 0 || signed_height == (std::numeric_limits<int32_t>::min)()) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "BMP dimensions are invalid");
    }
    const auto width = static_cast<uint32_t>(signed_width);
    const auto height = static_cast<uint32_t>(signed_height < 0 ? -signed_height : signed_height);
    const auto planes = read_u16_le(encoded + 26);
    const auto bits_per_pixel = read_u16_le(encoded + 28);
    const auto compression = read_u32_le(encoded + 30);
    if (planes != 1 || (bits_per_pixel != 8 && bits_per_pixel != 24 && bits_per_pixel != 32) ||
        compression != 0) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "BMP encoding is unsupported");
    }
    const auto channels = bits_per_pixel == 24 ? UINT32_C(3) : UINT32_C(4);
    const auto dimension_status = validate_dimensions(width, height, channels, limits, error);
    if (dimension_status != EASYCON_NATIVE_STATUS_OK) {
        return dimension_status;
    }
    uint64_t row_bits{};
    uint64_t padded_bits{};
    uint64_t row_bytes{};
    uint64_t pixel_bytes{};
    uint64_t pixel_end{};
    if (!checked_multiply(width, bits_per_pixel, row_bits) || !checked_add(row_bits, 31, padded_bits) ||
        !checked_multiply(padded_bits / 32, 4, row_bytes) ||
        !checked_multiply(row_bytes, height, pixel_bytes) ||
        !checked_add(pixel_offset, pixel_bytes, pixel_end)) {
        return fail(error, EASYCON_NATIVE_STATUS_OVERFLOW, "BMP storage size overflows");
    }
    if (pixel_end > length || file_size < pixel_end || file_size > length) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "BMP pixel data is truncated");
    }
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status preflight_png(
    const uint8_t* encoded,
    uint64_t length,
    const easycon_native_image_limits& limits,
    easycon_native_error* error) noexcept {
    if (length < 33) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "PNG header is truncated");
    }
    if (read_u32_be(encoded + 8) != 13 || std::memcmp(encoded + 12, "IHDR", 4) != 0) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "PNG IHDR is invalid");
    }
    const auto width = read_u32_be(encoded + 16);
    const auto height = read_u32_be(encoded + 20);
    const auto bit_depth = encoded[24];
    const auto color_type = encoded[25];
    const auto compression = encoded[26];
    const auto filter = encoded[27];
    const auto interlace = encoded[28];
    if (bit_depth != 8 ||
        (color_type != 0 && color_type != 2 && color_type != 4 && color_type != 6) ||
        compression != 0 || filter != 0 || interlace > 1) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "PNG encoding is unsupported");
    }
    const auto channels = color_type == 0 ? UINT32_C(1) : (color_type == 2 ? UINT32_C(3) : UINT32_C(4));
    return validate_dimensions(width, height, channels, limits, error);
}

easycon_native_status preflight_encoded(
    const uint8_t* encoded,
    uint64_t length,
    const easycon_native_image_limits& limits,
    easycon_native_error* error) noexcept {
    if (length == 0 || encoded == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "encoded image is empty");
    }
    if (length > limits.max_encoded_bytes || length > max_encoded_ceiling ||
        length > static_cast<uint64_t>((std::numeric_limits<int>::max)())) {
        return fail(error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "encoded image exceeds limits");
    }
    if (length >= 2 && encoded[0] == 'B' && encoded[1] == 'M') {
        return preflight_bmp(encoded, length, limits, error);
    }
    constexpr uint8_t png_signature[] = {0x89, 'P', 'N', 'G', 0x0D, 0x0A, 0x1A, 0x0A};
    if (length >= sizeof(png_signature) &&
        std::memcmp(encoded, png_signature, sizeof(png_signature)) == 0) {
        return preflight_png(encoded, length, limits, error);
    }
    return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "encoded image format is unsupported");
}

uint32_t channels_for_format(uint32_t format) noexcept {
    switch (format) {
        case EASYCON_NATIVE_PIXEL_FORMAT_BGR8:
            return 3;
        case EASYCON_NATIVE_PIXEL_FORMAT_BGRA8:
            return 4;
        case EASYCON_NATIVE_PIXEL_FORMAT_GRAY8:
            return 1;
        default:
            return 0;
    }
}

int mat_type_for_format(uint32_t format) noexcept {
    switch (format) {
        case EASYCON_NATIVE_PIXEL_FORMAT_BGR8:
            return CV_8UC3;
        case EASYCON_NATIVE_PIXEL_FORMAT_BGRA8:
            return CV_8UC4;
        case EASYCON_NATIVE_PIXEL_FORMAT_GRAY8:
            return CV_8UC1;
        default:
            return -1;
    }
}

uint32_t format_for_channels(int channels) noexcept {
    switch (channels) {
        case 1:
            return EASYCON_NATIVE_PIXEL_FORMAT_GRAY8;
        case 3:
            return EASYCON_NATIVE_PIXEL_FORMAT_BGR8;
        case 4:
            return EASYCON_NATIVE_PIXEL_FORMAT_BGRA8;
        default:
            return 0;
    }
}

easycon_native_status validate_view(
    const easycon_native_image_view* image,
    const easycon_native_image_limits& limits,
    cv::Mat& out_mat,
    easycon_native_error* error) {
    if (image == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "image view is null");
    }
    const auto channels = channels_for_format(image->pixel_format);
    if (channels == 0 || image->data == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "image view is invalid");
    }
    const auto dimension_status =
        validate_dimensions(image->width, image->height, channels, limits, error);
    if (dimension_status != EASYCON_NATIVE_STATUS_OK) {
        return dimension_status;
    }
    uint64_t row_bytes{};
    uint64_t required{};
    if (!checked_multiply(image->width, channels, row_bytes) ||
        !checked_multiply(image->stride, image->height, required)) {
        return fail(error, EASYCON_NATIVE_STATUS_OVERFLOW, "image view layout overflows");
    }
    if (image->stride > limits.max_stride || image->length > limits.max_decoded_bytes ||
        required > limits.max_decoded_bytes) {
        return fail(error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "image view exceeds limits");
    }
    if (image->stride < row_bytes || required > image->length ||
        image->stride > static_cast<uint64_t>((std::numeric_limits<size_t>::max)())) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "image view layout is invalid");
    }
    out_mat = cv::Mat(
        static_cast<int>(image->height),
        static_cast<int>(image->width),
        mat_type_for_format(image->pixel_format),
        const_cast<uint8_t*>(image->data),
        static_cast<size_t>(image->stride));
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status copy_mat(
    const cv::Mat& mat,
    const easycon_native_image_limits& limits,
    easycon_native_image* output,
    easycon_native_error* error) noexcept {
    if (mat.empty() || mat.depth() != CV_8U) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "decoded image is empty or non-8-bit");
    }
    const auto format = format_for_channels(mat.channels());
    if (format == 0 || mat.cols <= 0 || mat.rows <= 0) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "decoded image channels are unsupported");
    }
    const auto width = static_cast<uint32_t>(mat.cols);
    const auto height = static_cast<uint32_t>(mat.rows);
    const auto channels = static_cast<uint32_t>(mat.channels());
    const auto dimension_status = validate_dimensions(width, height, channels, limits, error);
    if (dimension_status != EASYCON_NATIVE_STATUS_OK) {
        return dimension_status;
    }
    uint64_t row_bytes{};
    uint64_t length{};
    if (!checked_multiply(width, channels, row_bytes) || !checked_multiply(row_bytes, height, length) ||
        length > static_cast<uint64_t>((std::numeric_limits<size_t>::max)())) {
        return fail(error, EASYCON_NATIVE_STATUS_OVERFLOW, "decoded image copy size overflows");
    }
    auto* data = easycon::native::detail::allocate_bytes(static_cast<size_t>(length));
    if (data == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_ALLOCATION_FAILED, "image allocation failed");
    }
    for (uint32_t row = 0; row < height; ++row) {
        std::memcpy(
            data + static_cast<size_t>(row * row_bytes),
            mat.ptr(static_cast<int>(row)),
            static_cast<size_t>(row_bytes));
    }
    output->data = reinterpret_cast<uint8_t*>(data);
    output->length = length;
    output->width = width;
    output->height = height;
    output->stride = row_bytes;
    output->pixel_format = format;
    return EASYCON_NATIVE_STATUS_OK;
}

int conversion_code(uint32_t input, uint32_t output) noexcept {
    if (input == EASYCON_NATIVE_PIXEL_FORMAT_BGR8 && output == EASYCON_NATIVE_PIXEL_FORMAT_BGRA8) {
        return cv::COLOR_BGR2BGRA;
    }
    if (input == EASYCON_NATIVE_PIXEL_FORMAT_BGR8 && output == EASYCON_NATIVE_PIXEL_FORMAT_GRAY8) {
        return cv::COLOR_BGR2GRAY;
    }
    if (input == EASYCON_NATIVE_PIXEL_FORMAT_BGRA8 && output == EASYCON_NATIVE_PIXEL_FORMAT_BGR8) {
        return cv::COLOR_BGRA2BGR;
    }
    if (input == EASYCON_NATIVE_PIXEL_FORMAT_BGRA8 && output == EASYCON_NATIVE_PIXEL_FORMAT_GRAY8) {
        return cv::COLOR_BGRA2GRAY;
    }
    if (input == EASYCON_NATIVE_PIXEL_FORMAT_GRAY8 && output == EASYCON_NATIVE_PIXEL_FORMAT_BGR8) {
        return cv::COLOR_GRAY2BGR;
    }
    if (input == EASYCON_NATIVE_PIXEL_FORMAT_GRAY8 && output == EASYCON_NATIVE_PIXEL_FORMAT_BGRA8) {
        return cv::COLOR_GRAY2BGRA;
    }
    return -1;
}

}  // namespace

namespace easycon::native::detail {

easycon_native_status validate_image_limits(
    const easycon_native_image_limits* limits,
    easycon_native_error* error) noexcept {
    return validate_limits(limits, error);
}

easycon_native_status make_image_view(
    const easycon_native_image_view* image,
    const easycon_native_image_limits& limits,
    cv::Mat& out_mat,
    easycon_native_error* error) {
    return validate_view(image, limits, out_mat, error);
}

easycon_native_status copy_image_mat(
    const cv::Mat& mat,
    const easycon_native_image_limits& limits,
    easycon_native_image* output,
    easycon_native_error* error) noexcept {
    return copy_mat(mat, limits, output, error);
}

}  // namespace easycon::native::detail

extern "C" void EASYCON_NATIVE_CALL easycon_native_image_release(
    easycon_native_image* image) noexcept {
    if (image == nullptr) {
        return;
    }
    easycon::native::detail::release_bytes(image->data);
    *image = {};
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_decode(
    const uint8_t* encoded,
    uint64_t encoded_length,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) noexcept {
    if (out_image != nullptr) {
        *out_image = {};
    }
    return easycon::native::detail::guard(out_error, [=]() {
        if (out_image == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "out_image is null");
        }
        const auto limit_status = validate_limits(limits, out_error);
        if (limit_status != EASYCON_NATIVE_STATUS_OK) {
            return limit_status;
        }
        const auto preflight_status = preflight_encoded(encoded, encoded_length, *limits, out_error);
        if (preflight_status != EASYCON_NATIVE_STATUS_OK) {
            return preflight_status;
        }
        cv::Mat encoded_view(
            1,
            static_cast<int>(encoded_length),
            CV_8UC1,
            const_cast<uint8_t*>(encoded));
        const auto decoded = cv::imdecode(encoded_view, cv::IMREAD_UNCHANGED);
        return copy_mat(decoded, *limits, out_image, out_error);
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_encode_png(
    const easycon_native_image_view* image,
    const easycon_native_image_limits* limits,
    easycon_native_buffer* out_encoded,
    easycon_native_error* out_error) noexcept {
    if (out_encoded != nullptr) {
        *out_encoded = {};
    }
    return easycon::native::detail::guard(out_error, [=]() {
        if (out_encoded == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "out_encoded is null");
        }
        const auto limit_status = validate_limits(limits, out_error);
        if (limit_status != EASYCON_NATIVE_STATUS_OK) {
            return limit_status;
        }
        cv::Mat mat;
        const auto view_status = validate_view(image, *limits, mat, out_error);
        if (view_status != EASYCON_NATIVE_STATUS_OK) {
            return view_status;
        }
        const auto channels = channels_for_format(image->pixel_format);
        uint64_t row_bytes{};
        uint64_t scanline{};
        uint64_t raw_size{};
        if (!checked_multiply(image->width, channels, row_bytes) || !checked_add(row_bytes, 1, scanline) ||
            !checked_multiply(scanline, image->height, raw_size)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OVERFLOW, "PNG raw size overflows");
        }
        if (raw_size > (std::numeric_limits<uLong>::max)()) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "PNG raw size exceeds zlib");
        }
        const auto compressed_bound = static_cast<uint64_t>(compressBound(static_cast<uLong>(raw_size)));
        uint64_t chunks{};
        uint64_t chunk_overhead{};
        uint64_t output_bound{};
        if (!checked_add(compressed_bound, png_chunk_payload - 1, chunks)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OVERFLOW, "PNG bound overflows");
        }
        chunks /= png_chunk_payload;
        if (!checked_multiply(chunks, 12, chunk_overhead) || !checked_add(chunk_overhead, 45, chunk_overhead) ||
            !checked_add(compressed_bound, chunk_overhead, output_bound)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OVERFLOW, "PNG output bound overflows");
        }
        if (output_bound > limits->max_encoded_bytes) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "PNG output bound exceeds limits");
        }
        std::vector<uint8_t> encoded;
        if (!cv::imencode(".png", mat, encoded) || encoded.empty()) {
            return fail(out_error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "OpenCV PNG encode failed");
        }
        if (encoded.size() > limits->max_encoded_bytes) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "PNG output exceeds limits");
        }
        auto* data = easycon::native::detail::allocate_bytes(encoded.size());
        if (data == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_ALLOCATION_FAILED, "PNG allocation failed");
        }
        std::memcpy(data, encoded.data(), encoded.size());
        out_encoded->data = reinterpret_cast<uint8_t*>(data);
        out_encoded->length = encoded.size();
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_convert(
    const easycon_native_image_view* image,
    uint32_t output_format,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) noexcept {
    if (out_image != nullptr) {
        *out_image = {};
    }
    return easycon::native::detail::guard(out_error, [=]() {
        if (out_image == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "out_image is null");
        }
        const auto limit_status = validate_limits(limits, out_error);
        if (limit_status != EASYCON_NATIVE_STATUS_OK) {
            return limit_status;
        }
        if (channels_for_format(output_format) == 0) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "output format is invalid");
        }
        cv::Mat input;
        const auto view_status = validate_view(image, *limits, input, out_error);
        if (view_status != EASYCON_NATIVE_STATUS_OK) {
            return view_status;
        }
        if (image->pixel_format == output_format) {
            return copy_mat(input, *limits, out_image, out_error);
        }
        const auto code = conversion_code(image->pixel_format, output_format);
        if (code < 0) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "format conversion is unsupported");
        }
        cv::Mat converted;
        cv::cvtColor(input, converted, code);
        return copy_mat(converted, *limits, out_image, out_error);
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_image_crop(
    const easycon_native_image_view* image,
    uint32_t x,
    uint32_t y,
    uint32_t width,
    uint32_t height,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) noexcept {
    if (out_image != nullptr) {
        *out_image = {};
    }
    return easycon::native::detail::guard(out_error, [=]() {
        if (out_image == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "out_image is null");
        }
        const auto limit_status = validate_limits(limits, out_error);
        if (limit_status != EASYCON_NATIVE_STATUS_OK) {
            return limit_status;
        }
        cv::Mat input;
        const auto view_status = validate_view(image, *limits, input, out_error);
        if (view_status != EASYCON_NATIVE_STATUS_OK) {
            return view_status;
        }
        uint64_t right{};
        uint64_t bottom{};
        if (width == 0 || height == 0 || !checked_add(x, width, right) ||
            !checked_add(y, height, bottom) || right > image->width || bottom > image->height) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "ROI is empty or out of bounds");
        }
        const cv::Rect rectangle(
            static_cast<int>(x),
            static_cast<int>(y),
            static_cast<int>(width),
            static_cast<int>(height));
        const auto cropped = input(rectangle).clone();
        return copy_mat(cropped, *limits, out_image, out_error);
    });
}
