#include "capture_platform.hpp"

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#include <dshow.h>
#include <mfapi.h>
#include <mfidl.h>
#include <oleauto.h>
#include <wrl/client.h>

#include <climits>
#include <cstdint>
#include <cwchar>
#include <string>
#include <utility>

#include <opencv2/videoio/registry.hpp>

namespace {

constexpr size_t max_discovered_sources = 64;
constexpr size_t max_discovery_source_bytes = 4096;
constexpr size_t max_discovery_name_bytes = 1024;

easycon_native_status fail(
    easycon_native_error* error,
    easycon_native_status status,
    std::string_view message) noexcept {
    return easycon::native::detail::set_error(error, status, message);
}

class ComApartment {
public:
    ComApartment() noexcept : result_(CoInitializeEx(nullptr, COINIT_MULTITHREADED)) {}

    ~ComApartment() {
        if (SUCCEEDED(result_)) {
            CoUninitialize();
        }
    }

    ComApartment(const ComApartment&) = delete;
    ComApartment& operator=(const ComApartment&) = delete;

    [[nodiscard]] bool available() const noexcept {
        return SUCCEEDED(result_) || result_ == RPC_E_CHANGED_MODE;
    }

private:
    HRESULT result_;
};

class MediaFoundationLifetime {
public:
    MediaFoundationLifetime() noexcept : result_(MFStartup(MF_VERSION, MFSTARTUP_LITE)) {}

    ~MediaFoundationLifetime() {
        if (SUCCEEDED(result_)) {
            static_cast<void>(MFShutdown());
        }
    }

    MediaFoundationLifetime(const MediaFoundationLifetime&) = delete;
    MediaFoundationLifetime& operator=(const MediaFoundationLifetime&) = delete;

    [[nodiscard]] bool available() const noexcept { return SUCCEEDED(result_); }

private:
    HRESULT result_;
};

struct MediaFoundationDevices {
    IMFActivate** data{};
    UINT32 count{};

    ~MediaFoundationDevices() {
        if (data == nullptr) {
            return;
        }
        for (UINT32 index = 0; index < count; ++index) {
            if (data[index] != nullptr) {
                data[index]->Release();
            }
        }
        CoTaskMemFree(static_cast<void*>(data));
    }
};

struct CoTaskMemString {
    wchar_t* data{};

