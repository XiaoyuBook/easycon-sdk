#include "bridge_internal.hpp"

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cstring>
#include <filesystem>
#include <limits>
#include <memory>
#include <string>
#include <string_view>

#include <opencv2/imgproc.hpp>
#include <tesseract/baseapi.h>

struct easycon_native_ocr_engine {
    uint64_t marker;
    std::unique_ptr<tesseract::TessBaseAPI> api;
};

namespace {

constexpr uint64_t ocr_engine_marker = UINT64_C(0x454153594F435233);
constexpr uint64_t max_model_root_bytes = UINT64_C(32768);
constexpr uint64_t max_language_bytes = UINT64_C(128);
constexpr uint64_t max_ocr_output_bytes = UINT64_C(1048576);

std::atomic<uint64_t> engines_created{0};
std::atomic<uint64_t> engines_destroyed{0};
std::atomic<uint64_t> process_calls{0};
std::atomic<uint64_t> clear_calls{0};
std::atomic<uint64_t> teardown_exceptions{0};
#if defined(EASYCON_NATIVE_TESTING)
constexpr int32_t no_ocr_failpoint = INT32_C(-1);
std::atomic<int32_t> next_ocr_failpoint{no_ocr_failpoint};

void inject_ocr_failure(int32_t failpoint) {
    auto expected = failpoint;
    if (!next_ocr_failpoint.compare_exchange_strong(
            expected,
            no_ocr_failpoint,
            std::memory_order_acq_rel,
            std::memory_order_acquire)) {
        return;
    }
    if (failpoint == EASYCON_NATIVE_TEST_OCR_FAIL_MODEL_CHECK_BAD_ALLOC) {
        throw std::bad_alloc();
    }
    if (failpoint == EASYCON_NATIVE_TEST_OCR_FAIL_CONFIDENCE_STD) {
        throw std::runtime_error("scripted OCR confidence exception");
    }
    if (failpoint == EASYCON_NATIVE_TEST_OCR_FAIL_DESTROY_UNKNOWN) {
        throw UINT32_C(13);
    }
}
#endif

easycon_native_status fail(
    easycon_native_error* error,
    easycon_native_status status,
    std::string_view message) noexcept {
    return easycon::native::detail::set_error(error, status, message);
}

bool valid_utf8(std::string_view text) noexcept {
    size_t index = 0;
    while (index < text.size()) {
        const auto first = static_cast<uint8_t>(text[index]);
        size_t continuation = 0;
        uint32_t code_point = 0;
        if (first <= UINT8_C(0x7F)) {
            ++index;
            continue;
        }
        if (first >= UINT8_C(0xC2) && first <= UINT8_C(0xDF)) {
            continuation = 1;
            code_point = first & UINT8_C(0x1F);
        } else if (first >= UINT8_C(0xE0) && first <= UINT8_C(0xEF)) {
            continuation = 2;
            code_point = first & UINT8_C(0x0F);
        } else if (first >= UINT8_C(0xF0) && first <= UINT8_C(0xF4)) {
            continuation = 3;
            code_point = first & UINT8_C(0x07);
        } else {
            return false;
        }
        if (index + continuation >= text.size()) {
            return false;
        }
        for (size_t offset = 1; offset <= continuation; ++offset) {
            const auto byte = static_cast<uint8_t>(text[index + offset]);
            if ((byte & UINT8_C(0xC0)) != UINT8_C(0x80)) {
                return false;
            }
            code_point = (code_point << 6U) | (byte & UINT8_C(0x3F));
        }
        const bool overlong =
            (continuation == 1 && code_point < UINT32_C(0x80)) ||
            (continuation == 2 && code_point < UINT32_C(0x800)) ||
            (continuation == 3 && code_point < UINT32_C(0x10000));
        if (overlong || (code_point >= UINT32_C(0xD800) && code_point <= UINT32_C(0xDFFF)) ||
            code_point > UINT32_C(0x10FFFF)) {
            return false;
        }
        index += continuation + 1;
    }
    return true;
}

easycon_native_status copy_utf8(
    const char* text,
    uint64_t maximum,
    easycon_native_buffer* output,
    easycon_native_error* error) noexcept {
    if (text == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "Tesseract returned null text");
    }
    const auto maximum_size = static_cast<size_t>(maximum);
    const auto length = strnlen(text, maximum_size + 1);
    if (length > maximum_size) {
        return fail(error, EASYCON_NATIVE_STATUS_RESOURCE_EXHAUSTED, "OCR text exceeds limit");
    }
    const std::string_view view(text, length);
    if (!valid_utf8(view)) {
        return fail(error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "Tesseract returned invalid UTF-8");
    }
    if (length == 0) {
        return EASYCON_NATIVE_STATUS_OK;
    }
    auto* bytes = easycon::native::detail::allocate_bytes(length);
    if (bytes == nullptr) {
        return fail(error, EASYCON_NATIVE_STATUS_ALLOCATION_FAILED, "OCR text allocation failed");
    }
    std::memcpy(bytes, text, length);
    output->data = reinterpret_cast<uint8_t*>(bytes);
    output->length = static_cast<uint64_t>(length);
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status read_utf8(
    const uint8_t* data,
    uint64_t length,
    uint64_t maximum,
    std::string& output,
    easycon_native_error* error,
    std::string_view label) {
    if (data == nullptr || length == 0 || length > maximum ||
        length > static_cast<uint64_t>((std::numeric_limits<size_t>::max)())) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, label);
    }
    const auto size = static_cast<size_t>(length);
    const std::string_view view(reinterpret_cast<const char*>(data), size);
    if (view.find('\0') != std::string_view::npos || !valid_utf8(view)) {
        return fail(error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, label);
    }
    output.assign(view);
    return EASYCON_NATIVE_STATUS_OK;
}

