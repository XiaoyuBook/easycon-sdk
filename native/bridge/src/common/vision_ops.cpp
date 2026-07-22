#include "bridge_internal.hpp"

#include <cmath>
#include <cstdint>
#include <limits>
#include <string_view>
#include <vector>

#include <opencv2/imgproc.hpp>

namespace {

easycon_native_status fail(
    easycon_native_error* error,
    easycon_native_status status,
    std::string_view message) noexcept {
    return easycon::native::detail::set_error(error, status, message);
}

int template_method(uint32_t method) noexcept {
    switch (method) {
        case EASYCON_NATIVE_TEMPLATE_SQDIFF_NORMED:
            return cv::TM_SQDIFF_NORMED;
        case EASYCON_NATIVE_TEMPLATE_CCORR_NORMED:
            return cv::TM_CCORR_NORMED;
        case EASYCON_NATIVE_TEMPLATE_CCOEFF_NORMED:
            return cv::TM_CCOEFF_NORMED;
        default:
            return -1;
    }
}

bool has_zero_normalizer(const cv::Mat& image, uint32_t method) {
    if (method == EASYCON_NATIVE_TEMPLATE_CCOEFF_NORMED) {
        cv::Scalar mean;
        cv::Scalar deviation;
        cv::meanStdDev(image, mean, deviation);
        double variance{};
        for (int channel = 0; channel < image.channels(); ++channel) {
            variance += deviation[channel] * deviation[channel];
        }
        return variance == 0.0;
    }
    return cv::norm(image, cv::NORM_L2) == 0.0;
}

easycon_native_status to_gray(
    const cv::Mat& input,
    uint32_t format,
    cv::Mat& output,
    easycon_native_error* error) {
    switch (format) {
        case EASYCON_NATIVE_PIXEL_FORMAT_BGR8:
            cv::cvtColor(input, output, cv::COLOR_BGR2GRAY);
            return EASYCON_NATIVE_STATUS_OK;
        case EASYCON_NATIVE_PIXEL_FORMAT_BGRA8:
            cv::cvtColor(input, output, cv::COLOR_BGRA2GRAY);
            return EASYCON_NATIVE_STATUS_OK;
        case EASYCON_NATIVE_PIXEL_FORMAT_GRAY8:
            output = input;
            return EASYCON_NATIVE_STATUS_OK;
        default:
            return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "edge image format is invalid");
    }
}

easycon_native_status to_bgr(
    const cv::Mat& input,
    uint32_t format,
    cv::Mat& output,
    easycon_native_error* error) {
    switch (format) {
        case EASYCON_NATIVE_PIXEL_FORMAT_BGR8:
            output = input;
            return EASYCON_NATIVE_STATUS_OK;
        case EASYCON_NATIVE_PIXEL_FORMAT_BGRA8:
            cv::cvtColor(input, output, cv::COLOR_BGRA2BGR);
            return EASYCON_NATIVE_STATUS_OK;
        case EASYCON_NATIVE_PIXEL_FORMAT_GRAY8:
            cv::cvtColor(input, output, cv::COLOR_GRAY2BGR);
            return EASYCON_NATIVE_STATUS_OK;
        default:
            return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "color image format is invalid");
    }
}

