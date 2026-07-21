#include "internal/easycon_native_bridge.h"

#include <algorithm>
#include <cctype>
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

int hex_digit(char value) {
    if (value >= '0' && value <= '9') {
        return value - '0';
    }
    if (value >= 'a' && value <= 'f') {
        return value - 'a' + 10;
    }
    return -1;
}

std::vector<uint8_t> read_fixture() {
    std::ifstream input("fixtures/ocr/easycon-gray.hex");
    expect(input.good(), "OCR fixture opens");
    std::string text;
    input >> text;
    expect((text.size() % 2) == 0, "OCR fixture has complete hex bytes");
    std::vector<uint8_t> bytes;
    bytes.reserve(text.size() / 2);
    for (size_t index = 0; index + 1 < text.size(); index += 2) {
        const auto high = hex_digit(text[index]);
        const auto low = hex_digit(text[index + 1]);
        expect(high >= 0 && low >= 0, "OCR fixture is lowercase hexadecimal");
        if (high < 0 || low < 0) {
            return {};
        }
        bytes.push_back(static_cast<uint8_t>((high << 4) | low));
    }
    return bytes;
}

easycon_native_counts resource_counts() {
    easycon_native_counts result{};
    easycon_native_error error{};
    expect(
        easycon_native_debug_counts(&result, &error) == EASYCON_NATIVE_STATUS_OK,
        "native resource counts succeed");
    easycon_native_error_release(&error);
    return result;
}

easycon_native_ocr_test_counts ocr_counts() {
    easycon_native_ocr_test_counts result{};
    easycon_native_error error{};
    expect(
        easycon_native_test_ocr_counts(&result, &error) == EASYCON_NATIVE_STATUS_OK,
        "OCR counters succeed");
    easycon_native_error_release(&error);
    return result;
}

std::string copy_text(const easycon_native_buffer& buffer) {
    if (buffer.data == nullptr) {
        return {};
    }
    return std::string(
        reinterpret_cast<const char*>(buffer.data),
        static_cast<size_t>(buffer.length));
}

std::string compact_ascii(std::string text) {
    text.erase(
        std::remove_if(
            text.begin(),
            text.end(),
            [](unsigned char value) { return std::isspace(value) != 0; }),
        text.end());
    return text;
}

easycon_native_ocr_engine* create_engine(std::string_view model_root) {
    constexpr std::string_view language = "eng";
    easycon_native_ocr_engine* engine = nullptr;
    easycon_native_error error{};
    const auto status = easycon_native_ocr_engine_create(
        reinterpret_cast<const uint8_t*>(model_root.data()),
        model_root.size(),
        reinterpret_cast<const uint8_t*>(language.data()),
        language.size(),
        EASYCON_NATIVE_OCR_ENGINE_DEFAULT,
        &engine,
        &error);
    expect(status == EASYCON_NATIVE_STATUS_OK, "OCR engine create succeeds");
    expect(engine != nullptr, "OCR engine create returns one handle");
    expect(error.data == nullptr && error.length == 0, "OCR create success leaves error empty");
    easycon_native_error_release(&error);
    return engine;
}

void test_missing_model_is_explicit() {
    constexpr std::string_view missing = "Z:/easycon-sdk-missing-tessdata";
    constexpr std::string_view language = "eng";
    easycon_native_ocr_engine* engine = reinterpret_cast<easycon_native_ocr_engine*>(UINTPTR_MAX);
    easycon_native_error error{};
    const auto status = easycon_native_ocr_engine_create(
        reinterpret_cast<const uint8_t*>(missing.data()),
        missing.size(),
        reinterpret_cast<const uint8_t*>(language.data()),
        language.size(),
        EASYCON_NATIVE_OCR_ENGINE_DEFAULT,
        &engine,
        &error);
    expect(status == EASYCON_NATIVE_STATUS_MODEL_NOT_FOUND, "missing model is ModelNotFound");
    expect(engine == nullptr, "failed OCR create zeroes the handle");
    expect(error.data != nullptr && error.length != 0, "missing model has a diagnostic");
    easycon_native_error_release(&error);
}