bool valid_language(std::string_view language) noexcept {
    if (language.empty() || language.front() == '+' || language.back() == '+') {
        return false;
    }
    bool previous_plus = false;
    for (const unsigned char value : language) {
        const bool plus = value == static_cast<unsigned char>('+');
        if (plus && previous_plus) {
            return false;
        }
        if (!plus && std::isalnum(value) == 0 && value != static_cast<unsigned char>('_') &&
            value != static_cast<unsigned char>('-')) {
            return false;
        }
        previous_plus = plus;
    }
    return true;
}

tesseract::OcrEngineMode engine_mode(uint32_t mode, bool& valid) noexcept {
    valid = true;
    switch (mode) {
        case EASYCON_NATIVE_OCR_ENGINE_DEFAULT:
            return tesseract::OEM_DEFAULT;
        case EASYCON_NATIVE_OCR_ENGINE_LSTM_ONLY:
            return tesseract::OEM_LSTM_ONLY;
        default:
            valid = false;
            return tesseract::OEM_DEFAULT;
    }
}

tesseract::PageSegMode page_segmentation(uint32_t mode, bool& valid) noexcept {
    valid = true;
    switch (mode) {
        case EASYCON_NATIVE_OCR_PSM_AUTO:
            return tesseract::PSM_AUTO;
        case EASYCON_NATIVE_OCR_PSM_SINGLE_BLOCK:
            return tesseract::PSM_SINGLE_BLOCK;
        case EASYCON_NATIVE_OCR_PSM_SINGLE_LINE:
            return tesseract::PSM_SINGLE_LINE;
        case EASYCON_NATIVE_OCR_PSM_SINGLE_WORD:
            return tesseract::PSM_SINGLE_WORD;
        default:
            valid = false;
            return tesseract::PSM_SINGLE_LINE;
    }
}

bool models_exist(const std::filesystem::path& root, std::string_view language) {
#if defined(EASYCON_NATIVE_TESTING)
    inject_ocr_failure(EASYCON_NATIVE_TEST_OCR_FAIL_MODEL_CHECK_BAD_ALLOC);
#endif
    std::error_code error;
    if (!std::filesystem::is_directory(root, error) || error) {
        return false;
    }
    size_t start = 0;
    while (start < language.size()) {
        const auto end = language.find('+', start);
        const auto name = language.substr(start, end == std::string_view::npos ? language.size() - start
                                                                               : end - start);
        const auto model = root / (std::string(name) + ".traineddata");
        if (!std::filesystem::is_regular_file(model, error) || error) {
            return false;
        }
        if (end == std::string_view::npos) {
            break;
        }
        start = end + 1;
    }
    return true;
}

