#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <cstdio>
#include <cwchar>

// Exercise real resource creation, GPU clear, copy and CPU readback as well as
// loading the architecture-matched UMD. Odd dimensions exercise RowPitch.
static bool check_readback(ID3D11Device *device, ID3D11DeviceContext *context) {
    D3D11_TEXTURE2D_DESC desc = {};
    desc.Width = 31;
    desc.Height = 17;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_RENDER_TARGET;
    ID3D11Texture2D *target = nullptr;
    ID3D11Texture2D *staging = nullptr;
    ID3D11RenderTargetView *view = nullptr;
    HRESULT hr = device->CreateTexture2D(&desc, nullptr, &target);
    if (SUCCEEDED(hr)) hr = device->CreateRenderTargetView(target, nullptr, &view);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    if (SUCCEEDED(hr)) hr = device->CreateTexture2D(&desc, nullptr, &staging);
    bool valid = false;
    if (SUCCEEDED(hr)) {
        const FLOAT red[4] = {1.0f, 0.0f, 0.0f, 1.0f};
        context->ClearRenderTargetView(view, red);
        context->CopyResource(staging, target);
        D3D11_MAPPED_SUBRESOURCE mapped = {};
        hr = context->Map(staging, 0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(hr)) {
            valid = mapped.pData && mapped.RowPitch >= desc.Width * 4;
            if (!valid) std::fprintf(stderr, "D3D11 Map returned an invalid pointer or row pitch.\n");
            for (UINT y = 0; valid && y < desc.Height; ++y) {
                const auto *row = static_cast<const unsigned char *>(mapped.pData) + y * mapped.RowPitch;
                for (UINT x = 0; x < desc.Width; ++x) {
                    const auto *pixel = row + x * 4;
                    if (pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 255 || pixel[3] != 255) {
                        std::fprintf(stderr, "D3D11 readback mismatch at (%u,%u): %02x %02x %02x %02x.\n",
                                     x, y, pixel[0], pixel[1], pixel[2], pixel[3]);
                        valid = false;
                        break;
                    }
                }
            }
            context->Unmap(staging, 0);
        }
    }
    if (FAILED(hr)) std::fprintf(stderr, "D3D11 clear/readback failed: 0x%08lx.\n", static_cast<unsigned long>(hr));
    if (view) view->Release();
    if (staging) staging->Release();
    if (target) target->Release();
    if (valid) std::printf("Direct3D 11 clear/copy/readback: all 527 pixels match.\n");
    return valid;
}

int wmain() {
    std::printf("Direct3D 11 smoke: %zu-bit process.\n", sizeof(void *) * 8);
    IDXGIFactory1 *factory = nullptr;
    HRESULT hr = CreateDXGIFactory1(__uuidof(IDXGIFactory1), reinterpret_cast<void **>(&factory));
    if (FAILED(hr)) {
        std::fprintf(stderr, "CreateDXGIFactory1 failed: 0x%08lx\n", static_cast<unsigned long>(hr));
        return 1;
    }
    IDXGIAdapter1 *helios = nullptr;
    for (UINT index = 0; ; ++index) {
        IDXGIAdapter1 *adapter = nullptr;
        hr = factory->EnumAdapters1(index, &adapter);
        if (hr == DXGI_ERROR_NOT_FOUND) break;
        if (FAILED(hr) || !adapter) {
            std::fprintf(stderr, "EnumAdapters1 failed: 0x%08lx\n", static_cast<unsigned long>(hr));
            factory->Release();
            return 2;
        }
        DXGI_ADAPTER_DESC1 description = {};
        hr = adapter->GetDesc1(&description);
        if (FAILED(hr)) {
            std::fprintf(stderr, "GetDesc1 failed: 0x%08lx\n", static_cast<unsigned long>(hr));
            adapter->Release();
            factory->Release();
            return 2;
        }
        std::wprintf(L"DXGI adapter %u: %ls\n", index, description.Description);
        if (std::wcsstr(description.Description, L"Helios")) {
            helios = adapter;
            break;
        }
        adapter->Release();
    }
    if (!helios) {
        factory->Release();
        std::fprintf(stderr, "Helios DXGI adapter was not found.\n");
        return 2;
    }
    ID3D11Device *device = nullptr;
    ID3D11DeviceContext *context = nullptr;
    D3D_FEATURE_LEVEL level = D3D_FEATURE_LEVEL_9_1;
    const D3D_FEATURE_LEVEL requested[] = {D3D_FEATURE_LEVEL_11_0};
    hr = D3D11CreateDevice(helios, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, requested, 1,
                           D3D11_SDK_VERSION, &device, &level, &context);
    if (FAILED(hr) || !device || !context || level < D3D_FEATURE_LEVEL_11_0) {
        if (context) context->Release();
        if (device) device->Release();
        std::fprintf(stderr, "D3D11CreateDevice on Helios failed: 0x%08lx\n", static_cast<unsigned long>(hr));
        helios->Release();
        factory->Release();
        return 3;
    }
    std::printf("Direct3D 11 device created on Helios; feature level 0x%x.\n", level);
    const bool readbackOk = check_readback(device, context);
    context->Release();
    device->Release();
    helios->Release();
    factory->Release();
    return readbackOk ? 0 : 4;
}