    ~CoTaskMemString() { CoTaskMemFree(data); }
};

bool wide_to_utf8(
    const wchar_t* text,
    size_t length,
    size_t maximum,
    std::string& output) {
    if (text == nullptr || length == 0 || length > static_cast<size_t>(INT_MAX)) {
        return false;
    }
    const auto input_length = static_cast<int>(length);
    const auto required = WideCharToMultiByte(
        CP_UTF8,
        WC_ERR_INVALID_CHARS,
        text,
        input_length,
        nullptr,
        0,
        nullptr,
        nullptr);
    if (required <= 0 || static_cast<size_t>(required) > maximum) {
        return false;
    }
    output.resize(static_cast<size_t>(required));
    const auto written = WideCharToMultiByte(
        CP_UTF8,
        WC_ERR_INVALID_CHARS,
        text,
        input_length,
        output.data(),
        required,
        nullptr,
        nullptr);
    return written == required;
}

easycon_native_status discover_directshow(
    std::vector<easycon::native::capture::Descriptor>& output,
    easycon_native_error* error) {
    ComApartment apartment;
    if (!apartment.available()) {
        return fail(error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "DirectShow COM initialization failed");
    }
    Microsoft::WRL::ComPtr<ICreateDevEnum> device_enumerator;
    auto result = CoCreateInstance(
        CLSID_SystemDeviceEnum,
        nullptr,
        CLSCTX_INPROC_SERVER,
        IID_PPV_ARGS(device_enumerator.GetAddressOf()));
    if (FAILED(result)) {
        return fail(error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "DirectShow enumerator creation failed");
    }
    Microsoft::WRL::ComPtr<IEnumMoniker> monikers;
    result = device_enumerator->CreateClassEnumerator(
        CLSID_VideoInputDeviceCategory, monikers.GetAddressOf(), 0);
    if (result == S_FALSE) {
        return EASYCON_NATIVE_STATUS_OK;
    }
    if (FAILED(result)) {
        return fail(error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "DirectShow enumeration failed");
    }
    Microsoft::WRL::ComPtr<IBindCtx> bind_context;
    if (FAILED(CreateBindCtx(0, bind_context.GetAddressOf()))) {
        return fail(error, EASYCON_NATIVE_STATUS_BACKEND_ERROR, "DirectShow bind context failed");
    }

    Microsoft::WRL::ComPtr<IMoniker> moniker;
    ULONG fetched = 0;
    while (output.size() < max_discovered_sources &&
           monikers->Next(1, moniker.ReleaseAndGetAddressOf(), &fetched) == S_OK) {
        Microsoft::WRL::ComPtr<IPropertyBag> properties;
        if (FAILED(moniker->BindToStorage(
                bind_context.Get(), nullptr, IID_PPV_ARGS(properties.GetAddressOf())))) {
            continue;
        }
        VARIANT friendly{};
        VariantInit(&friendly);
        const auto friendly_result = properties->Read(L"FriendlyName", &friendly, nullptr);
        std::string display_name;
        const bool valid_name = SUCCEEDED(friendly_result) && friendly.vt == VT_BSTR &&
                                wide_to_utf8(
                                    friendly.bstrVal,
                                    SysStringLen(friendly.bstrVal),
                                    max_discovery_name_bytes,
                                    display_name);
        VariantClear(&friendly);
        if (!valid_name) {
            continue;
        }

        LPOLESTR display = nullptr;
        if (FAILED(moniker->GetDisplayName(bind_context.Get(), nullptr, &display)) ||
            display == nullptr) {
            continue;
        }
        const auto display_length = wcsnlen_s(display, max_discovery_source_bytes + 1);
        std::string source_id;
        const bool valid_source = display_length <= max_discovery_source_bytes &&
                                  wide_to_utf8(
                                      display,
                                      display_length,
                                      max_discovery_source_bytes - 6,
                                      source_id);
        CoTaskMemFree(display);
        if (!valid_source) {
            continue;
        }
        source_id.insert(0, "dshow:");
        output.push_back({std::move(source_id), std::move(display_name)});
    }
    return EASYCON_NATIVE_STATUS_OK;
}

easycon_native_status discover_media_foundation(
    std::vector<easycon::native::capture::Descriptor>& output,
    easycon_native_error* error) {
    ComApartment apartment;
    MediaFoundationLifetime media_foundation;
    if (!apartment.available() || !media_foundation.available()) {
        return fail(
            error,
            EASYCON_NATIVE_STATUS_BACKEND_ERROR,
            "Media Foundation initialization failed");
    }
    Microsoft::WRL::ComPtr<IMFAttributes> attributes;
    if (FAILED(MFCreateAttributes(attributes.GetAddressOf(), 1)) ||
        FAILED(attributes->SetGUID(
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID))) {
        return fail(
            error,
            EASYCON_NATIVE_STATUS_BACKEND_ERROR,
            "Media Foundation attributes failed");
    }
    MediaFoundationDevices devices;
    if (FAILED(MFEnumDeviceSources(attributes.Get(), &devices.data, &devices.count))) {
        return fail(
            error,
            EASYCON_NATIVE_STATUS_BACKEND_ERROR,
            "Media Foundation enumeration failed");
    }
    for (UINT32 index = 0;
         index < devices.count && output.size() < max_discovered_sources;
         ++index) {
        CoTaskMemString friendly;
        CoTaskMemString symbolic;
        UINT32 friendly_length = 0;
        UINT32 symbolic_length = 0;
        if (FAILED(devices.data[index]->GetAllocatedString(
                MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
                &friendly.data,
                &friendly_length)) ||
            FAILED(devices.data[index]->GetAllocatedString(
                MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                &symbolic.data,
                &symbolic_length))) {
            continue;
        }
        std::string display_name;
        std::string source_id;
        if (!wide_to_utf8(
                friendly.data,
                friendly_length,
                max_discovery_name_bytes,
                display_name) ||
            !wide_to_utf8(
                symbolic.data,
                symbolic_length,
                max_discovery_source_bytes - 5,
                source_id)) {
            continue;
        }
        source_id.insert(0, "msmf:");
        output.push_back({std::move(source_id), std::move(display_name)});
    }
    return EASYCON_NATIVE_STATUS_OK;
}

}  // namespace

namespace easycon::native::capture {

easycon_native_status platform_discover(
    uint32_t backend,
    std::vector<Descriptor>& output,
    easycon_native_error* error) {
    if (backend == EASYCON_NATIVE_CAPTURE_BACKEND_DIRECTSHOW) {
        return discover_directshow(output, error);
    }
    if (backend == EASYCON_NATIVE_CAPTURE_BACKEND_MEDIA_FOUNDATION) {
        return discover_media_foundation(output, error);
    }
    return fail(
        error,
        EASYCON_NATIVE_STATUS_UNSUPPORTED,
        "capture discovery adapter is unavailable on Windows");
}

easycon_native_status platform_open(
    uint32_t backend,
    std::string_view source,
    const easycon_native_capture_options& options,
    easycon_native_capture_profile* profile,
    easycon_native_error* error) {
    static_cast<void>(source);
    static_cast<void>(options);
    static_cast<void>(profile);
    if (backend == EASYCON_NATIVE_CAPTURE_BACKEND_DIRECTSHOW ||
        backend == EASYCON_NATIVE_CAPTURE_BACKEND_MEDIA_FOUNDATION) {
        const auto api = backend == EASYCON_NATIVE_CAPTURE_BACKEND_DIRECTSHOW
                             ? cv::CAP_DSHOW
                             : cv::CAP_MSMF;
        static_cast<void>(cv::videoio_registry::hasBackend(api));
        return fail(
            error,
            EASYCON_NATIVE_STATUS_UNSUPPORTED,
            "Windows capture open is disabled until bounded timeout capability is verified");
    }
    return fail(
        error,
        EASYCON_NATIVE_STATUS_UNSUPPORTED,
        "capture open adapter is unavailable on Windows");
}

}  // namespace easycon::native::capture
