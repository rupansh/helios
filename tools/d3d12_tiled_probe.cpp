// Native Windows D3D12 / Helios tiled-resource acceptance. Each invocation owns
// one device and one case; the companion runner uses an interactive task.
// Exit 77 means BLOCKED, never a skipped PASS. This is a bounded behavior suite,
// not a claim of complete feature-level or format conformance.
//
// Contract references:
// https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html
// https://learn.microsoft.com/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-updatetilemappings
// https://learn.microsoft.com/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-copytilemappings
// https://learn.microsoft.com/windows/win32/api/d3d12/nf-d3d12-id3d12graphicscommandlist-copytiles
// Buffers use the defined data-inheritance model and explicit aliasing barriers.
// Packed mips are copied through CopyTextureRegion, never through CopyTiles.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d12.h>
#include <d3d12sdklayers.h>
#include <d3dcompiler.h>
#include <dxgi1_6.h>
#include <tlhelp32.h>
#include <wrl/client.h>
#include "d3d12_native_identity.h"
#include <algorithm>
#include <array>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <stdexcept>
#include <string>
#include <vector>

static constexpr UINT tile_bytes = D3D12_TILED_RESOURCE_TILE_SIZE_IN_BYTES;
static constexpr UINT tile_words = tile_bytes / sizeof(uint32_t);
static constexpr uint32_t sentinel = 0xa5a5a5a5u;
static const char *selected_case = "unselected";
// Conservatively retain this state for the child's entire remaining lifetime.
// A failure after any queue operation exits without C++ owner destruction; it
// never infers retirement from an error, a gate signal, or an elapsed timeout.
static bool queue_operation_issued = false;
[[noreturn]] static void exit_without_unwind(unsigned code) noexcept {
    std::fflush(stdout); std::fflush(stderr);
    TerminateProcess(GetCurrentProcess(), code);
    std::_Exit(static_cast<int>(code)); // no automatic-object unwinding if TerminateProcess fails
}
[[noreturn]] static void fail_after_submission(const char *message) noexcept {
    std::fprintf(stderr, "FAIL,tiled,%s,%s;child-terminated-without-owner-unwind\n", selected_case, message);
    exit_without_unwind(1);
}
static void protect_owner_during_unwind() noexcept {
    if (queue_operation_issued && std::uncaught_exceptions())
        fail_after_submission("C++ exception after queue operation; GPU retirement not established");
}
// Library allocations can throw independently of check()/require(). Intercept
// their unwind before a COM resource, heap, allocator, queue, or fence is released.
template <typename T> class ComPtr : public Microsoft::WRL::ComPtr<T> {
public:
    using Microsoft::WRL::ComPtr<T>::ComPtr;
    ~ComPtr() noexcept { protect_owner_during_unwind(); }
};
struct Blocked : std::runtime_error { using std::runtime_error::runtime_error; };
static void require(bool condition, const char *message) {
    if (!condition) {
        if (queue_operation_issued) fail_after_submission(message);
        throw std::runtime_error(message);
    }
}
static void check(HRESULT hr, const char *operation) {
    if (FAILED(hr)) {
        std::fprintf(stderr, "HRESULT,%s,%08lx\n", operation, static_cast<unsigned long>(hr));
        require(false, operation);
    }
}
static void require_cap(bool available, const char *message) {
    if (!available) {
        if (queue_operation_issued) fail_after_submission("capability block after queue operation");
        throw Blocked(message);
    }
}
struct Event {
    HANDLE handle = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    Event() { require(handle != nullptr, "CreateEvent"); }
    ~Event() { protect_owner_during_unwind(); CloseHandle(handle); }
    Event(const Event &) = delete;
    Event &operator=(const Event &) = delete;
};
static void wait_fence(ID3D12Device *device, ID3D12Fence *fence, UINT64 value) {
    Event event;
    check(fence->SetEventOnCompletion(value, event.handle), "SetEventOnCompletion");
    require(WaitForSingleObject(event.handle, 10000) == WAIT_OBJECT_0,
        "owned GPU boundary timed out; timeout is not completion");
    const UINT64 completed = fence->GetCompletedValue();
    require(completed != UINT64_MAX && completed >= value,
        "completion event lacks an authenticated fence value");
    check(device->GetDeviceRemovedReason(), "device health after completion");
}
static ComPtr<ID3D12Fence> make_fence(ID3D12Device *device) {
    ComPtr<ID3D12Fence> fence;
    check(device->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&fence)), "CreateFence");
    return fence;
}
static void native_environment() {
    DWORD session = 0;
    require(ProcessIdToSessionId(GetCurrentProcessId(), &session) && session != 0,
        "run through an interactive scheduled task");
    std::fprintf(stderr, "PROCESS,%lu,session,%lu\n", GetCurrentProcessId(), session);
    for (auto name : {L"VKD3D_FEATURE_LEVEL", L"VKD3D_SHADER_MODEL", L"D3D12SDKPath", L"D3D12SDKVersion"}) {
        if (GetEnvironmentVariableW(name, nullptr, 0)) {
            std::fprintf(stderr, "FORBIDDEN_ENV,%ls\n", name);
            throw std::runtime_error("capability/runtime override present");
        }
    }
    for (auto name : {L"HELIOS_WSI_ASYNC_PRESENT"}) {
        wchar_t value[16] = {};
        require(GetEnvironmentVariableW(name, value, 16) == 1 && value[0] == L'1',
            "runner must explicitly preserve asynchronous present and retire feedback");
    }
}
static void native_modules() {
    wchar_t system[MAX_PATH] = {};
    require(GetSystemDirectoryW(system, MAX_PATH) != 0, "GetSystemDirectory");
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
    require(snapshot != INVALID_HANDLE_VALUE, "module snapshot");
    MODULEENTRY32W module = {}; module.dwSize = sizeof(module);
    bool runtime = false, core = false, dxgi = false, icd = false, valid = true;
    UINT umd_count = 0;
    for (BOOL next = Module32FirstW(snapshot, &module); next; next = Module32NextW(snapshot, &module)) {
        bool *native = !_wcsicmp(module.szModule, L"d3d12.dll") ? &runtime :
            !_wcsicmp(module.szModule, L"D3D12Core.dll") ? &core :
            !_wcsicmp(module.szModule, L"dxgi.dll") ? &dxgi : nullptr;
        const bool is_umd = helios_native_umd12_name(module.szModule);
        const bool is_icd = !_wcsnicmp(module.szModule, L"vulkan_virtio", 13);
        if (native || is_umd || is_icd || !_wcsicmp(module.szModule, L"vulkan-1.dll") ||
            !_wcsicmp(module.szModule, L"d3d12SDKLayers.dll"))
            std::fprintf(stderr, "MODULE,%ls,%ls\n", module.szModule, module.szExePath);
        if (native) {
            *native = true;
            const auto expected = std::wstring(system) + L"\\" + module.szModule;
            valid &= !_wcsicmp(expected.c_str(), module.szExePath);
        }
        umd_count += is_umd; icd |= is_icd;
        valid &= _wcsicmp(module.szModule, L"helios_vkd3d.dll") != 0 &&
            _wcsicmp(module.szModule, L"d3d10warp.dll") != 0;
    }
    CloseHandle(snapshot);
    require(valid && runtime && core && dxgi && umd_count == 1 && icd,
        "Microsoft system runtime and loaded native Helios UMD/ICD required");
}
struct Context {
    ComPtr<ID3D12Device> device;
    ComPtr<ID3D12InfoQueue> info;
    D3D12_FEATURE_DATA_D3D12_OPTIONS options = {};
    explicit Context(bool debug) {
        bool debug_available = true;
        if (debug) {
            ComPtr<ID3D12Debug> layer;
            const HRESULT hr = D3D12GetDebugInterface(IID_PPV_ARGS(&layer));
            debug_available = SUCCEEDED(hr);
            std::printf("CAP,NativeDebugLayerHRESULT,%08lx\n", static_cast<unsigned long>(hr));
            if (debug_available) layer->EnableDebugLayer();
        }
        ComPtr<IDXGIFactory4> factory;
        check(CreateDXGIFactory2(0, IID_PPV_ARGS(&factory)), "CreateDXGIFactory2");
        for (UINT i = 0; ; ++i) {
            ComPtr<IDXGIAdapter1> adapter;
            const auto hr = factory->EnumAdapters1(i, &adapter);
            if (hr == DXGI_ERROR_NOT_FOUND) break;
            check(hr, "EnumAdapters1");
            DXGI_ADAPTER_DESC1 desc = {};
            check(adapter->GetDesc1(&desc), "GetDesc1");
            if (desc.VendorId != 0x1af4 || desc.DeviceId != 0x1050 ||
                (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) continue;
            require(!device, "ambiguous multiple Helios adapters");
            check(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_11_0,
                IID_PPV_ARGS(&device)), "native D3D12CreateDevice exact Helios");
            std::printf("ADAPTER,%04x,%04x,%08lx:%08lx,%ls\n", desc.VendorId, desc.DeviceId,
                static_cast<unsigned long>(desc.AdapterLuid.HighPart), desc.AdapterLuid.LowPart, desc.Description);
        }
        require(device != nullptr, "exact Helios PCI adapter unavailable; no fallback");
        native_modules();
        check(device->CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS, &options, sizeof(options)),
            "D3D12_OPTIONS");
        std::printf("CAP,TiledResourcesTier,%u\n", static_cast<unsigned>(options.TiledResourcesTier));
        if (debug) {
            // Even a missing debug layer must first identify the admitted
            // FL11_0 device's native runtime/UMD/ICD for the BLOCKED receipt.
            require_cap(debug_available, "required native debug layer unavailable");
            check(device.As(&info), "ID3D12InfoQueue");
            check(info->SetBreakOnSeverity(D3D12_MESSAGE_SEVERITY_ERROR, FALSE), "disable debug error break");
            check(info->SetBreakOnSeverity(D3D12_MESSAGE_SEVERITY_CORRUPTION, FALSE), "disable debug corruption break");
        }
    }
    void tier(D3D12_TILED_RESOURCES_TIER minimum) {
        require_cap(options.TiledResourcesTier >= minimum, "required native tiled-resource tier not reported");
    }
    void rgba8(bool volume = false, bool msaa = false) {
        D3D12_FEATURE_DATA_FORMAT_SUPPORT format = {}; format.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        check(device->CheckFeatureSupport(D3D12_FEATURE_FORMAT_SUPPORT, &format, sizeof(format)), "FORMAT_SUPPORT RGBA8");
        std::printf("CAP,RGBA8_SUPPORT,%08x,%08x\n", format.Support1, format.Support2);
        require_cap((format.Support2 & D3D12_FORMAT_SUPPORT2_TILED) != 0, "RGBA8 tiled format not reported");
        require_cap((format.Support1 & (volume ? D3D12_FORMAT_SUPPORT1_TEXTURE3D : D3D12_FORMAT_SUPPORT1_TEXTURE2D)) != 0,
            "required RGBA8 texture dimension not reported");
        if (msaa) {
            D3D12_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS quality = {};
            quality.Format = format.Format; quality.SampleCount = 4;
            quality.Flags = D3D12_MULTISAMPLE_QUALITY_LEVELS_FLAG_TILED_RESOURCE;
            check(device->CheckFeatureSupport(D3D12_FEATURE_MULTISAMPLE_QUALITY_LEVELS, &quality, sizeof(quality)),
                "tiled RGBA8 4x quality query");
            std::printf("CAP,RGBA8_TILED_MSAA4_QUALITY,%u\n", quality.NumQualityLevels);
            require_cap(quality.NumQualityLevels > 0, "mandatory tiled RGBA8 4x MSAA unavailable");
            require_cap((format.Support1 & D3D12_FORMAT_SUPPORT1_MULTISAMPLE_RESOLVE) != 0,
                "RGBA8 resolve unavailable for independent MSAA copy witness");
        }
    }
};
struct Queue {
    ID3D12Device *device;
    ComPtr<ID3D12CommandQueue> queue;
    ComPtr<ID3D12Fence> boundary;
    UINT64 sequence = 0;
    explicit Queue(ID3D12Device *dev) : device(dev), boundary(make_fence(dev)) {
        D3D12_COMMAND_QUEUE_DESC desc = {}; desc.Type = D3D12_COMMAND_LIST_TYPE_DIRECT;
        check(dev->CreateCommandQueue(&desc, IID_PPV_ARGS(&queue)), "CreateCommandQueue");
    }
    // Only this probe's submitted work/mappings precede this owned fence. This
    // never requests device idle, drains other queues, or resets an allocator.
    void complete() {
        queue_operation_issued = true;
        check(queue->Signal(boundary.Get(), ++sequence), "owned queue Signal");
        wait_fence(device, boundary.Get(), sequence);
    }
};
struct Commands {
    ComPtr<ID3D12CommandAllocator> allocator;
    ComPtr<ID3D12GraphicsCommandList> list;
    explicit Commands(ID3D12Device *device) {
        check(device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT, IID_PPV_ARGS(&allocator)), "CreateCommandAllocator");
        check(device->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, allocator.Get(), nullptr,
            IID_PPV_ARGS(&list)), "CreateCommandList");
    }
    void submit(Queue &queue) {
        check(list->Close(), "Close");
        queue_operation_issued = true;
        ID3D12CommandList *lists[] = {list.Get()}; queue.queue->ExecuteCommandLists(1, lists);
    }
};
static void transition(Commands &commands, ID3D12Resource *resource,
                       D3D12_RESOURCE_STATES before, D3D12_RESOURCE_STATES after) {
    D3D12_RESOURCE_BARRIER barrier = {}; barrier.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    barrier.Transition = {resource, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, before, after};
    commands.list->ResourceBarrier(1, &barrier);
}
static void alias(Commands &commands, ID3D12Resource *after) {
    D3D12_RESOURCE_BARRIER barrier = {}; barrier.Type = D3D12_RESOURCE_BARRIER_TYPE_ALIASING;
    barrier.Aliasing = {nullptr, after};
    commands.list->ResourceBarrier(1, &barrier);
}
static D3D12_RESOURCE_DESC buffer_desc(UINT64 bytes) {
    D3D12_RESOURCE_DESC desc = {}; desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
    desc.Width = bytes; desc.Height = desc.DepthOrArraySize = desc.MipLevels = 1;
    desc.SampleDesc.Count = 1; desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    return desc;
}
static D3D12_RESOURCE_DESC texture_desc(bool volume, UINT width, UINT height, UINT16 depth, UINT16 mips) {
    D3D12_RESOURCE_DESC desc = {};
    desc.Dimension = volume ? D3D12_RESOURCE_DIMENSION_TEXTURE3D : D3D12_RESOURCE_DIMENSION_TEXTURE2D;
    desc.Width = width; desc.Height = height; desc.DepthOrArraySize = depth; desc.MipLevels = mips;
    desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM; desc.SampleDesc.Count = 1;
    desc.Layout = D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE;
    return desc;
}
static ComPtr<ID3D12Resource> committed(ID3D12Device *device, D3D12_RESOURCE_DESC desc,
                                      D3D12_HEAP_TYPE type, D3D12_RESOURCE_STATES state) {
    D3D12_HEAP_PROPERTIES heap = {}; heap.Type = type; heap.CreationNodeMask = heap.VisibleNodeMask = 1;
    ComPtr<ID3D12Resource> resource;
    const HRESULT hr = device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, nullptr,
        IID_PPV_ARGS(&resource));
    if (FAILED(hr)) {
        std::fprintf(stderr, "RESOURCE_FAILURE,format=%u,samples=%u,flags=%u,width=%llu,height=%u,layers=%u,state=%u\n",
            desc.Format, desc.SampleDesc.Count, desc.Flags, desc.Width, desc.Height, desc.DepthOrArraySize, state);
        ComPtr<ID3D12InfoQueue> info;
        if (SUCCEEDED(device->QueryInterface(IID_PPV_ARGS(&info)))) {
            for (UINT64 i = 0; i < info->GetNumStoredMessages(); ++i) {
                SIZE_T bytes = 0; if (FAILED(info->GetMessage(i, nullptr, &bytes))) continue;
                std::vector<unsigned char> storage(bytes);
                auto *message = reinterpret_cast<D3D12_MESSAGE *>(storage.data());
                if (SUCCEEDED(info->GetMessage(i, message, &bytes)))
                    std::fprintf(stderr, "NATIVE_DEBUG,%u,%s\n", message->ID, message->pDescription);
            }
        }
    }
    check(hr, "CreateCommittedResource staging/resolve");
    return resource;
}
static ComPtr<ID3D12Resource> reserved(ID3D12Device *device, D3D12_RESOURCE_DESC desc,
                                     D3D12_RESOURCE_STATES state = D3D12_RESOURCE_STATE_COPY_DEST) {
    ComPtr<ID3D12Resource> resource;
    const HRESULT hr = device->CreateReservedResource(&desc, state, nullptr, IID_PPV_ARGS(&resource));
    if (FAILED(hr)) {
        ComPtr<ID3D12InfoQueue> info;
        if (SUCCEEDED(device->QueryInterface(IID_PPV_ARGS(&info)))) {
            for (UINT64 i = 0; i < info->GetNumStoredMessages(); ++i) {
                SIZE_T bytes = 0; if (FAILED(info->GetMessage(i, nullptr, &bytes))) continue;
                std::vector<unsigned char> storage(bytes);
                auto *message = reinterpret_cast<D3D12_MESSAGE *>(storage.data());
                if (SUCCEEDED(info->GetMessage(i, message, &bytes)))
                    std::fprintf(stderr, "NATIVE_DEBUG,%u,%s\n", message->ID, message->pDescription);
            }
        }
    }
    check(hr, "CreateReservedResource");
    return resource;
}
static ComPtr<ID3D12Heap> make_heap(ID3D12Device *device, UINT tiles,
                                  D3D12_HEAP_FLAGS flags = D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS,
                                  UINT64 alignment = 0) {
    require(tiles != 0 && tiles <= 4096, "invalid probe heap extent");
    D3D12_HEAP_DESC desc = {}; desc.SizeInBytes = UINT64(tiles) * tile_bytes;
    desc.Properties.Type = D3D12_HEAP_TYPE_DEFAULT;
    desc.Properties.CreationNodeMask = desc.Properties.VisibleNodeMask = 1;
    desc.Flags = flags; desc.Alignment = alignment;
    ComPtr<ID3D12Heap> heap;
    check(device->CreateHeap(&desc, IID_PPV_ARGS(&heap)), "CreateHeap tiles");
    return heap;
}
static void map_all(Queue &queue, ID3D12Resource *resource, ID3D12Heap *heap) {
    queue_operation_issued = true;
    queue.queue->UpdateTileMappings(resource, 1, nullptr, nullptr, heap, 1, nullptr, nullptr, nullptr,
        D3D12_TILE_MAPPING_FLAG_NONE);
}
static void map_range(Queue &queue, ID3D12Resource *resource, UINT start, UINT count,
                      ID3D12Heap *heap, UINT offset, D3D12_TILE_RANGE_FLAGS flag = D3D12_TILE_RANGE_FLAG_NONE) {
    const D3D12_TILED_RESOURCE_COORDINATE coord = {start, 0, 0, 0};
    const D3D12_TILE_REGION_SIZE region = {count, FALSE, 0, 0, 0};
    queue_operation_issued = true;
    queue.queue->UpdateTileMappings(resource, 1, &coord, &region, heap, 1, &flag, &offset, &count,
        D3D12_TILE_MAPPING_FLAG_NONE);
}
static void unmap_all(Queue &queue, ID3D12Resource *resource) {
    const auto flag = D3D12_TILE_RANGE_FLAG_NULL;
    queue_operation_issued = true;
    queue.queue->UpdateTileMappings(resource, 1, nullptr, nullptr, nullptr, 1, &flag, nullptr, nullptr,
        D3D12_TILE_MAPPING_FLAG_NONE);
}
static uint32_t pattern(UINT tag, UINT word) {
    return 0x46bc912du ^ (0x9e3779b9u * tag) ^ (0x10204081u * word);
}
static ComPtr<ID3D12Resource> staging(ID3D12Device *device, UINT64 bytes, bool upload) {
    auto resource = committed(device, buffer_desc(bytes), upload ? D3D12_HEAP_TYPE_UPLOAD : D3D12_HEAP_TYPE_READBACK,
        upload ? D3D12_RESOURCE_STATE_GENERIC_READ : D3D12_RESOURCE_STATE_COPY_DEST);
    void *data = nullptr; const D3D12_RANGE empty = {0, 0};
    check(resource->Map(0, &empty, &data), "initialize staging sentinel");
    std::memset(data, 0xa5, static_cast<size_t>(bytes));
    const D3D12_RANGE written = {0, static_cast<SIZE_T>(bytes)}; resource->Unmap(0, &written);
    return resource;
}