easycon_native_image_limits ocr_image_limits() noexcept {
    return easycon_native_image_limits{
        UINT64_C(67108864),
        UINT32_C(16384),
        UINT32_C(16384),
        UINT64_C(67108864),
        UINT64_C(268435456),
        UINT64_C(65536),
    };
}

}  // namespace

namespace easycon::native::detail {

OcrDebugCounts ocr_debug_counts() noexcept {
    return OcrDebugCounts{
        engines_created.load(std::memory_order_relaxed),
        engines_destroyed.load(std::memory_order_relaxed),
        process_calls.load(std::memory_order_relaxed),
        clear_calls.load(std::memory_order_relaxed),
        teardown_exceptions.load(std::memory_order_relaxed),
    };
}

bool ocr_engine_is_valid(const easycon_native_ocr_engine* engine) noexcept {
    return engine != nullptr && engine->marker == ocr_engine_marker && engine->api != nullptr;
}

#if defined(EASYCON_NATIVE_TESTING)
bool ocr_test_fail_next(int32_t failpoint) noexcept {
    if (failpoint != EASYCON_NATIVE_TEST_OCR_FAIL_MODEL_CHECK_BAD_ALLOC &&
        failpoint != EASYCON_NATIVE_TEST_OCR_FAIL_CONFIDENCE_STD &&
        failpoint != EASYCON_NATIVE_TEST_OCR_FAIL_DESTROY_UNKNOWN) {
        return false;
    }
    auto expected = no_ocr_failpoint;
    return next_ocr_failpoint.compare_exchange_strong(
        expected,
        failpoint,
        std::memory_order_release,
        std::memory_order_relaxed);
}

bool ocr_test_invalidate(easycon_native_ocr_engine* engine) noexcept {
    if (!ocr_engine_is_valid(engine)) {
        return false;
    }
    engine->marker = 0;
    return true;
}
#endif

}  // namespace easycon::native::detail

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_ocr_engine_create(
    const uint8_t* model_root_utf8,
    uint64_t model_root_length,
    const uint8_t* language_utf8,
    uint64_t language_length,
    uint32_t mode,
    easycon_native_ocr_engine** out_engine,
    easycon_native_error* out_error) noexcept {
    if (out_engine != nullptr) {
        *out_engine = nullptr;
    }
    return easycon::native::detail::guard(out_error, [&]() {
        if (out_engine == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "out_engine is null");
        }
        std::string root_utf8;
        auto status = read_utf8(
            model_root_utf8,
            model_root_length,
            max_model_root_bytes,
            root_utf8,
            out_error,
            "model root must be bounded UTF-8");
        if (status != EASYCON_NATIVE_STATUS_OK) {
            return status;
        }
        std::string language;
        status = read_utf8(
            language_utf8,
            language_length,
            max_language_bytes,
            language,
            out_error,
            "language must be bounded UTF-8");
        if (status != EASYCON_NATIVE_STATUS_OK) {
            return status;
        }
        if (!valid_language(language)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "language is invalid");
        }
        bool valid_mode = false;
        const auto native_mode = engine_mode(mode, valid_mode);
        if (!valid_mode) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "OCR engine mode is invalid");
        }
        const std::u8string root_u8(
            reinterpret_cast<const char8_t*>(root_utf8.data()),
            root_utf8.size());
        const std::filesystem::path root(root_u8);
        if (!models_exist(root, language)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_MODEL_NOT_FOUND, "OCR model was not found");
        }

        auto api = std::make_unique<tesseract::TessBaseAPI>();
        if (api->Init(root_utf8.c_str(), language.c_str(), native_mode) != 0) {
            api->End();
            return fail(out_error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "Tesseract initialization failed");
        }
        auto engine = std::make_unique<easycon_native_ocr_engine>();
        engine->marker = ocr_engine_marker;
        engine->api = std::move(api);
        *out_engine = engine.release();
        engines_created.fetch_add(1, std::memory_order_relaxed);
        easycon::native::detail::track_handle_created();
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_ocr_engine_process(
    easycon_native_ocr_engine* engine,
    const easycon_native_image_view* image,
    uint32_t segmentation,
    uint64_t max_output_bytes,
    easycon_native_buffer* out_text,
    double* out_confidence,
    easycon_native_error* out_error) noexcept {
    if (out_text != nullptr) {
        *out_text = {};
    }
    if (out_confidence != nullptr) {
        *out_confidence = 0.0;
    }
    return easycon::native::detail::guard(out_error, [&]() {
        if (out_text == nullptr || out_confidence == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "OCR output is null");
        }
        if (!easycon::native::detail::ocr_engine_is_valid(engine)) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "OCR engine is invalid");
        }
        if (max_output_bytes == 0 || max_output_bytes > max_ocr_output_bytes ||
            max_output_bytes >= static_cast<uint64_t>((std::numeric_limits<size_t>::max)())) {
            return fail(out_error, EASYCON_NATIVE_STATUS_OUT_OF_RANGE, "OCR output limit is invalid");
        }
        bool valid_segmentation = false;
        const auto native_segmentation = page_segmentation(segmentation, valid_segmentation);
        if (!valid_segmentation) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "OCR page segmentation is invalid");
        }
        auto limits = ocr_image_limits();
        cv::Mat source;
        const auto image_status =
            easycon::native::detail::make_image_view(image, limits, source, out_error);
        if (image_status != EASYCON_NATIVE_STATUS_OK) {
            return image_status;
        }

        cv::Mat gray;
        if (source.channels() == 1) {
            gray = source;
        } else if (source.channels() == 3) {
            cv::cvtColor(source, gray, cv::COLOR_BGR2GRAY);
        } else if (source.channels() == 4) {
            cv::cvtColor(source, gray, cv::COLOR_BGRA2GRAY);
        } else {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_IMAGE, "OCR image format is unsupported");
        }

        engine->api->Clear();
        clear_calls.fetch_add(1, std::memory_order_relaxed);
        engine->api->SetPageSegMode(native_segmentation);
        engine->api->SetImage(
            gray.data,
            gray.cols,
            gray.rows,
            1,
            static_cast<int>(gray.step));
        if (engine->api->Recognize(nullptr) != 0) {
            return fail(out_error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "Tesseract recognition failed");
        }
        process_calls.fetch_add(1, std::memory_order_relaxed);
        std::unique_ptr<char[]> text(engine->api->GetUTF8Text());
