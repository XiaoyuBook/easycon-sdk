#include "internal/easycon_native_bridge.h"

#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <string_view>

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

    const auto final_counts = counts();
    expect(final_counts.live_handles == 0, "all native handles are released");
    expect(final_counts.live_allocations == 0, "all native allocations are released");
    return failures == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}