static void no_debug_errors(Context &context) {
    if (!context.info) return;
    for (UINT64 i = 0; i < context.info->GetNumStoredMessages(); ++i) {
        SIZE_T bytes = 0; check(context.info->GetMessage(i, nullptr, &bytes), "debug message size");
        std::vector<unsigned char> storage(bytes); auto *message = reinterpret_cast<D3D12_MESSAGE *>(storage.data());
        check(context.info->GetMessage(i, message, &bytes), "debug message");
        if (message->Severity <= D3D12_MESSAGE_SEVERITY_ERROR) {
            std::fprintf(stderr, "DEBUG_ERROR,%s\n", message->pDescription);
            require(false, "native debug layer rejected control");
        }
    }
}

// DDI0102+ no-output rasterization, independent of resource tiling. The oracle
// distinguishes actual sample-frequency invocation from one pixel invocation,
// checks TIR coverage, then replays the same list with all PSO transitions.
static void no_output_msaa(Context &context) {
    D3D12_FEATURE_DATA_D3D12_OPTIONS19 options = {};
    check(context.device->CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS19, &options, sizeof(options)), "OPTIONS19");
    std::printf("CAP,NoOutputSampleCounts,%08x\n", options.SupportedSampleCountsWithNoOutputs);
    require_cap((options.SupportedSampleCountsWithNoOutputs & 0x1f) == 0x1f, "no-output sample counts 1/2/4/8/16 unavailable");
    const char *source = R"(