easycon_native_status validate_roi(
    const easycon_native_image_view& image,
    uint32_t x,
    uint32_t y,
    uint32_t width,
    uint32_t height,
    easycon_native_error* error) noexcept {
    const auto right = static_cast<uint64_t>(x) + width;
    const auto bottom = static_cast<uint64_t>(y) + height;
    if (width == 0 || height == 0 || right > image.width || bottom > image.height) {
        return fail(error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "HSV ROI is empty or out of bounds");
    }
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status validate_hsv_range(
    const easycon_native_hsv_range* range,
    easycon_native_error* error) noexcept {
    if (range == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "HSV range is null");
    }
    if (range->h_min > 179 || range->h_max > 179 || range->s_min > 255 ||
        range->s_max > 255 || range->v_min > 255 || range->v_max > 255) {
        return fail(error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "HSV range exceeds OpenCV scale");
    }
    if (range->s_min > range->s_max || range->v_min > range->v_max) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "HSV S/V range is reversed");
    }
    return EASYCON_NATIVE_STATUS_OK;
}

}  // namespace

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_match_template(
    const easycon_native_image_view* search,
    const easycon_native_image_view* target,
    uint32_t method,
    const easycon_native_image_limits* limits,
    easycon_native_match_extrema* out_result,
    easycon_native_error* out_error) noexcept {
    if (out_result != nullptr) {
        *out_result = {};
    }
    return easycon::native::detail::guard(out_error, [=]() {
        if (out_result == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "match result is null");
        }
        const auto limit_status = easycon::native::detail::validate_image_limits(limits, out_error);
        if (limit_status != EASYCON_NATIVE_STATUS_OK) {
            return limit_status;
        }
        const auto native_method = template_method(method);
        if (native_method < 0) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "template method is invalid");
        }
        cv::Mat search_mat;
        cv::Mat target_mat;
        const auto search_status =
            easycon::native::detail::make_image_view(search, *limits, search_mat, out_error);
        if (search_status != EASYCON_NATIVE_STATUS_OK) {
            return search_status;
        }
        const auto target_status =
            easycon::native::detail::make_image_view(target, *limits, target_mat, out_error);
        if (target_status != EASYCON_NATIVE_STATUS_OK) {
            return target_status;
        }
        if (search->pixel_format != target->pixel_format) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "template formats differ");
        }
        if (target->width > search->width || target->height > search->height) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "target exceeds search image");
        }
        if (has_zero_normalizer(target_mat, method) || has_zero_normalizer(search_mat, method)) {
            return fail(
                out_error,
                EASYCON_NATIVE_STATUS_BACKEND_ERROR,
                "normalized template denominator is zero");
        }

        cv::Mat result;
        cv::matchTemplate(search_mat, target_mat, result, native_method);
        cv::Point min_location;
        cv::Point max_location;
        double min_value{};
        double max_value{};
        cv::minMaxLoc(result, &min_value, &max_value, &min_location, &max_location);
        if (!std::isfinite(min_value) || !std::isfinite(max_value)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "template result is non-finite");
        }
        if (min_location.x < 0 || min_location.y < 0 || max_location.x < 0 ||
            max_location.y < 0) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INTERNAL, "template location is negative");
        }
        *out_result = easycon_native_match_extrema{
            min_value,
            max_value,
            min_location.x,
            min_location.y,
            max_location.x,
            max_location.y,
        };
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_edge_preprocess(
    const easycon_native_image_view* image,
    uint32_t method,
    const easycon_native_image_limits* limits,
    easycon_native_image* out_image,
    easycon_native_error* out_error) noexcept {
    if (out_image != nullptr) {
        *out_image = {};
    }
    return easycon::native::detail::guard(out_error, [=]() {
        if (out_image == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "edge output is null");
        }
        const auto limit_status = easycon::native::detail::validate_image_limits(limits, out_error);
        if (limit_status != EASYCON_NATIVE_STATUS_OK) {
            return limit_status;
        }
        if (method != EASYCON_NATIVE_EDGE_XY && method != EASYCON_NATIVE_EDGE_LAPLACIAN) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "edge method is invalid");
        }
        cv::Mat input;
        const auto input_status =
            easycon::native::detail::make_image_view(image, *limits, input, out_error);
        if (input_status != EASYCON_NATIVE_STATUS_OK) {
            return input_status;
        }
        cv::Mat gray;
        const auto gray_status = to_gray(input, image->pixel_format, gray, out_error);
        if (gray_status != EASYCON_NATIVE_STATUS_OK) {
            return gray_status;
        }

        cv::Mat edge;
        if (method == EASYCON_NATIVE_EDGE_XY) {
            cv::Mat gradient_x;
            cv::Mat gradient_y;
            cv::Mat combined;
            cv::Sobel(gray, gradient_x, CV_16S, 1, 0, -1);
            cv::Sobel(gray, gradient_y, CV_16S, 0, 1, -1);
            cv::addWeighted(gradient_x, 0.5, gradient_y, 0.5, 0.0, combined);
            combined.convertTo(edge, CV_8U);
        } else {
            cv::Mat blurred;
            cv::Mat laplacian;
            cv::Mat absolute;
            cv::GaussianBlur(gray, blurred, cv::Size(5, 5), 1.5);
            cv::Laplacian(blurred, laplacian, CV_16S, 3);
            cv::convertScaleAbs(laplacian, absolute);
            cv::threshold(absolute, edge, 30.0, 255.0, cv::THRESH_BINARY);
        }
        return easycon::native::detail::copy_image_mat(edge, *limits, out_image, out_error);
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_hsv_count(
    const easycon_native_image_view* image,
    uint32_t roi_x,
    uint32_t roi_y,
    uint32_t roi_width,
    uint32_t roi_height,
    const easycon_native_hsv_range* range,
    const easycon_native_image_limits* limits,
    easycon_native_color_result* out_result,
    easycon_native_error* out_error) noexcept {
    if (out_result != nullptr) {
        *out_result = {};
    }
    return easycon::native::detail::guard(out_error, [=]() {
        if (out_result == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "color result is null");
        }
        const auto limit_status = easycon::native::detail::validate_image_limits(limits, out_error);
        if (limit_status != EASYCON_NATIVE_STATUS_OK) {
            return limit_status;
        }
        const auto range_status = validate_hsv_range(range, out_error);
        if (range_status != EASYCON_NATIVE_STATUS_OK) {
            return range_status;
        }
        cv::Mat input;
        const auto input_status =
            easycon::native::detail::make_image_view(image, *limits, input, out_error);
        if (input_status != EASYCON_NATIVE_STATUS_OK) {
            return input_status;
        }
        const auto roi_status =
            validate_roi(*image, roi_x, roi_y, roi_width, roi_height, out_error);
        if (roi_status != EASYCON_NATIVE_STATUS_OK) {
            return roi_status;
        }
        const cv::Rect rectangle(
            static_cast<int>(roi_x),
            static_cast<int>(roi_y),
            static_cast<int>(roi_width),
            static_cast<int>(roi_height));
        const cv::Mat cropped = input(rectangle);
        cv::Mat bgr;
        const auto bgr_status = to_bgr(cropped, image->pixel_format, bgr, out_error);
        if (bgr_status != EASYCON_NATIVE_STATUS_OK) {
            return bgr_status;
        }
        cv::Mat hsv;
        cv::cvtColor(bgr, hsv, cv::COLOR_BGR2HSV);
        cv::Mat mask;
        if (range->h_min <= range->h_max) {
            cv::inRange(
                hsv,
                cv::Scalar(range->h_min, range->s_min, range->v_min),
                cv::Scalar(range->h_max, range->s_max, range->v_max),
                mask);
        } else {
            cv::Mat high;
            cv::Mat low;
            cv::inRange(
                hsv,
                cv::Scalar(range->h_min, range->s_min, range->v_min),
                cv::Scalar(179, range->s_max, range->v_max),
                high);
            cv::inRange(
                hsv,
                cv::Scalar(0, range->s_min, range->v_min),
                cv::Scalar(range->h_max, range->s_max, range->v_max),
                low);
            cv::bitwise_or(high, low, mask);
        }

        const auto count = cv::countNonZero(mask);
        const auto area = static_cast<uint64_t>(roi_width) * roi_height;
        if (count < 0 || static_cast<uint64_t>(count) > area) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INTERNAL, "HSV count exceeds ROI area");
        }
        easycon_native_color_result result{};
        result.count = static_cast<uint64_t>(count);
        if (count > 0) {
            std::vector<cv::Point> points;
            cv::findNonZero(mask, points);
            const auto bounds = cv::boundingRect(points);
            if (bounds.x < 0 || bounds.y < 0 || bounds.width <= 0 || bounds.height <= 0 ||
                bounds.x + bounds.width > static_cast<int>(roi_width) ||
                bounds.y + bounds.height > static_cast<int>(roi_height)) {
                return fail(out_error, EASYCON_NATIVE_STATUS_INTERNAL, "HSV bounding box is invalid");
            }
            result.bbox_x = static_cast<uint32_t>(bounds.x);
            result.bbox_y = static_cast<uint32_t>(bounds.y);
            result.bbox_width = static_cast<uint32_t>(bounds.width);
            result.bbox_height = static_cast<uint32_t>(bounds.height);
            result.has_bbox = 1;
        }
        *out_result = result;
        return EASYCON_NATIVE_STATUS_OK;
    });
}