void test_process_reuse_exception_and_release(std::string_view model_root) {
    const auto baseline = resource_counts();
    const auto initial_ocr = ocr_counts();
    auto* engine = create_engine(model_root);
    const auto created = resource_counts();
    expect(created.live_handles == baseline.live_handles + 1, "OCR create owns one handle");

    auto pixels = read_fixture();
    const easycon_native_image_view image{
        pixels.data(), pixels.size(), 273, 77, 273, EASYCON_NATIVE_PIXEL_FORMAT_GRAY8};

    for (int iteration = 0; iteration < 2; ++iteration) {
        easycon_native_buffer text{};
        double confidence = -1.0;
        easycon_native_error error{};
        const auto status = easycon_native_ocr_engine_process(
            engine,
            &image,
            EASYCON_NATIVE_OCR_PSM_SINGLE_LINE,
            UINT64_C(4096),
            &text,
            &confidence,
            &error);
        expect(status == EASYCON_NATIVE_STATUS_OK, "OCR process succeeds");
        const auto recognized = compact_ascii(copy_text(text));
        if (recognized.find("EASCON") == std::string::npos) {
            std::cerr << "OCR observed text: " << recognized << '\n';
        }
        expect(recognized == "EASCON", "OCR fixture recognizes exactly EASCON");
        expect(confidence >= 0.0 && confidence <= 1.0, "OCR confidence is normalized");
        easycon_native_buffer_release(&text);
        easycon_native_error_release(&error);
    }

    easycon_native_buffer bad_text{reinterpret_cast<uint8_t*>(UINTPTR_MAX), 1};
    double bad_confidence = 2.0;
    easycon_native_error error{};
    const easycon_native_image_view bad_image{
        pixels.data(), 1, 273, 77, 273, EASYCON_NATIVE_PIXEL_FORMAT_GRAY8};
    expect(
        easycon_native_ocr_engine_process(
            engine,
            &bad_image,
            EASYCON_NATIVE_OCR_PSM_SINGLE_LINE,
            UINT64_C(4096),
            &bad_text,
            &bad_confidence,
            &error) == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "bad OCR image is rejected before Tesseract");
    expect(bad_text.data == nullptr && bad_text.length == 0, "bad image zeroes text output");
    expect(bad_confidence == 0.0, "bad image zeroes confidence output");
    easycon_native_error_release(&error);

    easycon_native_buffer invalid_text{reinterpret_cast<uint8_t*>(UINTPTR_MAX), 1};
    double invalid_confidence = 2.0;
    expect(
        easycon_native_ocr_engine_process(
            engine,
            &image,
            UINT32_C(99),
            UINT64_C(4096),
            &invalid_text,
            &invalid_confidence,
            &error) == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "unknown OCR page segmentation is rejected");
    expect(
        invalid_text.data == nullptr && invalid_text.length == 0,
        "invalid OCR mode zeroes text output");
    expect(invalid_confidence == 0.0, "invalid OCR mode zeroes confidence output");
    easycon_native_error_release(&error);

    easycon_native_buffer limited_text{};
    double limited_confidence = 2.0;
    expect(
        easycon_native_ocr_engine_process(
            engine,
            &image,
            EASYCON_NATIVE_OCR_PSM_SINGLE_LINE,
            UINT64_C(1),
            &limited_text,
            &limited_confidence,
            &error) == EASYCON_NATIVE_STATUS_RESOURCE_EXHAUSTED,
        "OCR text over the explicit output limit is rejected");
    expect(
        limited_text.data == nullptr && limited_text.length == 0,
        "limited OCR output owns no allocation");
    expect(limited_confidence == 0.0, "limited OCR output leaves confidence zero");
    easycon_native_buffer_release(&limited_text);
    easycon_native_error_release(&error);

    easycon_native_buffer null_engine_text{reinterpret_cast<uint8_t*>(UINTPTR_MAX), 1};
    double null_engine_confidence = 2.0;
    expect(
        easycon_native_ocr_engine_process(
            nullptr,
            &image,
            EASYCON_NATIVE_OCR_PSM_SINGLE_LINE,
            UINT64_C(4096),
            &null_engine_text,
            &null_engine_confidence,
            &error) == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "null OCR engine is rejected");
    expect(
        null_engine_text.data == nullptr && null_engine_text.length == 0,
        "null engine zeroes text output");
    expect(null_engine_confidence == 0.0, "null engine zeroes confidence output");
    easycon_native_error_release(&error);

    auto* const text_sentinel = reinterpret_cast<uint8_t*>(UINTPTR_MAX);
    easycon_native_buffer no_error_text{text_sentinel, 1};
    double no_error_confidence = 2.0;
    expect(
        easycon_native_ocr_engine_process(
            engine,
            &image,
            EASYCON_NATIVE_OCR_PSM_SINGLE_LINE,
            UINT64_C(4096),
            &no_error_text,
            &no_error_confidence,
            nullptr) == EASYCON_NATIVE_STATUS_INVALID_ARGUMENT,
        "OCR process requires error storage");
    expect(
        no_error_text.data == nullptr && no_error_text.length == 0,
        "missing error storage still zeroes OCR text");
    expect(no_error_confidence == 0.0, "missing error storage still zeroes confidence");

    for (const auto kind : {
             EASYCON_NATIVE_TEST_RAISE_CV,
             EASYCON_NATIVE_TEST_RAISE_STD,
             EASYCON_NATIVE_TEST_RAISE_UNKNOWN}) {
        const auto expected = kind == EASYCON_NATIVE_TEST_RAISE_CV
                                  ? EASYCON_NATIVE_STATUS_CV_EXCEPTION
                              : kind == EASYCON_NATIVE_TEST_RAISE_STD
                                  ? EASYCON_NATIVE_STATUS_STD_EXCEPTION
                                  : EASYCON_NATIVE_STATUS_UNKNOWN_EXCEPTION;
        expect(
            easycon_native_test_ocr_raise(engine, kind, &error) == expected,
            "OCR exception is isolated at its entry point");
        easycon_native_error_release(&error);
    }

    expect(
        easycon_native_test_ocr_fail_next(
            EASYCON_NATIVE_TEST_OCR_FAIL_CONFIDENCE_STD,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "OCR confidence failpoint is armed");
    easycon_native_buffer exception_text{reinterpret_cast<uint8_t*>(UINTPTR_MAX), 1};
    double exception_confidence = 2.0;
    expect(
        easycon_native_ocr_engine_process(
            engine,
            &image,
            EASYCON_NATIVE_OCR_PSM_SINGLE_LINE,
            UINT64_C(4096),
            &exception_text,
            &exception_confidence,
            &error) == EASYCON_NATIVE_STATUS_STD_EXCEPTION,
        "OCR confidence exception is isolated");
    expect(
        exception_text.data == nullptr && exception_text.length == 0,
        "OCR confidence exception leaves text output zero");
    expect(exception_confidence == 0.0, "OCR confidence exception leaves confidence zero");
    easycon_native_buffer_release(&exception_text);
    easycon_native_error_release(&error);

    expect(
        easycon_native_test_ocr_invalidate(engine, &error) == EASYCON_NATIVE_STATUS_OK,
        "live OCR engine can enter the invalid poison state");
    easycon_native_error_release(&error);
    expect(
        easycon_native_test_ocr_fail_next(
            EASYCON_NATIVE_TEST_OCR_FAIL_DESTROY_UNKNOWN,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "OCR destroy exception failpoint is armed");
    easycon_native_error_release(&error);

    expect(
        easycon_native_ocr_engine_destroy(&engine, &error) == EASYCON_NATIVE_STATUS_OK,
        "OCR destroy consumes a poisoned engine");
    expect(engine == nullptr, "OCR destroy consumes the handle");
    expect(
        easycon_native_ocr_engine_destroy(&engine, &error) == EASYCON_NATIVE_STATUS_OK,
        "OCR destroy is idempotent");
    easycon_native_error_release(&error);

    const auto final_ocr = ocr_counts();
    expect(final_ocr.created == initial_ocr.created + 1, "one OCR engine was created");
    expect(final_ocr.destroyed == initial_ocr.destroyed + 1, "one OCR engine was destroyed");
    expect(
        final_ocr.process_calls == initial_ocr.process_calls + 4,
        "one engine completed two outputs, one bounded recognition, and one isolated failure");
    expect(
        final_ocr.clear_calls == initial_ocr.clear_calls + 4,
        "reuse clears before every admitted recognition");
    expect(
        final_ocr.teardown_exceptions == initial_ocr.teardown_exceptions + 1,
        "destroy isolates and records one teardown exception");
    const auto final = resource_counts();
    expect(final.live_handles == baseline.live_handles, "OCR handle count returns to baseline");
    expect(final.live_allocations == baseline.live_allocations, "OCR allocations return to baseline");
}

void test_model_check_exception_is_isolated(std::string_view model_root) {
    constexpr std::string_view language = "eng";
    easycon_native_error error{};
    expect(
        easycon_native_test_ocr_fail_next(
            EASYCON_NATIVE_TEST_OCR_FAIL_MODEL_CHECK_BAD_ALLOC,
            &error) == EASYCON_NATIVE_STATUS_OK,
        "OCR model-check failpoint is armed");
    easycon_native_error_release(&error);

    easycon_native_ocr_engine* engine = reinterpret_cast<easycon_native_ocr_engine*>(UINTPTR_MAX);
    const auto status = easycon_native_ocr_engine_create(
        reinterpret_cast<const uint8_t*>(model_root.data()),
        model_root.size(),
        reinterpret_cast<const uint8_t*>(language.data()),
        language.size(),
        EASYCON_NATIVE_OCR_ENGINE_DEFAULT,
        &engine,
        &error);
    expect(status == EASYCON_NATIVE_STATUS_ALLOCATION_FAILED, "model-check allocation is isolated");
    expect(engine == nullptr, "model-check exception leaves handle output null");
    easycon_native_error_release(&error);
}

}  // namespace

int main() {
    static_assert(sizeof(void*) == 8, "Phase 3 native bridge is x64-only");
    char* model_root = nullptr;
    size_t model_root_length = 0;
    const auto environment_status =
        _dupenv_s(&model_root, &model_root_length, "EASYCON_VISION_TEST_TESSDATA");
    expect(
        environment_status == 0 && model_root != nullptr && model_root_length > 1,
        "OCR test model path is required");
    test_missing_model_is_explicit();
    if (environment_status == 0 && model_root != nullptr && model_root_length > 1) {
        test_process_reuse_exception_and_release(model_root);
        test_model_check_exception_is_isolated(model_root);
    }
    std::free(model_root);
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