RWByteAddressBuffer output : register(u0);
cbuffer Data : register(b0) { uint baseWord; };
float4 vs(uint id : SV_VertexID) : SV_Position {
    return float4(id == 1 ? 3 : -1, id == 2 ? 3 : -1, 0, 1);
}
void pixel(float4 p : SV_Position, uint coverage : SV_Coverage) {
    uint address = (baseWord + (uint(p.y) * 8 + uint(p.x)) * 17) * 4;
    uint ignored; output.InterlockedAdd(address, 1, ignored);
    output.Store(address + 64, coverage);
}
void sample_pixel(float4 p : SV_Position, uint sampleIndex : SV_SampleIndex) {
    uint address = (baseWord + (uint(p.y) * 8 + uint(p.x)) * 17 + sampleIndex) * 4;
    uint ignored; output.InterlockedAdd(address, 1, ignored);
})";
    auto compile = [&](const char *entry, const char *target) {
        ComPtr<ID3DBlob> blob, error;
        const HRESULT hr = D3DCompile(source, std::strlen(source), "no-output", nullptr, nullptr,
            entry, target, D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS, 0, &blob, &error);
        if (FAILED(hr) && error) std::fprintf(stderr, "%s\n", static_cast<const char *>(error->GetBufferPointer()));
        check(hr, "compile no-output shader"); return blob;
    };
    auto vs = compile("vs", "vs_5_1"), pixel = compile("pixel", "ps_5_1"), sample = compile("sample_pixel", "ps_5_1");
    D3D12_ROOT_PARAMETER params[2] = {};
    params[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_UAV;
    params[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
    params[1].Constants.Num32BitValues = 1;
    D3D12_ROOT_SIGNATURE_DESC root_desc = {}; root_desc.NumParameters = 2; root_desc.pParameters = params;
    ComPtr<ID3DBlob> serialized, errors;
    check(D3D12SerializeRootSignature(&root_desc, D3D_ROOT_SIGNATURE_VERSION_1, &serialized, &errors), "serialize no-output root");
    ComPtr<ID3D12RootSignature> root;
    check(context.device->CreateRootSignature(0, serialized->GetBufferPointer(), serialized->GetBufferSize(),
        IID_PPV_ARGS(&root)), "create no-output root");
    struct Case { UINT samples, forced; bool per_sample; };
    std::vector<Case> cases;
    for (UINT count : {1u, 2u, 4u, 8u, 16u}) {
        cases.push_back({count, 0, false}); cases.push_back({count, 0, true});
    }
    for (UINT count : {1u, 4u, 8u, 16u}) cases.push_back({1, count, false});
    cases.push_back({4, 1, false}); // ForcedSampleCount=1 overrides SampleDesc when no outputs exist.
    std::vector<ComPtr<ID3D12PipelineState>> pipelines;
    for (const auto &test : cases) {
        D3D12_GRAPHICS_PIPELINE_STATE_DESC desc = {};
        desc.pRootSignature = root.Get(); desc.VS = {vs->GetBufferPointer(), vs->GetBufferSize()};
        auto *ps = test.per_sample ? sample.Get() : pixel.Get();
        desc.PS = {ps->GetBufferPointer(), ps->GetBufferSize()};
        desc.SampleMask = UINT_MAX; desc.SampleDesc.Count = test.samples;
        desc.SampleDesc.Quality = test.per_sample && test.samples > 1 ? D3D12_STANDARD_MULTISAMPLE_PATTERN : 0;
        desc.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID; desc.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
        desc.RasterizerState.DepthClipEnable = TRUE; desc.RasterizerState.ForcedSampleCount = test.forced;
        desc.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        ComPtr<ID3D12PipelineState> pso;
        check(context.device->CreateGraphicsPipelineState(&desc, IID_PPV_ARGS(&pso)), "create no-output PSO");
        pipelines.push_back(pso);
    }
    constexpr UINT guard = 16, pixels = 8 * 4, fields = 17, region_words = 2 * guard + pixels * fields;
    const UINT words = static_cast<UINT>(cases.size()) * region_words;
    auto upload = staging(context.device.Get(), UINT64(words) * 4, true);
    auto readback = staging(context.device.Get(), UINT64(words) * 4, false);
    void *data = nullptr; const D3D12_RANGE empty = {0, 0}, whole = {0, SIZE_T(words) * 4};
    check(upload->Map(0, &empty, &data), "initialize no-output counters");
    for (UINT i = 0; i < cases.size(); ++i)
        std::memset(static_cast<uint32_t *>(data) + i * region_words + guard, 0, pixels * fields * 4);
    upload->Unmap(0, &whole);
    auto desc = buffer_desc(UINT64(words) * 4); desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    auto output = committed(context.device.Get(), desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    Queue queue(context.device.Get()); Commands init(context.device.Get()), commands(context.device.Get());
    init.list->CopyBufferRegion(output.Get(), 0, upload.Get(), 0, UINT64(words) * 4);
    transition(init, output.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    transition(commands, output.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    commands.list->SetGraphicsRootSignature(root.Get());
    commands.list->SetGraphicsRootUnorderedAccessView(0, output->GetGPUVirtualAddress());
    commands.list->OMSetRenderTargets(0, nullptr, FALSE, nullptr);
    const D3D12_VIEWPORT viewport = {0, 0, 8, 4, 0, 1}; const D3D12_RECT scissor = {0, 0, 8, 4};
    commands.list->RSSetViewports(1, &viewport); commands.list->RSSetScissorRects(1, &scissor);
    commands.list->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    for (UINT i = 0; i < cases.size(); ++i) {
        commands.list->SetPipelineState(pipelines[i].Get());
        commands.list->SetGraphicsRoot32BitConstant(1, i * region_words + guard, 0);
        commands.list->DrawInstanced(3, 1, 0, 0);
    }
    transition(commands, output.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
    commands.list->CopyBufferRegion(readback.Get(), 0, output.Get(), 0, UINT64(words) * 4);
    init.submit(queue); commands.submit(queue);
    for (UINT replay = 1; replay <= 2; ++replay) {
        if (replay == 2) { ID3D12CommandList *lists[] = {commands.list.Get()}; queue.queue->ExecuteCommandLists(1, lists); }
        queue.complete();
        check(readback->Map(0, &whole, &data), "read no-output counters");
        UINT mismatches = 0;
        for (UINT i = 0; i < cases.size(); ++i) for (UINT j = 0; j < region_words; ++j) {
            const auto &test = cases[i]; const UINT count = test.forced ? test.forced : test.samples;
            uint32_t expected = sentinel;
            if (j >= guard && j < region_words - guard) {
                const UINT field = (j - guard) % fields;
                expected = test.per_sample ? (field < count ? replay : 0) :
                    (field == 0 ? replay : field == 16 ? (1u << count) - 1u : 0);
            }
            const auto got = static_cast<const uint32_t *>(data)[i * region_words + j];
            if (got != expected && mismatches++ < 16)
                std::printf("NO_OUTPUT_MISMATCH,case,%u,samples,%u,forced,%u,sample-frequency,%u,word,%u,got,%08x,want,%08x\n",
                    i, test.samples, test.forced, test.per_sample, j, got, expected);
        }
        readback->Unmap(0, &empty);
        std::printf("READBACK,no-output-msaa,replay,%u,cases,%zu,words,%u,mismatches,%u\n", replay, cases.size(), words, mismatches);
        require(mismatches == 0, "no-output rasterization readback differs");
    }
    no_debug_errors(context);
}

// Native DXBC immediate-buffer control, independent of resource tiling.
static void immediate_constants(Context &context) {
    const uint32_t values[] = {0, 0x80000000, 1, 0x80000001, 0x007fffff,
        0x00800000, 0x3dcccccd, 0x3f800000, 0x40000000, 0xbf800000,
        0x7f7fffff, 0x7f800000, 0xff800000, 0x7fc12345, 0x7f812345, 0xffc54321};
    // Only one dynamically indexed table: FXC emits one ICB, with 16 vectors.
    // A second table could share the ICB and invalidate the OOB expectations.
    const char shader[] = R"(
cbuffer Params : register(b0) { uint base; };
RWStructuredBuffer<uint> result : register(u0);
[numthreads(64, 1, 1)] void main(uint3 id : SV_DispatchThreadID) {
    const uint bits[16] = {0, 0x80000000, 1, 0x80000001, 0x007fffff,
        0x00800000, 0x3dcccccd, 0x3f800000, 0x40000000, 0xbf800000,
        0x7f7fffff, 0x7f800000, 0xff800000, 0x7fc12345, 0x7f812345, 0xffc54321};
    uint index = id.x + base;
    result[4 * id.x] = bits[index];
    result[4 * id.x + 1] = bits[(index ^ 11) & 15];
    result[4 * id.x + 2] = asuint(asfloat(bits[7 + (id.x & 1)]) * 2.0);
    result[4 * id.x + 3] = bits[14] ^ id.x;
}
)";
    ComPtr<ID3DBlob> code, errors, serialized;
    check(D3DCompile(shader, sizeof(shader) - 1, "native-immediate-constants", nullptr, nullptr,
        "main", "cs_5_1", D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &code, &errors), "compile immediate constants");
    // Assert the actual DXBC declaration shape before assuming its bounds.
    ComPtr<ID3DBlob> assembly;
    check(D3DDisassemble(code->GetBufferPointer(), code->GetBufferSize(), 0, nullptr, &assembly),
        "disassemble immediate constants");
    const std::string disassembly(static_cast<const char *>(assembly->GetBufferPointer()), assembly->GetBufferSize());
    const auto declaration = disassembly.find("dcl_immediateConstantBuffer");
    require(declaration != std::string::npos, "FXC did not emit an immediate buffer");
    const auto start = disassembly.find('{', declaration);
    UINT nesting = 0, vectors = 0;
    for (size_t i = start; i < disassembly.size(); ++i) {
        if (disassembly[i] == '{') { if (++nesting == 2) ++vectors; }
        else if (disassembly[i] == '}' && !--nesting) break;
    }
    require(start != std::string::npos && !nesting && vectors == 16,
        "immediate-buffer size changed; OOB oracle needs updating");
    D3D12_ROOT_PARAMETER parameters[2] = {};
    parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
    parameters[0].Constants.Num32BitValues = 1;
    parameters[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_UAV;
    D3D12_ROOT_SIGNATURE_DESC desc = {}; desc.NumParameters = 2; desc.pParameters = parameters;
    check(D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &serialized, &errors), "serialize immediate root");
    ComPtr<ID3D12RootSignature> root;
    check(context.device->CreateRootSignature(0, serialized->GetBufferPointer(), serialized->GetBufferSize(),
        IID_PPV_ARGS(&root)), "create immediate root");
    D3D12_COMPUTE_PIPELINE_STATE_DESC pso_desc = {}; pso_desc.pRootSignature = root.Get();
    pso_desc.CS = {code->GetBufferPointer(), code->GetBufferSize()}; ComPtr<ID3D12PipelineState> pso;
    check(context.device->CreateComputePipelineState(&pso_desc, IID_PPV_ARGS(&pso)), "create immediate PSO");
    const UINT words = 256, bytes = words * 4;
    auto output_desc = buffer_desc(bytes); output_desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    auto output = committed(context.device.Get(), output_desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    auto readback = staging(context.device.Get(), bytes * 2, false);
    Queue queue(context.device.Get()); Commands commands(context.device.Get());
    commands.list->SetComputeRootSignature(root.Get()); commands.list->SetPipelineState(pso.Get());
    commands.list->SetComputeRootUnorderedAccessView(1, output->GetGPUVirtualAddress());
    const uint32_t bases[] = {0, 0xfffffff0};
    for (UINT arm = 0; arm < 2; ++arm) {
        if (arm) transition(commands, output.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        commands.list->SetComputeRoot32BitConstant(0, bases[arm], 0); commands.list->Dispatch(1, 1, 1);
        transition(commands, output.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
        commands.list->CopyBufferRegion(readback.Get(), arm * bytes, output.Get(), 0, bytes);
    }
    commands.submit(queue); queue.complete();
    void *mapped = nullptr; const D3D12_RANGE range = {0, 2 * bytes}, empty = {0, 0};
    check(readback->Map(0, &range, &mapped), "immediate constants readback");
    const auto *actual = static_cast<const uint32_t *>(mapped); UINT mismatches = 0;
    for (UINT arm = 0; arm < 2; ++arm) for (UINT i = 0; i < 64; ++i) {
        const uint32_t index = bases[arm] + i;
        const uint32_t expected[] = {index < 16 ? values[index] : 0, values[(index ^ 11) & 15],
            (i & 1) ? 0x40800000u : 0x40000000u, values[14] ^ i};
        for (UINT lane = 0; lane < 4; ++lane) if (actual[arm * words + 4 * i + lane] != expected[lane]) {
            if (mismatches < 16) std::printf("DIFF,immediate-constants,arm=%u,index=%u,lane=%u,expected=%08x,actual=%08x\n",
                arm, i, lane, expected[lane], actual[arm * words + 4 * i + lane]);
            ++mismatches;
        }
    }
    readback->Unmap(0, &empty);
    std::printf("CHECK,immediate-constants,words=512,mismatches=%u\n", mismatches);
    require(!mismatches, "raw immediate constants, float arithmetic, literal or OOB read differs");
    no_debug_errors(context);
}

// Independent native control for the raw color/depth transfer used by MSAA
// CopyTiles. The committed arms do not need or bypass tiled admission. The
// separate tile_roundtrip arm requires tier2, reads tiles from one reserved
// image and writes a second image, with independent integer shader witnesses.
// Integer RT exports and integer texture loads avoid lossy fragment-depth
// export. Every sample/layer receives a different pattern.
static void committed_depth_copy(Context &context, bool immediates, bool tile_roundtrip = false) {
    // The native tier2/3 runtime disallows array mips smaller than one tile.
    // Use full 64x64 D32 MSAA4 tiles; the committed controls keep their old size.
    const UINT width = tile_roundtrip ? 64 : 16, height = tile_roundtrip ? 64 : 2, layers = 2, samples = 4;
    const UINT words = width * height * layers * samples;
    if (tile_roundtrip) context.tier(D3D12_TILED_RESOURCES_TIER_2);
    const uint32_t values[] = {0, 0x80000000, 1, 0x80000001, 0x007fffff,
        0x00800000, 0x3dcccccd, 0x3f800000, 0x40000000, 0xbf800000,
        0x7f7fffff, 0x7f800000, 0xff800000, 0x7fc12345, 0x7f812345, 0xffc54321};
    for (auto format : {DXGI_FORMAT_R32_UINT, DXGI_FORMAT_D32_FLOAT}) {
        D3D12_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS quality = {};
        quality.Format = format; quality.SampleCount = samples;
        if (tile_roundtrip && format == DXGI_FORMAT_D32_FLOAT)
            quality.Flags = D3D12_MULTISAMPLE_QUALITY_LEVELS_FLAG_TILED_RESOURCE;
        check(context.device->CheckFeatureSupport(D3D12_FEATURE_MULTISAMPLE_QUALITY_LEVELS,
            &quality, sizeof(quality)), "committed MSAA4 quality query");
        require_cap(quality.NumQualityLevels != 0, "committed integer/depth MSAA4 unavailable");
    }
    const char shader[] = R"(
cbuffer Params : register(b0) { uint layer; };
Texture2DMSArray<uint, 4> source : register(t0);
RWStructuredBuffer<uint> result : register(u0);
ByteAddressBuffer patterns : register(t1);
float4 vs(uint id : SV_VertexID) : SV_Position {
    return float4((id == 2 ? 3.0 : -1.0), (id == 1 ? 3.0 : -1.0), 0.0, 1.0);
}
uint ps(float4 position : SV_Position, uint sample_index : SV_SampleIndex) : SV_Target {
    uint index = (uint(position.x) + 5 * uint(position.y) + 3 * sample_index + 7 * layer) & 15;
#if PROBE_IMMEDIATES
    const uint bits[16] = {0, 0x80000000, 1, 0x80000001, 0x007fffff,
        0x00800000, 0x3dcccccd, 0x3f800000, 0x40000000, 0xbf800000,
        0x7f7fffff, 0x7f800000, 0xff800000, 0x7fc12345, 0x7f812345, 0xffc54321};
    return bits[index];
#else
    return patterns.Load(4 * index);
#endif
}
[numthreads(16, 1, 1)] void cs(uint3 id : SV_DispatchThreadID) {
    for (uint sample_index = 0; sample_index < 4; ++sample_index)
        result[((id.z * PROBE_HEIGHT + id.y) * PROBE_WIDTH + id.x) * 4 + sample_index] = source.Load(id, sample_index);
}
)";
    auto compile = [&](const char *entry, const char *profile) {
        ComPtr<ID3DBlob> code, errors;
        const D3D_SHADER_MACRO macros[] = {{"PROBE_IMMEDIATES", immediates ? "1" : "0"},
            {"PROBE_WIDTH", tile_roundtrip ? "64" : "16"}, {"PROBE_HEIGHT", tile_roundtrip ? "64" : "2"}, {nullptr, nullptr}};
        const HRESULT hr = D3DCompile(shader, sizeof(shader) - 1, "committed-depth-control",
            macros, nullptr, entry, profile, D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_WARNINGS_ARE_ERRORS,
            0, &code, &errors);
        if (errors) std::fprintf(stderr, "%s\n", static_cast<const char *>(errors->GetBufferPointer()));
        check(hr, "compile native depth-control shader");
        if (!std::strcmp(entry, "ps")) {
            UINT signaling = 0, quiet = 0;
            const auto *bytes = static_cast<const unsigned char *>(code->GetBufferPointer());
            for (SIZE_T i = 0; i + 4 <= code->GetBufferSize(); i += 4) {
                uint32_t word; std::memcpy(&word, bytes + i, 4);
                signaling += word == 0x7f812345; quiet += word == 0x7fc12345;
            }
            std::printf("SHADER_INPUT,immediates=%u,signaling_literal_words=%u,quiet_literal_words=%u\n",
                immediates, signaling, quiet);
        }
        return code;
    };
    auto vs = compile("vs", "vs_5_1"), ps = compile("ps", "ps_5_1"), cs = compile("cs", "cs_5_1");
    D3D12_DESCRIPTOR_RANGE range = {}; range.RangeType = D3D12_DESCRIPTOR_RANGE_TYPE_SRV;
    range.NumDescriptors = 1;
    D3D12_ROOT_PARAMETER parameters[4] = {};
    parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
    parameters[0].Constants.Num32BitValues = 1;
    parameters[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE;
    parameters[1].DescriptorTable = {1, &range};
    parameters[2].ParameterType = D3D12_ROOT_PARAMETER_TYPE_UAV;
    parameters[3].ParameterType = D3D12_ROOT_PARAMETER_TYPE_SRV; parameters[3].Descriptor.ShaderRegister = 1;
    D3D12_ROOT_SIGNATURE_DESC root_desc = {}; root_desc.NumParameters = 4; root_desc.pParameters = parameters;
    ComPtr<ID3DBlob> serialized, errors;
    check(D3D12SerializeRootSignature(&root_desc, D3D_ROOT_SIGNATURE_VERSION_1, &serialized, &errors),
        "serialize depth-control root signature");
    ComPtr<ID3D12RootSignature> root;
    check(context.device->CreateRootSignature(0, serialized->GetBufferPointer(), serialized->GetBufferSize(),
        IID_PPV_ARGS(&root)), "create depth-control root signature");
    D3D12_GRAPHICS_PIPELINE_STATE_DESC graphics = {};
    graphics.pRootSignature = root.Get();
    graphics.VS = {vs->GetBufferPointer(), vs->GetBufferSize()};
    graphics.PS = {ps->GetBufferPointer(), ps->GetBufferSize()};
    graphics.SampleMask = UINT_MAX; graphics.SampleDesc.Count = samples;
    graphics.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID;
    graphics.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
    graphics.RasterizerState.DepthClipEnable = TRUE;
    graphics.RasterizerState.MultisampleEnable = TRUE;
    auto &blend = graphics.BlendState.RenderTarget[0];
    blend.SrcBlend = blend.SrcBlendAlpha = D3D12_BLEND_ONE;
    blend.DestBlend = blend.DestBlendAlpha = D3D12_BLEND_ZERO;
    blend.BlendOp = blend.BlendOpAlpha = D3D12_BLEND_OP_ADD;
    blend.LogicOp = D3D12_LOGIC_OP_NOOP; blend.RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL;
    graphics.DepthStencilState.DepthFunc = D3D12_COMPARISON_FUNC_ALWAYS;
    graphics.DepthStencilState.FrontFace.StencilFunc = D3D12_COMPARISON_FUNC_ALWAYS;
    graphics.DepthStencilState.FrontFace.StencilFailOp = D3D12_STENCIL_OP_KEEP;
    graphics.DepthStencilState.FrontFace.StencilDepthFailOp = D3D12_STENCIL_OP_KEEP;
    graphics.DepthStencilState.FrontFace.StencilPassOp = D3D12_STENCIL_OP_KEEP;
    graphics.DepthStencilState.BackFace = graphics.DepthStencilState.FrontFace;
    graphics.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
    graphics.NumRenderTargets = 1; graphics.RTVFormats[0] = DXGI_FORMAT_R32_UINT;
    ComPtr<ID3D12PipelineState> draw_pipeline, read_pipeline;
    check(context.device->CreateGraphicsPipelineState(&graphics, IID_PPV_ARGS(&draw_pipeline)), "integer sample PSO");
    D3D12_COMPUTE_PIPELINE_STATE_DESC compute = {}; compute.pRootSignature = root.Get();
    compute.CS = {cs->GetBufferPointer(), cs->GetBufferSize()};
    check(context.device->CreateComputePipelineState(&compute, IID_PPV_ARGS(&read_pipeline)), "integer sample read PSO");
    auto desc = texture_desc(false, width, height, layers, 1);
    desc.Layout = D3D12_TEXTURE_LAYOUT_UNKNOWN; desc.SampleDesc.Count = samples;
    desc.Format = DXGI_FORMAT_R32_UINT; desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    auto input = committed(context.device.Get(), desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_RENDER_TARGET);
    // D3D12 requires an attachment flag on every multisample resource, including
    // a texture used only as a copy destination and sampled readback witness.
    auto output = committed(context.device.Get(), desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    desc.Format = DXGI_FORMAT_D32_FLOAT; desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL;
    if (tile_roundtrip) desc.Layout = D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE;
    auto depth = tile_roundtrip ? reserved(context.device.Get(), desc) :
        committed(context.device.Get(), desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    ComPtr<ID3D12Resource> tile_destination, tile_linear, tile_readback;
    ComPtr<ID3D12Heap> source_heap, destination_heap;
    ComPtr<ID3D12DescriptorHeap> tile_dsv;
    D3D12_TILE_SHAPE tile_shape = {};
    const UINT tile_offset = 1, tile_span = layers * tile_bytes + tile_offset;
    if (tile_roundtrip) {
        tile_destination = reserved(context.device.Get(), desc);
        D3D12_DESCRIPTOR_HEAP_DESC heap_desc = {}; heap_desc.Type = D3D12_DESCRIPTOR_HEAP_TYPE_DSV; heap_desc.NumDescriptors = 1;
        check(context.device->CreateDescriptorHeap(&heap_desc, IID_PPV_ARGS(&tile_dsv)), "D32 tile initialization DSV heap");
        D3D12_DEPTH_STENCIL_VIEW_DESC view = {}; view.Format = DXGI_FORMAT_D32_FLOAT;
        view.ViewDimension = D3D12_DSV_DIMENSION_TEXTURE2DMSARRAY; view.Texture2DMSArray.ArraySize = layers;
        context.device->CreateDepthStencilView(tile_destination.Get(), &view, tile_dsv->GetCPUDescriptorHandleForHeapStart());
        tile_linear = committed(context.device.Get(), buffer_desc(tile_span), D3D12_HEAP_TYPE_DEFAULT,
            D3D12_RESOURCE_STATE_COPY_DEST);
        tile_readback = staging(context.device.Get(), tile_span, false);
        D3D12_PACKED_MIP_INFO packed = {}; UINT total = 0;
        context.device->GetResourceTiling(depth.Get(), &total, &packed, &tile_shape, nullptr, 0, nullptr);
        require(total == layers && !packed.NumPackedMips && tile_shape.WidthInTexels == 64 &&
            tile_shape.HeightInTexels == 64 && tile_shape.DepthInTexels == 1, "D32 MSAA4 tile geometry");
        source_heap = make_heap(context.device.Get(), 64, D3D12_HEAP_FLAG_ALLOW_ONLY_RT_DS_TEXTURES,
            D3D12_DEFAULT_MSAA_RESOURCE_PLACEMENT_ALIGNMENT);
        destination_heap = make_heap(context.device.Get(), 64, D3D12_HEAP_FLAG_ALLOW_ONLY_RT_DS_TEXTURES,
            D3D12_DEFAULT_MSAA_RESOURCE_PLACEMENT_ALIGNMENT);
    }
    auto result_desc = buffer_desc(words * sizeof(uint32_t)); result_desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    auto result = committed(context.device.Get(), result_desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
    auto readback = staging(context.device.Get(), 2 * words * sizeof(uint32_t), false);
    auto patterns = staging(context.device.Get(), sizeof(values), true);
    void *pattern_map = nullptr; const D3D12_RANGE no_read = {0, 0};
    check(patterns->Map(0, &no_read, &pattern_map), "raw pattern upload");
    std::memcpy(pattern_map, values, sizeof(values)); patterns->Unmap(0, nullptr);
    D3D12_DESCRIPTOR_HEAP_DESC heap_desc = {}; heap_desc.Type = D3D12_DESCRIPTOR_HEAP_TYPE_RTV; heap_desc.NumDescriptors = layers;
    ComPtr<ID3D12DescriptorHeap> rtvs, srvs;
    check(context.device->CreateDescriptorHeap(&heap_desc, IID_PPV_ARGS(&rtvs)), "integer RT descriptors");
    heap_desc.Type = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV; heap_desc.NumDescriptors = 2;
    heap_desc.Flags = D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE;
    check(context.device->CreateDescriptorHeap(&heap_desc, IID_PPV_ARGS(&srvs)), "integer read descriptors");
    const UINT rtv_stride = context.device->GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
    const UINT srv_stride = context.device->GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
    auto rtv = rtvs->GetCPUDescriptorHandleForHeapStart();
    for (UINT layer = 0; layer < layers; ++layer) {
        D3D12_RENDER_TARGET_VIEW_DESC view = {}; view.Format = DXGI_FORMAT_R32_UINT;
        view.ViewDimension = D3D12_RTV_DIMENSION_TEXTURE2DMSARRAY;
        view.Texture2DMSArray.FirstArraySlice = layer; view.Texture2DMSArray.ArraySize = 1;
        context.device->CreateRenderTargetView(input.Get(), &view, rtv); rtv.ptr += rtv_stride;
    }
    auto srv = srvs->GetCPUDescriptorHandleForHeapStart();
    for (auto resource : {input.Get(), output.Get()}) {
        D3D12_SHADER_RESOURCE_VIEW_DESC view = {}; view.Format = DXGI_FORMAT_R32_UINT;
        view.Shader4ComponentMapping = D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING;
        view.ViewDimension = D3D12_SRV_DIMENSION_TEXTURE2DMSARRAY; view.Texture2DMSArray.ArraySize = layers;
        context.device->CreateShaderResourceView(resource, &view, srv); srv.ptr += srv_stride;
    }
    Queue queue(context.device.Get()); Commands commands(context.device.Get());
    if (tile_roundtrip) {
        map_all(queue, depth.Get(), source_heap.Get());
        map_all(queue, tile_destination.Get(), destination_heap.Get());
        alias(commands, depth.Get()); alias(commands, tile_destination.Get());
    }
    auto *list = commands.list.Get();
    list->SetGraphicsRootSignature(root.Get()); list->SetPipelineState(draw_pipeline.Get());
    list->SetGraphicsRootShaderResourceView(3, patterns->GetGPUVirtualAddress());
    list->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    const D3D12_VIEWPORT viewport = {0, 0, float(width), float(height), 0, 1};
    const D3D12_RECT scissor = {0, 0, LONG(width), LONG(height)};
    list->RSSetViewports(1, &viewport); list->RSSetScissorRects(1, &scissor);
    rtv = rtvs->GetCPUDescriptorHandleForHeapStart();
    for (UINT layer = 0; layer < layers; ++layer) {
        list->SetGraphicsRoot32BitConstant(0, layer, 0); list->OMSetRenderTargets(1, &rtv, FALSE, nullptr);
        list->DrawInstanced(3, 1, 0, 0); rtv.ptr += rtv_stride;
    }
    transition(commands, input.Get(), D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_SOURCE);
    for (UINT layer = 0; layer < layers; ++layer) {
        D3D12_TEXTURE_COPY_LOCATION source = {}, destination = {};
        source.Type = destination.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
        source.SubresourceIndex = destination.SubresourceIndex = layer;
        source.pResource = input.Get(); destination.pResource = depth.Get();
        list->CopyTextureRegion(&destination, 0, 0, 0, &source, nullptr);
    }
    transition(commands, depth.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    if (tile_roundtrip) {
        const D3D12_TILE_REGION_SIZE region = {1, TRUE, 1, 1, 1};
        for (UINT layer = 0; layer < layers; ++layer) {
            const D3D12_TILED_RESOURCE_COORDINATE start = {0, 0, 0, layer};
            list->CopyTiles(depth.Get(), &start, &region, tile_linear.Get(), tile_offset + layer * tile_bytes,
                D3D12_TILE_COPY_FLAG_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER);
        }
        transition(commands, tile_linear.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
        // CopyTiles cannot initialize RT/DS metadata. Initialize the reserved
        // destination through a legal clear, using a value absent from the
        // expected bit patterns so a dropped CopyTiles cannot pass readback.
        transition(commands, tile_destination.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_DEPTH_WRITE);
        list->ClearDepthStencilView(tile_dsv->GetCPUDescriptorHandleForHeapStart(), D3D12_CLEAR_FLAG_DEPTH, 0.25f, 0, 0, nullptr);
        transition(commands, tile_destination.Get(), D3D12_RESOURCE_STATE_DEPTH_WRITE, D3D12_RESOURCE_STATE_COPY_DEST);
        for (UINT layer = 0; layer < layers; ++layer) {
            const D3D12_TILED_RESOURCE_COORDINATE start = {0, 0, 0, layer};
            list->CopyTiles(tile_destination.Get(), &start, &region, tile_linear.Get(), tile_offset + layer * tile_bytes,
                D3D12_TILE_COPY_FLAG_LINEAR_BUFFER_TO_SWIZZLED_TILED_RESOURCE);
        }
        transition(commands, tile_destination.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
        list->CopyBufferRegion(tile_readback.Get(), 0, tile_linear.Get(), 0, tile_span);
    }
    for (UINT layer = 0; layer < layers; ++layer) {
        D3D12_TEXTURE_COPY_LOCATION source = {}, destination = {};
        source.Type = destination.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
        source.SubresourceIndex = destination.SubresourceIndex = layer;
        source.pResource = tile_roundtrip ? tile_destination.Get() : depth.Get(); destination.pResource = output.Get();
        list->CopyTextureRegion(&destination, 0, 0, 0, &source, nullptr);
    }
    transition(commands, input.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
    transition(commands, output.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
    ID3D12DescriptorHeap *heaps[] = {srvs.Get()}; list->SetDescriptorHeaps(1, heaps);
    list->SetComputeRootSignature(root.Get()); list->SetPipelineState(read_pipeline.Get());
    list->SetComputeRootUnorderedAccessView(2, result->GetGPUVirtualAddress());
    auto gpu_srv = srvs->GetGPUDescriptorHandleForHeapStart();
    // Capture the untouched integer producer too: a coincidentally wrong
    // producer/consumer pair cannot establish raw depth-copy acceptance.
    for (UINT arm = 0; arm < 2; ++arm) {
        if (arm) transition(commands, result.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        list->SetComputeRootDescriptorTable(1, gpu_srv); list->Dispatch(width / 16, height, layers);
        transition(commands, result.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
        list->CopyBufferRegion(readback.Get(), UINT64(arm) * words * 4, result.Get(), 0, words * 4);
        gpu_srv.ptr += srv_stride;
    }
    commands.submit(queue); queue.complete();
    const D3D12_RANGE range_read = {0, 2 * words * sizeof(uint32_t)}, empty = {0, 0}; void *mapped = nullptr;
    check(readback->Map(0, &range_read, &mapped), "raw depth-copy readback");
    const auto *actual = static_cast<const uint32_t *>(mapped); UINT mismatches = 0;
    for (UINT arm = 0; arm < 2; ++arm) for (UINT layer = 0; layer < layers; ++layer)
        for (UINT y = 0; y < height; ++y) for (UINT x = 0; x < width; ++x) for (UINT sample = 0; sample < samples; ++sample) {
            const UINT index = arm * words + ((layer * height + y) * width + x) * samples + sample;
            const uint32_t expected = values[(x + 5 * y + 3 * sample + 7 * layer) & 15];
            if (actual[index] != expected) {
                if (mismatches < 16) std::printf("DIFF,committed-depth-copy,arm=%u,layer=%u,x=%u,y=%u,sample=%u,expected=%08x,actual=%08x\n",
                    arm, layer, x, y, sample, expected, actual[index]);
                ++mismatches;
            }
        }
    readback->Unmap(0, &empty);
    if (tile_roundtrip) {
        const D3D12_RANGE tiled_range = {0, tile_span};
        check(tile_readback->Map(0, &tiled_range, &mapped), "D32 raw tile layout readback");
        for (UINT layer = 0; layer < layers; ++layer) for (UINT y = 0; y < height; ++y)
            for (UINT x = 0; x < width; ++x) for (UINT sample = 0; sample < samples; ++sample) {
                const UINT offset = tile_offset + layer * tile_bytes +
                    ((y * tile_shape.WidthInTexels + x) * samples + sample) * sizeof(uint32_t);
                uint32_t word; std::memcpy(&word, static_cast<const unsigned char *>(mapped) + offset, sizeof(word));
                mismatches += word != values[(x + 5 * y + 3 * sample + 7 * layer) & 15];
            }
        tile_readback->Unmap(0, &empty);
    }
    std::printf("CHECK,%s,words=%u,mismatches=%u\n", selected_case, (tile_roundtrip ? 3 : 2) * words, mismatches);
    require(!mismatches, "integer producer or raw D32 MSAA roundtrip differs");
    // Check the actual Microsoft validation output, independently of readback.
    no_debug_errors(context);
    if (tile_roundtrip)
        std::puts("COVERAGE,native-D32-MSAA4-CopyTiles-both-directions+two-independent-reserved-images+two-array-edge-tiles+byte-offset1+integer-producer-and-consumer+raw-tile-layout+special-bits+owned-fence;mapping-alias-semantics=not-tested");
    else
        std::puts("COVERAGE,committed-R32_UINT-to-D32-to-R32_UINT+MSAA4+two-array-layers+integer-producer-witness+special-bits+owned-fence;not-tiled-conformance");
}
static ComPtr<ID3D12Resource> upload_tiles(ID3D12Device *device, const std::vector<int> &tags) {
    auto upload = staging(device, tags.size() * tile_bytes, true);
    void *data = nullptr; const D3D12_RANGE empty = {0, 0};
    check(upload->Map(0, &empty, &data), "Map upload pattern");
    auto *words = static_cast<uint32_t *>(data);
    for (size_t tile = 0; tile < tags.size(); ++tile)
        for (UINT word = 0; word < tile_words; ++word)
            words[tile * tile_words + word] = tags[tile] < 0 ? 0 : pattern(static_cast<UINT>(tags[tile]), word);
    upload->Unmap(0, nullptr);
    return upload;
}
static void expect_tiles(ID3D12Resource *readback, const std::vector<int> &tags) {
    void *data = nullptr; const D3D12_RANGE range = {0, tags.size() * tile_bytes};
    check(readback->Map(0, &range, &data), "Map completed tile readback");
    const auto *words = static_cast<const uint32_t *>(data);
    bool matches = true;
    for (size_t tile = 0; tile < tags.size() && matches; ++tile) {
        for (UINT word = 0; word < tile_words; ++word) {
            const auto expected = tags[tile] < 0 ? 0 : pattern(static_cast<UINT>(tags[tile]), word);
            if (words[tile * tile_words + word] != expected) {
                std::fprintf(stderr, "MISMATCH,tile,%zu,word,%u,actual,%08x,expected,%08x\n",
                    tile, word, words[tile * tile_words + word], expected);
                matches = false; break;
            }
        }
    }
    const D3D12_RANGE empty = {0, 0}; readback->Unmap(0, &empty);
    require(matches, "GPU tile readback mismatch");
}
static void observe(Queue &queue, ID3D12Resource *resource, const std::vector<int> &tags) {
    auto readback = staging(queue.device, tags.size() * tile_bytes, false);
    Commands commands(queue.device); alias(commands, resource);
    commands.list->CopyBufferRegion(readback.Get(), 0, resource, 0, tags.size() * tile_bytes);
    commands.submit(queue); queue.complete(); expect_tiles(readback.Get(), tags);
}
static ComPtr<ID3D12Resource> seed_heap(Queue &queue, ID3D12Heap *heap, const std::vector<int> &tags) {
    auto resource = reserved(queue.device, buffer_desc(tags.size() * tile_bytes));
    auto upload = upload_tiles(queue.device, tags);
    map_all(queue, resource.Get(), heap);
    Commands commands(queue.device); alias(commands, resource.Get());
    commands.list->CopyBufferRegion(resource.Get(), 0, upload.Get(), 0, tags.size() * tile_bytes);
    transition(commands, resource.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    commands.submit(queue); queue.complete();
    return resource;
}
struct Tiling {
    UINT total = 0;
    D3D12_PACKED_MIP_INFO packed = {};
    D3D12_TILE_SHAPE shape = {};
    std::vector<D3D12_SUBRESOURCE_TILING> subresources;
};
static Tiling get_tiling(ID3D12Device *device, ID3D12Resource *resource) {
    const auto desc = resource->GetDesc();
    const UINT subresources = desc.MipLevels * (desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D ? desc.DepthOrArraySize : 1u);
    Tiling result; result.subresources.resize(subresources);
    UINT count = subresources;
    device->GetResourceTiling(resource, &result.total, &result.packed, &result.shape, &count, 0, result.subresources.data());
    require(count == subresources && result.total > 0 && result.total <= 4096, "invalid full GetResourceTiling result");
    std::printf("TILING,total,%u,standard-mips,%u,packed-mips,%u,packed-tiles,%u,packed-start,%u,shape,%u,%u,%u\n",
        result.total, result.packed.NumStandardMips, result.packed.NumPackedMips,
        result.packed.NumTilesForPackedMips, result.packed.StartTileIndexInOverallResource,
        result.shape.WidthInTexels, result.shape.HeightInTexels, result.shape.DepthInTexels);
    for (UINT i = 0; i < count; ++i) {
        const auto &sub = result.subresources[i];
        std::printf("SUBRESOURCE,%u,%u,%u,%u,%u\n", i, sub.WidthInTiles, sub.HeightInTiles,
            sub.DepthInTiles, sub.StartTileIndexInOverallResource);
    }
    // Query a suffix with an oversized output capacity. Check clamping, exact
    // equality with the full query, and untouched storage past the output count.
    std::vector<D3D12_SUBRESOURCE_TILING> suffix(subresources + 2);
    std::memset(suffix.data(), 0xcd, suffix.size() * sizeof(suffix[0]));
    const UINT first = subresources > 1 ? 1 : 0;
    UINT suffix_count = static_cast<UINT>(suffix.size());
    device->GetResourceTiling(resource, nullptr, nullptr, nullptr, &suffix_count, first, suffix.data());
    require(suffix_count == subresources - first, "GetResourceTiling did not clamp subresource count");
    require(!std::memcmp(suffix.data(), result.subresources.data() + first, suffix_count * sizeof(suffix[0])),
        "partial GetResourceTiling differs from full query");
    const auto *canary = reinterpret_cast<const unsigned char *>(suffix.data() + suffix_count);
    for (size_t i = 0; i < (suffix.size() - suffix_count) * sizeof(suffix[0]); ++i)
        require(canary[i] == 0xcd, "GetResourceTiling overwrote output capacity");
    return result;
}
static void tiling_buffer(Context &context) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2);
    auto heap = make_heap(context.device.Get(), 4);
    Queue queue(context.device.Get());
    auto resource = seed_heap(queue, heap.Get(), {1, 2, 3, 4});
    const auto tiling = get_tiling(context.device.Get(), resource.Get());
    require(tiling.total == 4 && tiling.packed.NumPackedMips == 0 && tiling.packed.NumTilesForPackedMips == 0 &&
        tiling.shape.WidthInTexels == tile_bytes && tiling.shape.HeightInTexels == 1 && tiling.shape.DepthInTexels == 1 &&
        tiling.subresources[0].WidthInTiles == 4 && tiling.subresources[0].HeightInTiles == 1 &&
        tiling.subresources[0].DepthInTiles == 1 && tiling.subresources[0].StartTileIndexInOverallResource == 0,
        "buffer tiling contract mismatch");
    auto readback = staging(context.device.Get(), 4 * tile_bytes, false);
    Commands commands(context.device.Get());
    const D3D12_TILED_RESOURCE_COORDINATE start = {};
    const D3D12_TILE_REGION_SIZE region = {4, FALSE, 0, 0, 0};
    commands.list->CopyTiles(resource.Get(), &start, &region, readback.Get(), 0,
        D3D12_TILE_COPY_FLAG_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER);
    commands.submit(queue); queue.complete(); expect_tiles(readback.Get(), {1, 2, 3, 4});
}
static void tiling_texture(Context &context, bool volume) {
    context.tier(volume ? D3D12_TILED_RESOURCES_TIER_3 : D3D12_TILED_RESOURCES_TIER_2);
    context.rgba8(volume);
    const auto desc = texture_desc(volume, volume ? 64 : 512, volume ? 64 : 256, volume ? 64 : 1, volume ? 7 : 10);
    auto resource = reserved(context.device.Get(), desc);
    const auto tiling = get_tiling(context.device.Get(), resource.Get());
    require(tiling.packed.NumStandardMips + tiling.packed.NumPackedMips == desc.MipLevels,
        "standard plus packed mip count mismatch");
    require(tiling.shape.WidthInTexels && tiling.shape.HeightInTexels && tiling.shape.DepthInTexels &&
        UINT64(tiling.shape.WidthInTexels) * tiling.shape.HeightInTexels * tiling.shape.DepthInTexels * 4 == tile_bytes,
        "RGBA8 tile shape is not 64 KiB");
    UINT standard_tiles = 0;
    for (UINT mip = 0; mip < desc.MipLevels; ++mip) {
        const auto &sub = tiling.subresources[mip];
        const UINT w = std::max(1u, static_cast<UINT>(desc.Width) >> mip);
        const UINT h = std::max(1u, desc.Height >> mip);
        const UINT d = volume ? std::max(1u, UINT(desc.DepthOrArraySize) >> mip) : 1;
        if (mip < tiling.packed.NumStandardMips) {
            require(sub.WidthInTiles == (w + tiling.shape.WidthInTexels - 1) / tiling.shape.WidthInTexels &&
                sub.HeightInTiles == (h + tiling.shape.HeightInTexels - 1) / tiling.shape.HeightInTexels &&
                sub.DepthInTiles == (d + tiling.shape.DepthInTexels - 1) / tiling.shape.DepthInTexels &&
                sub.StartTileIndexInOverallResource == standard_tiles, "standard subresource tiling mismatch");
            standard_tiles += sub.WidthInTiles * sub.HeightInTiles * sub.DepthInTiles;
        } else {
            require(w < tiling.shape.WidthInTexels || h < tiling.shape.HeightInTexels || d < tiling.shape.DepthInTexels,
                "tier 2/3 packed a mip that fills a standard tile in every dimension");
            require(sub.WidthInTiles == 0 && sub.HeightInTiles == 0 && sub.DepthInTiles == 0 &&
                sub.StartTileIndexInOverallResource == D3D12_PACKED_TILE, "packed subresource sentinel mismatch");
        }
    }
    require(standard_tiles + tiling.packed.NumTilesForPackedMips == tiling.total, "single-slice tile accounting mismatch");
    if (tiling.packed.NumPackedMips)
        require(tiling.packed.NumTilesForPackedMips > 0 && tiling.packed.StartTileIndexInOverallResource == standard_tiles,
            "packed mip tail range mismatch");
    // Some implementations legitimately choose no packed mips. Such a run
    // cannot exercise this case's packed-tail acceptance requirement.
    require_cap(tiling.packed.NumPackedMips != 0, "chosen texture has no packed mips; packed-tail behavior unexercised");
    auto heap = make_heap(context.device.Get(), tiling.total, D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES);
    std::vector<D3D12_PLACED_SUBRESOURCE_FOOTPRINT> layouts(desc.MipLevels);
    UINT64 bytes = 0;
    context.device->GetCopyableFootprints(&desc, 0, desc.MipLevels, 0, layouts.data(), nullptr, nullptr, &bytes);
    require(bytes > 0 && bytes < 64 * 1024 * 1024, "invalid mip footprints");
    auto upload = staging(context.device.Get(), bytes, true);
    auto readback = staging(context.device.Get(), bytes, false);
    void *data = nullptr; const D3D12_RANGE empty = {0, 0};
    check(upload->Map(0, &empty, &data), "Map mip upload");
    for (UINT mip = 0; mip < desc.MipLevels; ++mip) {
        const auto &layout = layouts[mip]; const auto &f = layout.Footprint;
        for (UINT z = 0; z < f.Depth; ++z) for (UINT y = 0; y < f.Height; ++y) {
            auto *row = reinterpret_cast<uint32_t *>(static_cast<unsigned char *>(data) + layout.Offset +
                UINT64(z * f.Height + y) * f.RowPitch);
            for (UINT x = 0; x < f.Width; ++x) row[x] = pattern(mip + 1, x + f.Width * (y + f.Height * z));
        }
    }
    upload->Unmap(0, nullptr);
    Queue queue(context.device.Get()); map_all(queue, resource.Get(), heap.Get());
    Commands commands(context.device.Get()); alias(commands, resource.Get());
    for (UINT mip = 0; mip < desc.MipLevels; ++mip) {
        D3D12_TEXTURE_COPY_LOCATION src = {}; src.pResource = upload.Get();
        src.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT; src.PlacedFootprint = layouts[mip];
        D3D12_TEXTURE_COPY_LOCATION dst = {}; dst.pResource = resource.Get();
        dst.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX; dst.SubresourceIndex = mip;
        commands.list->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
    }
    transition(commands, resource.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    for (UINT mip = 0; mip < desc.MipLevels; ++mip) {
        D3D12_TEXTURE_COPY_LOCATION src = {}; src.pResource = resource.Get();
        src.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX; src.SubresourceIndex = mip;
        D3D12_TEXTURE_COPY_LOCATION dst = {}; dst.pResource = readback.Get();
        dst.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT; dst.PlacedFootprint = layouts[mip];
        commands.list->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
    }
    commands.submit(queue); queue.complete();
    const D3D12_RANGE range = {0, static_cast<SIZE_T>(bytes)};
    check(readback->Map(0, &range, &data), "Map all-mip readback");
    bool matches = true;
    for (UINT mip = 0; mip < desc.MipLevels && matches; ++mip) {
        const auto &layout = layouts[mip]; const auto &f = layout.Footprint;
        for (UINT z = 0; z < f.Depth && matches; ++z) for (UINT y = 0; y < f.Height && matches; ++y) {
            const auto *row = reinterpret_cast<const uint32_t *>(static_cast<const unsigned char *>(data) +
                layout.Offset + UINT64(z * f.Height + y) * f.RowPitch);
            for (UINT x = 0; x < f.Width; ++x) if (row[x] != pattern(mip + 1, x + f.Width * (y + f.Height * z))) {
                std::fprintf(stderr, "MISMATCH,mip,%u,xyz,%u,%u,%u\n", mip, x, y, z); matches = false; break;
            }
        }
    }
    readback->Unmap(0, &empty); require(matches, "standard/packed mip GPU readback mismatch");
    unmap_all(queue, resource.Get()); queue.complete();
    std::printf("COVERAGE,all-mip-GPU-readback,%u,packed-mips,%u\n", desc.MipLevels, tiling.packed.NumPackedMips);
}
static void mappings(Context &context) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2);
    auto heap_a = make_heap(context.device.Get(), 6), heap_b = make_heap(context.device.Get(), 6);
    Queue queue(context.device.Get());
    auto resource = seed_heap(queue, heap_a.Get(), {10, 11, 12, 13, 14, 15});
    auto seed_b = seed_heap(queue, heap_b.Get(), {20, 21, 22, 23, 24, 25});
    observe(queue, resource.Get(), {10, 11, 12, 13, 14, 15});
    const D3D12_TILE_RANGE_FLAGS flags[] = {D3D12_TILE_RANGE_FLAG_NONE, D3D12_TILE_RANGE_FLAG_SKIP,
        D3D12_TILE_RANGE_FLAG_NULL, D3D12_TILE_RANGE_FLAG_REUSE_SINGLE_TILE, D3D12_TILE_RANGE_FLAG_NONE};
    const UINT offsets[] = {5, 0, 0, 3, 0}, counts[] = {1, 1, 1, 2, 1};
    queue.queue->UpdateTileMappings(resource.Get(), 1, nullptr, nullptr, heap_a.Get(), 5,
        flags, offsets, counts, D3D12_TILE_MAPPING_FLAG_NONE);
    observe(queue, resource.Get(), {15, 11, -1, 13, 13, 10});
    const D3D12_TILED_RESOURCE_COORDINATE coords[] = {{0, 0, 0, 0}, {4, 0, 0, 0}};
    const UINT b_offsets[] = {1, 3};
    queue.queue->UpdateTileMappings(resource.Get(), 2, coords, nullptr, heap_b.Get(), 2,
        nullptr, b_offsets, nullptr, D3D12_TILE_MAPPING_FLAG_NONE);
    observe(queue, resource.Get(), {21, 11, -1, 13, 23, 10});
    // Null writes must be discarded; both the null tile and live neighbors are read back.
    auto upload = upload_tiles(context.device.Get(), {99});
    Commands write(context.device.Get()); alias(write, resource.Get());
    transition(write, resource.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COPY_DEST);
    write.list->CopyBufferRegion(resource.Get(), 2 * tile_bytes, upload.Get(), 0, tile_bytes);
    transition(write, resource.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    write.submit(queue); queue.complete(); observe(queue, resource.Get(), {21, 11, -1, 13, 23, 10});
    // Restore the repeated mapping, write one virtual tile, and observe both aliases.
    map_range(queue, resource.Get(), 3, 2, heap_a.Get(), 3, D3D12_TILE_RANGE_FLAG_REUSE_SINGLE_TILE);
    Commands repeated(context.device.Get()); alias(repeated, resource.Get());
    transition(repeated, resource.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COPY_DEST);
    repeated.list->CopyBufferRegion(resource.Get(), 3 * tile_bytes, upload.Get(), 0, tile_bytes);
    transition(repeated, resource.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    repeated.submit(queue); queue.complete(); observe(queue, resource.Get(), {21, 11, -1, 99, 99, 10});
    std::puts("COVERAGE,default+null+skip+reuse+multiple-heaps+implicit-single-tile-regions+null-write-discard");
}
static void copy_mappings(Context &context) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2);
    auto heap = make_heap(context.device.Get(), 6); Queue queue(context.device.Get());
    auto resource = seed_heap(queue, heap.Get(), {30, 31, 32, 33, 34, 35});
    const D3D12_TILED_RESOURCE_COORDINATE zero = {}, one = {1, 0, 0, 0};
    const D3D12_TILE_REGION_SIZE five = {5, FALSE, 0, 0, 0}, six = {6, FALSE, 0, 0, 0};
    queue.queue->CopyTileMappings(resource.Get(), &one, resource.Get(), &zero, &five, D3D12_TILE_MAPPING_FLAG_NONE);
    observe(queue, resource.Get(), {30, 30, 31, 32, 33, 34});
    queue.queue->CopyTileMappings(resource.Get(), &zero, resource.Get(), &one, &five, D3D12_TILE_MAPPING_FLAG_NONE);
    observe(queue, resource.Get(), {30, 31, 32, 33, 34, 34});
    auto copy = reserved(context.device.Get(), buffer_desc(6 * tile_bytes), D3D12_RESOURCE_STATE_COPY_SOURCE);
    queue.queue->CopyTileMappings(copy.Get(), &zero, resource.Get(), &zero, &six, D3D12_TILE_MAPPING_FLAG_NONE);
    unmap_all(queue, resource.Get()); queue.complete();
    // Release the original resource only after its last mapping operation completes.
    // The application retains the heap until the copied mappings' GPU accesses retire.
    resource.Reset(); observe(queue, copy.Get(), {30, 31, 32, 33, 34, 34});
    unmap_all(queue, copy.Get()); queue.complete(); copy.Reset(); heap.Reset();
    std::puts("COVERAGE,overlap-forward-snapshot+overlap-backward-snapshot+copy-survives-source-unmap-destruction");
}
static uint32_t pixel(UINT x, UINT y) { return pattern(y + 1, x); }
static void copy_tiles_2d_at_offset(Context &context, UINT linear_offset, bool predicated = false) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2); context.rgba8();
    const auto desc = texture_desc(false, 257, 129, 1, 1);
    auto resource = reserved(context.device.Get(), desc);
    const auto tiling = get_tiling(context.device.Get(), resource.Get());
    require(tiling.shape.WidthInTexels == 128 && tiling.shape.HeightInTexels == 128 &&
        tiling.shape.DepthInTexels == 1 && tiling.total == 6 && !tiling.packed.NumPackedMips,
        "RGBA8 non-MSAA standard shape mismatch");
    auto heap = make_heap(context.device.Get(), tiling.total, D3D12_HEAP_FLAG_ALLOW_ONLY_NON_RT_DS_TEXTURES);
    const UINT bytes = (tiling.total + 2) * tile_bytes;
    auto upload = staging(context.device.Get(), bytes, true), linear = staging(context.device.Get(), bytes, false);
    void *data = nullptr; const D3D12_RANGE empty = {0, 0};
    check(upload->Map(0, &empty, &data), "Map CopyTiles upload");
    for (UINT tile = 0; tile < tiling.total; ++tile) for (UINT y = 0; y < 128; ++y) for (UINT x = 0; x < 128; ++x) {
        const uint32_t value = pixel(tile % 3 * 128 + x, tile / 3 * 128 + y);
        // The buffer offset may be byte-aligned. Avoid unaligned C++ loads/stores.
        std::memcpy(static_cast<unsigned char *>(data) + linear_offset + tile * tile_bytes + (y * 128 + x) * 4,
            &value, sizeof(value));
    }
    upload->Unmap(0, nullptr);
    ComPtr<ID3D12Resource> poison, false_output, predicate, predicate_upload, query_output;
    ComPtr<ID3D12QueryHeap> query_heap;
    if (predicated) {
        poison = staging(context.device.Get(), bytes, true);
        check(poison->Map(0, &empty, &data), "Map poison tile upload");
        std::memset(data, 0x39, bytes); poison->Unmap(0, nullptr);
        false_output = staging(context.device.Get(), bytes, false);
        const UINT64 values[] = {0, UINT64(1) << 32, 1};
        predicate_upload = staging(context.device.Get(), sizeof(values), true);
        check(predicate_upload->Map(0, &empty, &data), "Map predicate upload");
        std::memcpy(data, values, sizeof(values)); predicate_upload->Unmap(0, nullptr);
        predicate = committed(context.device.Get(), buffer_desc(sizeof(values)),
            D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
        D3D12_QUERY_HEAP_DESC query_desc = {};
        query_desc.Type = D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS; query_desc.Count = 1;
        check(context.device->CreateQueryHeap(&query_desc, IID_PPV_ARGS(&query_heap)), "CreateQueryHeap tile copies");
        query_output = staging(context.device.Get(), sizeof(D3D12_QUERY_DATA_PIPELINE_STATISTICS), false);
    }
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT layout = {}; UINT64 footprint_bytes = 0;
    context.device->GetCopyableFootprints(&desc, 0, 1, 0, &layout, nullptr, nullptr, &footprint_bytes);
    auto texture_readback = staging(context.device.Get(), footprint_bytes, false);
    Queue queue(context.device.Get()); map_all(queue, resource.Get(), heap.Get());
    Commands commands(context.device.Get()); alias(commands, resource.Get());
    const D3D12_TILED_RESOURCE_COORDINATE start = {};
    const D3D12_TILE_REGION_SIZE box = {6, TRUE, 3, 2, 1}, sequential = {6, FALSE, 0, 0, 0};
    if (predicated) {
        commands.list->CopyBufferRegion(predicate.Get(), 0, predicate_upload.Get(), 0, 24);
        transition(commands, predicate.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_PREDICATION);
        commands.list->BeginQuery(query_heap.Get(), D3D12_QUERY_TYPE_PIPELINE_STATISTICS, 0);
        commands.list->SetPredication(predicate.Get(), 8, D3D12_PREDICATION_OP_EQUAL_ZERO);
        // SetPredication snapshots the full 64-bit value on the GPU. Changing
        // its source afterward must not replace the captured true decision.
        transition(commands, predicate.Get(), D3D12_RESOURCE_STATE_PREDICATION, D3D12_RESOURCE_STATE_COPY_DEST);
        commands.list->CopyBufferRegion(predicate.Get(), 8, predicate_upload.Get(), 0, 8);
        transition(commands, predicate.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_PREDICATION);
    }
    commands.list->CopyTiles(resource.Get(), &start, &box, upload.Get(), linear_offset,
        D3D12_TILE_COPY_FLAG_LINEAR_BUFFER_TO_SWIZZLED_TILED_RESOURCE);
    if (predicated) {
        commands.list->SetPredication(predicate.Get(), 8, D3D12_PREDICATION_OP_EQUAL_ZERO);
        commands.list->CopyTiles(resource.Get(), &start, &box, poison.Get(), linear_offset,
            D3D12_TILE_COPY_FLAG_LINEAR_BUFFER_TO_SWIZZLED_TILED_RESOURCE);
    }
    transition(commands, resource.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    if (predicated) {
        commands.list->CopyTiles(resource.Get(), &start, &sequential, false_output.Get(), linear_offset,
            D3D12_TILE_COPY_FLAG_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER);
        commands.list->SetPredication(predicate.Get(), 16, D3D12_PREDICATION_OP_EQUAL_ZERO);
    }
    D3D12_TEXTURE_COPY_LOCATION src = {}; src.pResource = resource.Get(); src.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
    D3D12_TEXTURE_COPY_LOCATION dst = {}; dst.pResource = texture_readback.Get();
    dst.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT; dst.PlacedFootprint = layout;
    commands.list->CopyTiles(resource.Get(), &start, &sequential, linear.Get(), linear_offset,
        D3D12_TILE_COPY_FLAG_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER);
    if (predicated) {
        commands.list->SetPredication(predicate.Get(), 8, D3D12_PREDICATION_OP_EQUAL_ZERO);
        // Queries and their resolve are unconditional, including when the
        // active predicate is false. Internal shader work must remain hidden.
        commands.list->EndQuery(query_heap.Get(), D3D12_QUERY_TYPE_PIPELINE_STATISTICS, 0);
        commands.list->ResolveQueryData(query_heap.Get(), D3D12_QUERY_TYPE_PIPELINE_STATISTICS, 0, 1, query_output.Get(), 0);
        commands.list->SetPredication(nullptr, 0, D3D12_PREDICATION_OP_EQUAL_ZERO);
    }
    commands.list->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
    commands.submit(queue); queue.complete();
    const D3D12_RANGE tex_range = {0, static_cast<SIZE_T>(footprint_bytes)};
    check(texture_readback->Map(0, &tex_range, &data), "Map independent texture readback");
    bool matches = true;
    for (UINT y = 0; y < desc.Height; ++y) {
        const auto *row = reinterpret_cast<const uint32_t *>(static_cast<const unsigned char *>(data) + layout.Offset + UINT64(y) * layout.Footprint.RowPitch);
        for (UINT x = 0; x < desc.Width; ++x) matches &= row[x] == pixel(x, y);
    }
    texture_readback->Unmap(0, &empty); require(matches, "CopyTiles input differs from independent CopyTextureRegion witness");
    const D3D12_RANGE linear_range = {0, bytes};
    check(linear->Map(0, &linear_range, &data), "Map linear CopyTiles result");
    const auto *raw = static_cast<const unsigned char *>(data);
    const auto *guard = reinterpret_cast<const unsigned char *>(&sentinel);
    for (UINT i = 0; i < bytes; ++i)
        if (i < linear_offset || i >= linear_offset + tiling.total * tile_bytes)
            matches &= raw[i] == guard[i % sizeof(sentinel)];
    for (UINT tile = 0; tile < tiling.total; ++tile) for (UINT y = 0; y < 128; ++y) for (UINT x = 0; x < 128; ++x) {
        const UINT gx = tile % 3 * 128 + x, gy = tile / 3 * 128 + y;
        if (gx < desc.Width && gy < desc.Height) {
            uint32_t value;
            std::memcpy(&value, raw + linear_offset + tile * tile_bytes + (y * 128 + x) * 4, sizeof(value));
            matches &= value == pixel(gx, gy);
        }
    }
    linear->Unmap(0, &empty); require(matches, "CopyTiles output ordering/edge pitch/buffer bounds mismatch");
    if (predicated) {
        check(false_output->Map(0, &linear_range, &data), "Map false-predicated tile output");
        const auto *untouched = static_cast<const unsigned char *>(data);
        for (UINT i = 0; i < bytes; ++i) matches &= untouched[i] == 0xa5;
        false_output->Unmap(0, &empty); require(matches, "False CopyTiles modified destination bytes");
        const D3D12_RANGE query_range = {0, sizeof(D3D12_QUERY_DATA_PIPELINE_STATISTICS)};
        check(query_output->Map(0, &query_range, &data), "Map tile pipeline statistics");
        D3D12_QUERY_DATA_PIPELINE_STATISTICS statistics;
        std::memcpy(&statistics, data, sizeof(statistics)); query_output->Unmap(0, &empty);
        const D3D12_QUERY_DATA_PIPELINE_STATISTICS zero = {};
        require(!std::memcmp(&statistics, &zero, sizeof(zero)), "Internal CopyTiles work leaked into pipeline statistics");
        std::printf("COVERAGE,predicated-CopyTiles+GPU-predicate-snapshot-high-DWORD+false-write-preservation+unconditional-query-resolve,offset=%u\n", linear_offset);
    }
    std::printf("COVERAGE,CopyTiles-both-directions+box-and-linear-regions+edge-tiles+offset-guard+independent-CopyTextureRegion,offset=%u\n", linear_offset);
}
static void copy_tiles_2d(Context &context) {
    for (UINT offset : {tile_bytes, 32u, 1u}) copy_tiles_2d_at_offset(context, offset);
}
static void copy_tiles_predicated(Context &context) {
    for (UINT offset : {tile_bytes, 32u, 1u}) copy_tiles_2d_at_offset(context, offset, true);
}
static uint32_t sample_pixel(UINT x, UINT y, UINT sample) {
    const UINT delta[] = {0, 32, 64, 128};
    return 0xff000000u | (((x + 3 * y) % 32 + delta[sample] / 4) << 16) |
        (((3 * x + y) % 32 + delta[sample] / 2) << 8) | ((x + y) % 32 + delta[sample]);
}
static void copy_tiles_msaa(Context &context) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2); context.rgba8(false, true);
    auto desc = texture_desc(false, 128, 64, 1, 1);
    desc.SampleDesc.Count = 4; desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    auto resource = reserved(context.device.Get(), desc, D3D12_RESOURCE_STATE_RENDER_TARGET);
    const auto tiling = get_tiling(context.device.Get(), resource.Get());
    require(tiling.total == 2 && tiling.shape.WidthInTexels == 64 && tiling.shape.HeightInTexels == 64 &&
        tiling.shape.DepthInTexels == 1 && !tiling.packed.NumPackedMips, "4x RGBA8 standard tile shape mismatch");
    // Heaps containing MSAA require 4 MiB alignment even though tile addresses
    // and UpdateTileMappings offsets still use 64 KiB units.
    auto heap = make_heap(context.device.Get(), 64, D3D12_HEAP_FLAG_ALLOW_ONLY_RT_DS_TEXTURES,
        D3D12_DEFAULT_MSAA_RESOURCE_PLACEMENT_ALIGNMENT);
    auto upload = staging(context.device.Get(), 2 * tile_bytes, true), linear = staging(context.device.Get(), 2 * tile_bytes, false);
    void *data = nullptr; const D3D12_RANGE empty = {0, 0};
    check(upload->Map(0, &empty, &data), "Map MSAA samples");
    auto *words = static_cast<uint32_t *>(data);
    for (UINT tile = 0; tile < 2; ++tile) for (UINT y = 0; y < 64; ++y) for (UINT x = 0; x < 64; ++x) for (UINT s = 0; s < 4; ++s)
        words[tile * tile_words + (y * 64 + x) * 4 + s] = sample_pixel(tile * 64 + x, y, s);
    upload->Unmap(0, nullptr);
    auto resolved_desc = desc; resolved_desc.SampleDesc.Count = 1; resolved_desc.Layout = D3D12_TEXTURE_LAYOUT_UNKNOWN;
    resolved_desc.Flags = D3D12_RESOURCE_FLAG_NONE;
    auto resolved = committed(context.device.Get(), resolved_desc, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_RESOLVE_DEST);
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT layout = {}; UINT64 bytes = 0;
    context.device->GetCopyableFootprints(&resolved_desc, 0, 1, 0, &layout, nullptr, nullptr, &bytes);
    auto readback = staging(context.device.Get(), bytes, false);
    D3D12_DESCRIPTOR_HEAP_DESC rtv_desc = {}; rtv_desc.Type = D3D12_DESCRIPTOR_HEAP_TYPE_RTV; rtv_desc.NumDescriptors = 1;
    ComPtr<ID3D12DescriptorHeap> rtv;
    check(context.device->CreateDescriptorHeap(&rtv_desc, IID_PPV_ARGS(&rtv)), "MSAA RTV heap");
    const auto handle = rtv->GetCPUDescriptorHandleForHeapStart();
    context.device->CreateRenderTargetView(resource.Get(), nullptr, handle);
    Queue queue(context.device.Get()); map_all(queue, resource.Get(), heap.Get());
    Commands commands(context.device.Get()); alias(commands, resource.Get());
    const float clear[] = {0, 0, 0, 1}; commands.list->ClearRenderTargetView(handle, clear, 0, nullptr);
    transition(commands, resource.Get(), D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_DEST);
    const D3D12_TILED_RESOURCE_COORDINATE start = {};
    const D3D12_TILE_REGION_SIZE region = {2, TRUE, 2, 1, 1};
    commands.list->CopyTiles(resource.Get(), &start, &region, upload.Get(), 0,
        D3D12_TILE_COPY_FLAG_LINEAR_BUFFER_TO_SWIZZLED_TILED_RESOURCE);
    transition(commands, resource.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    commands.list->CopyTiles(resource.Get(), &start, &region, linear.Get(), 0,
        D3D12_TILE_COPY_FLAG_SWIZZLED_TILED_RESOURCE_TO_LINEAR_BUFFER);
    transition(commands, resource.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_RESOLVE_SOURCE);
    commands.list->ResolveSubresource(resolved.Get(), 0, resource.Get(), 0, DXGI_FORMAT_R8G8B8A8_UNORM);
    transition(commands, resolved.Get(), D3D12_RESOURCE_STATE_RESOLVE_DEST, D3D12_RESOURCE_STATE_COPY_SOURCE);
    D3D12_TEXTURE_COPY_LOCATION src = {}; src.pResource = resolved.Get(); src.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
    D3D12_TEXTURE_COPY_LOCATION dst = {}; dst.pResource = readback.Get();
    dst.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT; dst.PlacedFootprint = layout;
    commands.list->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
    commands.submit(queue); queue.complete();
    const D3D12_RANGE samples_range = {0, 2 * tile_bytes};
    check(linear->Map(0, &samples_range, &data), "Map MSAA CopyTiles output"); words = static_cast<uint32_t *>(data);
    bool matches = true;
    for (UINT tile = 0; tile < 2; ++tile) for (UINT y = 0; y < 64; ++y) for (UINT x = 0; x < 64; ++x) for (UINT s = 0; s < 4; ++s)
        matches &= words[tile * tile_words + (y * 64 + x) * 4 + s] == sample_pixel(tile * 64 + x, y, s);
    linear->Unmap(0, &empty); require(matches, "MSAA CopyTiles sample roundtrip mismatch");
    const D3D12_RANGE range = {0, static_cast<SIZE_T>(bytes)};
    check(readback->Map(0, &range, &data), "Map independent resolve witness");
    for (UINT y = 0; y < 64; ++y) {
        const auto *row = reinterpret_cast<const uint32_t *>(static_cast<const unsigned char *>(data) + layout.Offset + UINT64(y) * layout.Footprint.RowPitch);
        for (UINT x = 0; x < 128; ++x) {
            const uint32_t expected = 0xff000000u | (((x + 3 * y) % 32 + 14) << 16) |
                (((3 * x + y) % 32 + 28) << 8) | ((x + y) % 32 + 56);
            // UNORM resolve conversion can round by one; alpha must remain exact.
            matches &= (row[x] >> 24) == 255;
            for (UINT shift : {0u, 8u, 16u})
                matches &= std::abs(int((row[x] >> shift) & 255) - int((expected >> shift) & 255)) <= 1;
        }
    }
    readback->Unmap(0, &empty); require(matches, "MSAA CopyTiles disagrees with independent hardware resolve");
    std::puts("COVERAGE,4x-CopyTiles-both-directions+sample-roundtrip+independent-resolve;per-sample-shader-load=unexercised");
}
static void tiled_format_caps(Context &context) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2);
    UINT checks = 0;
    for (auto format : {DXGI_FORMAT_R8_UINT, DXGI_FORMAT_R16_UINT, DXGI_FORMAT_R32_UINT,
            DXGI_FORMAT_R32G32_UINT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
            DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_D16_UNORM, DXGI_FORMAT_D32_FLOAT}) {
        D3D12_FEATURE_DATA_FORMAT_SUPPORT support = {}; support.Format = format;
        check(context.device->CheckFeatureSupport(D3D12_FEATURE_FORMAT_SUPPORT, &support, sizeof(support)), "tiled format query");
        require((support.Support2 & D3D12_FORMAT_SUPPORT2_TILED) != 0, "required format lacks tiled support"); ++checks;
        for (UINT count : {1u, 2u, 4u, 8u}) {
            D3D12_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS quality = {};
            quality.Format = format; quality.SampleCount = count;
            quality.Flags = D3D12_MULTISAMPLE_QUALITY_LEVELS_FLAG_TILED_RESOURCE;
            check(context.device->CheckFeatureSupport(D3D12_FEATURE_MULTISAMPLE_QUALITY_LEVELS, &quality, sizeof(quality)),
                "tiled sample count query");
            std::printf("CAP,tiled-format=%u,samples=%u,levels=%u\n", format, count, quality.NumQualityLevels);
            require((quality.NumQualityLevels != 0) == (count == 1 || count == 4), "tiled 1x/4x contract mismatch"); ++checks;
        }
    }
    for (auto format : {DXGI_FORMAT_D24_UNORM_S8_UINT, DXGI_FORMAT_D32_FLOAT_S8X24_UINT}) {
        D3D12_FEATURE_DATA_FORMAT_SUPPORT support = {}; support.Format = format;
        check(context.device->CheckFeatureSupport(D3D12_FEATURE_FORMAT_SUPPORT, &support, sizeof(support)), "multi-aspect format query");
        require(!(support.Support2 & D3D12_FORMAT_SUPPORT2_TILED), "unsupported multi-aspect tiled format advertised"); ++checks;
    }
    for (auto format : {DXGI_FORMAT_BC1_UNORM, DXGI_FORMAT_BC3_UNORM, DXGI_FORMAT_R32G32B32A32_FLOAT}) {
        for (UINT count : {1u, 4u}) {
            D3D12_FEATURE_DATA_MULTISAMPLE_QUALITY_LEVELS quality = {};
            quality.Format = format; quality.SampleCount = count;
            quality.Flags = D3D12_MULTISAMPLE_QUALITY_LEVELS_FLAG_TILED_RESOURCE;
            check(context.device->CheckFeatureSupport(D3D12_FEATURE_MULTISAMPLE_QUALITY_LEVELS, &quality, sizeof(quality)),
                "tiled compressed/128-bit sample count query");
            std::printf("CAP,tiled-format=%u,samples=%u,levels=%u\n", format, count, quality.NumQualityLevels);
            require((quality.NumQualityLevels != 0) == (count == 1), "BC/128-bit tiled sample restriction mismatch"); ++checks;
        }
    }
    std::printf("CHECK,tiled-format-caps,checks=%u\n", checks);
    no_debug_errors(context);
}
static void lifetime(Context &context) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2); Queue queue(context.device.Get());
    auto survivor_heap = make_heap(context.device.Get(), 1);
    auto survivor = seed_heap(queue, survivor_heap.Get(), {70});
    for (UINT epoch = 0; epoch < 4; ++epoch) {
        auto heap = make_heap(context.device.Get(), 3);
        auto resource = seed_heap(queue, heap.Get(), {80 + int(epoch), 90 + int(epoch), 100 + int(epoch)});
        map_range(queue, resource.Get(), 1, 1, nullptr, 0, D3D12_TILE_RANGE_FLAG_NULL);
        observe(queue, resource.Get(), {80 + int(epoch), -1, 100 + int(epoch)});
        unmap_all(queue, resource.Get()); queue.complete();
        observe(queue, resource.Get(), {-1, -1, -1});
        // The public API requires the application's backing to remain alive
        // while GPU work uses it. Release only after access and unmap completion.
        resource.Reset(); heap.Reset(); observe(queue, survivor.Get(), {70});
    }
    std::puts("COVERAGE,unmap-zero-read+completion-before-release+allocation-churn+live-neighbor-preservation;early-application-release=not-valid-test");
}
struct HeldGate {
    ComPtr<ID3D12Fence> fence;
    explicit HeldGate(ID3D12Device *device) : fence(make_fence(device)) {}
    // There is no destructor release: signaling alone cannot retire a pending
    // remap. Failure terminates the child before pending owners can unwind.
    void release() { check(fence->Signal(1), "CPU gate release"); }
};
static void mapping_order(Context &context, bool gated) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2);
    auto heap_a = make_heap(context.device.Get(), 1), heap_b = make_heap(context.device.Get(), 1);
    Queue mapping(context.device.Get()), witness(context.device.Get());
    auto old_seed = seed_heap(witness, heap_a.Get(), {110});
    auto new_seed = seed_heap(witness, heap_b.Get(), {210});
    auto resource = reserved(context.device.Get(), buffer_desc(tile_bytes), D3D12_RESOURCE_STATE_COPY_SOURCE);
    map_all(mapping, resource.Get(), heap_a.Get()); mapping.complete(); observe(witness, resource.Get(), {110});
    auto remapped = make_fence(context.device.Get());
    // This gate remains owned until the remap's authenticated completion.
    HeldGate gate(context.device.Get());
    if (gated) check(mapping.queue->Wait(gate.fence.Get(), 1), "queue Wait before remap");
    map_range(mapping, resource.Get(), 0, 1, heap_b.Get(), 0);
    // No command list is submitted to this queue after UpdateTileMappings.
    check(mapping.queue->Signal(remapped.Get(), 1), "mapping-only QueueSignal");
    if (gated) {
        // A deliberate negative observation interval, not a GPU-idle wait or
        // evidence of completion. No event handle is closed while still armed.
        Sleep(100);
        require(remapped->GetCompletedValue() == 0,
            "remap signal completed through unsignaled queue wait");
        // This completed, independent queue performs a real GPU read. A held
        // completion notification on the mapping queue cannot fake old bytes.
        observe(witness, resource.Get(), {110});
        check(context.device->GetDeviceRemovedReason(), "device health during held remap");
        require(remapped->GetCompletedValue() == 0, "remap completed before gate release");
        std::puts("WITNESS,gate-held,independent-queue-GPU-readback-old-data");
        gate.release();
    }
    wait_fence(context.device.Get(), remapped.Get(), 1);
    check(witness.queue->Wait(remapped.Get(), 1), "witness waits for mapping-only completion");
    observe(witness, resource.Get(), {210});
    std::puts("WITNESS,mapping-signal-completed,independent-queue-GPU-readback-new-data");
}
static void invalid_input(Context &context, bool bounds) {
    context.tier(D3D12_TILED_RESOURCES_TIER_2);
    require(context.info != nullptr, "invalid-input case requires native debug info queue");
    auto heap = make_heap(context.device.Get(), 4);
    auto resource = reserved(context.device.Get(), buffer_desc(4 * tile_bytes));
    Queue queue(context.device.Get());
    context.info->ClearStoredMessages();
    const D3D12_TILED_RESOURCE_COORDINATE zero = {}, end = {3, 0, 0, 0};
    const D3D12_TILE_REGION_SIZE two = {2, FALSE, 0, 0, 0};
    std::puts("NEGATIVE,isolated-process-device,native-runtime-validation;DDI-delivery=unproven");
    std::fflush(stdout);
    queue_operation_issued = true;
    if (bounds) {
        queue.queue->CopyTileMappings(resource.Get(), &end, resource.Get(), &zero, &two, D3D12_TILE_MAPPING_FLAG_NONE);
    } else {
        const UINT offset = 0, count = 1;
        queue.queue->UpdateTileMappings(resource.Get(), 1, &zero, &two, heap.Get(), 1, nullptr,
            &offset, &count, D3D12_TILE_MAPPING_FLAG_NONE);
    }
    const std::string operation = bounds ? "copytilemappings" : "updatetilemappings";
    bool error = false;
    const UINT64 count = context.info->GetNumStoredMessagesAllowedByRetrievalFilter();
    require(count < 10000, "unbounded native debug message count");
    for (UINT64 i = 0; i < count; ++i) {
        SIZE_T bytes = 0; check(context.info->GetMessage(i, nullptr, &bytes), "debug message size");
        require(bytes > 0 && bytes <= 1024 * 1024, "invalid debug message size");
        std::vector<unsigned char> storage(bytes);
        auto *message = reinterpret_cast<D3D12_MESSAGE *>(storage.data());
        check(context.info->GetMessage(i, message, &bytes), "debug message");
        std::string description(message->pDescription, message->DescriptionByteLength);
        std::fprintf(stderr, "DEBUG,%u,%u,%s\n", static_cast<UINT>(message->ID), static_cast<UINT>(message->Severity), description.c_str());
        std::transform(description.begin(), description.end(), description.begin(),
            [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
        error |= (message->Severity == D3D12_MESSAGE_SEVERITY_ERROR || message->Severity == D3D12_MESSAGE_SEVERITY_CORRUPTION) &&
            description.find(operation) != std::string::npos;
    }
    require(error, "corresponding native runtime validation error missing");
    std::printf("NEGATIVE_RESULT,native-debug-error,device-reason,%08lx,DDI-delivery=unproven\n",
        static_cast<unsigned long>(context.device->GetDeviceRemovedReason()));
    // Bounds misuse has undefined API behavior beyond the debug error. Do not
    // submit GPU work after it or promote it to driver failure-path conformance.
    // Whether the malformed operation reached the driver is unproven, so even
    // this successful runtime-validation case exits without releasing owners.
    std::printf("PASS,tiled,%s,case-completed\n", selected_case);
    exit_without_unwind(0);
}
int main(int argc, char **argv) {
    if (argc != 3 || std::strcmp(argv[1], "--case")) {
        std::fprintf(stderr, "Usage: d3d12_tiled_probe --case <no-output-msaa|tiled-format-caps|tiling-buffer|tiling-2d|tiling-3d|mappings|copy-mappings|copy-tiles-2d|copy-tiles-predicated|copy-tiles-msaa4x|copy-tiles-depth-msaa4x|committed-depth-copy|committed-depth-copy-immediates|immediate-constants|unmap-lifetime|mapping-signal|wait-remap|invalid-counts|invalid-bounds>\n");
        return 2;
    }
    selected_case = argv[2]; const std::string name(selected_case);
    try {
        native_environment();
        Context context(name == "invalid-counts" || name == "invalid-bounds" || name == "committed-depth-copy" || name == "committed-depth-copy-immediates" || name == "immediate-constants" || name == "copy-tiles-depth-msaa4x" || name == "tiled-format-caps" || name == "no-output-msaa");
        std::printf("BEGIN,tiled,%s\n", selected_case); std::fflush(stdout);
        if (name == "tiling-buffer") tiling_buffer(context);
        else if (name == "no-output-msaa") no_output_msaa(context);
        else if (name == "tiled-format-caps") tiled_format_caps(context);
        else if (name == "tiling-2d" || name == "tiling-3d") tiling_texture(context, name == "tiling-3d");
        else if (name == "mappings") mappings(context);
        else if (name == "copy-mappings") copy_mappings(context);
        else if (name == "copy-tiles-2d") copy_tiles_2d(context);
        else if (name == "copy-tiles-predicated") copy_tiles_predicated(context);
        else if (name == "copy-tiles-msaa4x") copy_tiles_msaa(context);
        else if (name == "copy-tiles-depth-msaa4x") committed_depth_copy(context, false, true);
        else if (name == "committed-depth-copy" || name == "committed-depth-copy-immediates")
            committed_depth_copy(context, name == "committed-depth-copy-immediates");
        else if (name == "immediate-constants") immediate_constants(context);
        else if (name == "unmap-lifetime") lifetime(context);
        else if (name == "mapping-signal" || name == "wait-remap") mapping_order(context, name == "wait-remap");
        else if (name == "invalid-counts" || name == "invalid-bounds") invalid_input(context, name == "invalid-bounds");
        else throw std::runtime_error("unknown case");
        if (name != "invalid-counts" && name != "invalid-bounds")
            check(context.device->GetDeviceRemovedReason(), "final device health");
        std::printf("PASS,tiled,%s,case-completed\n", selected_case);
        return 0;
    } catch (const Blocked &e) {
        std::printf("BLOCKED,tiled,%s,%s\n", selected_case, e.what());
        return 77;
    } catch (const std::exception &e) {
        std::fprintf(stderr, "FAIL,tiled,%s,%s\n", selected_case, e.what());
        return 1;
    }
}