#if defined(EASYCON_NATIVE_TESTING)
        inject_ocr_failure(EASYCON_NATIVE_TEST_OCR_FAIL_CONFIDENCE_STD);
#endif
        const auto confidence = engine->api->MeanTextConf();
        if (confidence < 0 || confidence > 100) {
            return fail(out_error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "OCR confidence is invalid");
        }
        const auto copy_status = copy_utf8(text.get(), max_output_bytes, out_text, out_error);
        if (copy_status != EASYCON_NATIVE_STATUS_OK) {
            return copy_status;
        }
        *out_confidence = static_cast<double>(confidence) / 100.0;
        return EASYCON_NATIVE_STATUS_OK;
    });
}

extern "C" easycon_native_status EASYCON_NATIVE_CALL easycon_native_ocr_engine_destroy(
    easycon_native_ocr_engine** inout_engine,
    easycon_native_error* out_error) noexcept {
    return easycon::native::detail::guard(out_error, [inout_engine, out_error]() {
        if (inout_engine == nullptr) {
            return fail(out_error, EASYCON_NATIVE_STATUS_INVALID_ARGUMENT, "inout_engine is null");
        }
        if (*inout_engine == nullptr) {
            return EASYCON_NATIVE_STATUS_OK;
        }
        auto* const engine = *inout_engine;
        *inout_engine = nullptr;
        if (engine->api != nullptr) {
            try {
#if defined(EASYCON_NATIVE_TESTING)
                inject_ocr_failure(EASYCON_NATIVE_TEST_OCR_FAIL_DESTROY_UNKNOWN);
#endif
                engine->api->End();
            } catch (...) {
                teardown_exceptions.fetch_add(1, std::memory_order_relaxed);
            }
            engine->api.reset();
        }
        engine->marker = 0;
        delete engine;
        engines_destroyed.fetch_add(1, std::memory_order_relaxed);
        easycon::native::detail::track_handle_destroyed();
        return EASYCON_NATIVE_STATUS_OK;
    });
}
