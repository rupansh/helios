// Native Helios DXR acceptance: no app-local runtime/engine and no cap override.
// Run only through d3d12-raytracing-probe.ps1 in an interactive scheduled task.
// Checks GPU data, cross-queue ordering, local-root shader records, triangles /
// AABBs, update, compaction, cloning, serialization and relocated deserialization.
// Passing these cases does not establish the entire DXR or FL12_1 contract.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <tlhelp32.h>
#include <d3d12.h>
#include <dxgi1_6.h>
#include <wrl/client.h>
#include "d3d12_native_identity.h"
#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <cstdlib>
#include <exception>
#include <filesystem>
#include <fstream>
#include <stdexcept>
#include <string>
#include <vector>

struct AdmissionBlocked : std::runtime_error { using std::runtime_error::runtime_error; };

static void require(bool condition, const char *what)
{
    if (!condition) throw std::runtime_error(what);
}
static void check(HRESULT hr, const char *what)
{
    if (FAILED(hr)) {
        char message[320];
        snprintf(message, sizeof(message), "%s: HRESULT %08lx", what, static_cast<unsigned long>(hr));
        throw std::runtime_error(message);
    }
}
[[noreturn]] static void fail_pending(const char *what, HRESULT hr)
{
    fprintf(stderr, "FAIL,%s: HRESULT %08lx; terminating without unwinding submitted GPU owners\n",
            what, static_cast<unsigned long>(hr));
    fflush(nullptr);
    TerminateProcess(GetCurrentProcess(), 124);
    // TerminateProcess should not return for this process. Preserve the same
    // no-stack-unwinding rule if it fails, rather than throwing through resources.
    std::_Exit(124);
}
static void check_pending(HRESULT hr, const char *what)
{
    if (FAILED(hr)) fail_pending(what, hr);
}

// This probe submits from one CPU thread and observes a fence that dominates
// every submitted queue operation before clearing this bit. Destruction must
// not turn an unrelated allocation/IO exception into early GPU backing release.
static bool gpu_work_may_be_pending = false;
static void guard_pending_unwind()
{
    if (gpu_work_may_be_pending && std::uncaught_exceptions() != 0)
        fail_pending("exception while submitted GPU work may be pending", E_FAIL);
}
template <typename T> class ComPtr : public Microsoft::WRL::ComPtr<T> {
    using Base = Microsoft::WRL::ComPtr<T>;
public:
    using Base::Base;
    using Base::operator=;
    ComPtr() = default;
    ComPtr(const ComPtr &) = default;
    ComPtr(ComPtr &&) = default;
    ComPtr &operator=(const ComPtr &) = default;
    ComPtr &operator=(ComPtr &&) = default;
    ~ComPtr() { guard_pending_unwind(); }
};
using Resource = ComPtr<ID3D12Resource>;
static UINT64 aligned(UINT64 size)
{
    require(size && size <= UINT64_MAX - 255, "invalid AS buffer size");
    return (size + 255) & ~UINT64(255);
}

static void module_line(const wchar_t *name, const wchar_t *path)
{
    char utf8_name[256], utf8_path[4 * MAX_PATH];
    require(WideCharToMultiByte(CP_UTF8, 0, name, -1, utf8_name, sizeof(utf8_name), nullptr, nullptr) != 0 &&
            WideCharToMultiByte(CP_UTF8, 0, path, -1, utf8_path, sizeof(utf8_path), nullptr, nullptr) != 0,
            "module path UTF-8 conversion");
    fprintf(stderr, "MODULE,%s,%s\n", utf8_name, utf8_path);
}

static void native_modules(bool admitted)
{
    wchar_t system[MAX_PATH];
    require(GetSystemDirectoryW(system, MAX_PATH) != 0, "GetSystemDirectory");
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
    require(snapshot != INVALID_HANDLE_VALUE, "module snapshot");
    bool d3d12 = false, core = false, dxgi = false, umd = false, icd = false, valid = true;
    MODULEENTRY32W entry{};
    entry.dwSize = sizeof(entry);
    for (BOOL more = Module32FirstW(snapshot, &entry); more; more = Module32NextW(snapshot, &entry)) {
        bool *required = nullptr;
        if (!_wcsicmp(entry.szModule, L"d3d12.dll")) required = &d3d12;
        if (!_wcsicmp(entry.szModule, L"D3D12Core.dll")) required = &core;
        if (!_wcsicmp(entry.szModule, L"dxgi.dll")) required = &dxgi;
        if (required) {
            *required = true;
            const std::wstring expected = std::wstring(system) + L"\\" + entry.szModule;
            valid &= _wcsicmp(expected.c_str(), entry.szExePath) == 0;
            module_line(entry.szModule, entry.szExePath);
        }
        if (helios_native_umd12_name(entry.szModule)) {
            valid &= !umd;
            umd = true;
            module_line(entry.szModule, entry.szExePath);
        }
        if (!_wcsnicmp(entry.szModule, L"vulkan_virtio", 13)) {
            icd = true;
            module_line(entry.szModule, entry.szExePath);
        }
        if (!_wcsicmp(entry.szModule, L"helios_vkd3d.dll")) valid = false;
    }
    CloseHandle(snapshot);
    fprintf(stderr, "MODULES_PRESENT,d3d12=%u,core=%u,dxgi=%u,umd=%u,icd=%u,admitted=%u\n",
            static_cast<unsigned>(d3d12), static_cast<unsigned>(core), static_cast<unsigned>(dxgi),
            static_cast<unsigned>(umd), static_cast<unsigned>(icd), static_cast<unsigned>(admitted));
    require(valid && d3d12 && dxgi && (!admitted || (core && umd && icd)),
            "native runtime/Helios UMD/ICD module identity");
}

struct Queue {
    ComPtr<ID3D12CommandQueue> queue;
    ComPtr<ID3D12CommandAllocator> allocator;
    ComPtr<ID3D12GraphicsCommandList4> list;

    Queue(ID3D12Device5 *device, D3D12_COMMAND_LIST_TYPE type)
    {
        D3D12_COMMAND_QUEUE_DESC desc{};
        desc.Type = type;
        check(device->CreateCommandQueue(&desc, IID_PPV_ARGS(&queue)), "CreateCommandQueue");
        check(device->CreateCommandAllocator(type, IID_PPV_ARGS(&allocator)), "CreateCommandAllocator");
        check(device->CreateCommandList(0, type, allocator.Get(), nullptr, IID_PPV_ARGS(&list)), "CreateCommandList4");
    }
    void close() { check(list->Close(), "Close"); }
    void execute()
    {
        ID3D12CommandList *lists[] = {list.Get()};
        gpu_work_may_be_pending = true;
        queue->ExecuteCommandLists(1, lists);
    }
    void reset()
    {
        // Caller has observed this queue's exact completion fence first.
        check(allocator->Reset(), "Reset allocator after completion");
        check(list->Reset(allocator.Get(), nullptr), "Reset list");
    }
};

struct Context {
    ComPtr<ID3D12Device5> device;
    ComPtr<ID3D12Fence> fence;
    HANDLE event = nullptr;
    UINT64 value = 0;

    Context()
    {
        DWORD session = 0;
        require(ProcessIdToSessionId(GetCurrentProcessId(), &session) && session != 0, "interactive session required");
        require(!GetEnvironmentVariableW(L"VKD3D_FEATURE_LEVEL", nullptr, 0) &&
                !GetEnvironmentVariableW(L"VKD3D_SHADER_MODEL", nullptr, 0) &&
                !GetEnvironmentVariableW(L"VKD3D_SHADER_OVERRIDE", nullptr, 0), "capability/shader override is forbidden");
        printf("PROCESS,%lu,%lu\n", GetCurrentProcessId(), session);
        ComPtr<IDXGIFactory4> factory;
        check(CreateDXGIFactory2(0, IID_PPV_ARGS(&factory)), "CreateDXGIFactory2");
        ComPtr<IDXGIAdapter1> selected;
        for (UINT i = 0;; ++i) {
            ComPtr<IDXGIAdapter1> adapter;
            HRESULT hr = factory->EnumAdapters1(i, &adapter);
            if (hr == DXGI_ERROR_NOT_FOUND) break;
            check(hr, "EnumAdapters1");
            DXGI_ADAPTER_DESC1 desc{};
            check(adapter->GetDesc1(&desc), "GetDesc1");
            if (desc.VendorId == 0x1af4 && desc.DeviceId == 0x1050 && !(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) {
                require(!selected, "ambiguous Helios adapter");
                selected = adapter;
                printf("ADAPTER,%08lx:%08lx,%04x,%04x\n", desc.AdapterLuid.HighPart,
                       desc.AdapterLuid.LowPart, desc.VendorId, desc.DeviceId);
            }
        }
        require(selected != nullptr, "Helios hardware adapter absent");
        const HRESULT admission = D3D12CreateDevice(selected.Get(), D3D_FEATURE_LEVEL_12_1, IID_PPV_ARGS(&device));
        printf("CAP,NativeFL12_1Admission,%08lx\n", static_cast<unsigned long>(admission));
        native_modules(SUCCEEDED(admission));
        if (admission == DXGI_ERROR_UNSUPPORTED || admission == E_INVALIDARG)
            throw AdmissionBlocked("native FL12_1 admission unavailable");
        check(admission, "native FL12_1 admission");
        D3D12_FEATURE_DATA_D3D12_OPTIONS5 options{};
        check(device->CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS5, &options, sizeof(options)), "OPTIONS5");
        printf("CAP,RaytracingTier,%u\n", static_cast<unsigned>(options.RaytracingTier));
        if (options.RaytracingTier < D3D12_RAYTRACING_TIER_1_0)
            throw AdmissionBlocked("native DXR tier unavailable");
        check(device->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&fence)), "CreateFence");
        event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
        require(event != nullptr, "CreateEvent");
    }
    ~Context() { guard_pending_unwind(); if (event) CloseHandle(event); }
    void wait(UINT64 completion)
    {
        check_pending(fence->SetEventOnCompletion(completion, event), "SetEventOnCompletion after submission");
        const DWORD waited = WaitForSingleObject(event, 30000);
        if (waited != WAIT_OBJECT_0)
            fail_pending("exact GPU completion wait", HRESULT_FROM_WIN32(waited == WAIT_TIMEOUT ? ERROR_TIMEOUT : GetLastError()));
        const UINT64 observed = fence->GetCompletedValue();
        if (observed < completion || observed == UINT64_MAX)
            fail_pending("completion event has no authenticated fence value", E_FAIL);
        check_pending(device->GetDeviceRemovedReason(), "GetDeviceRemovedReason after submission");
        gpu_work_may_be_pending = false;
    }
    void submit(Queue &queue)
    {
        queue.close(); queue.execute();
        check_pending(queue.queue->Signal(fence.Get(), ++value), "Queue Signal after submission");
        wait(value);
    }
    Resource buffer(UINT64 bytes, D3D12_HEAP_TYPE type, D3D12_RESOURCE_STATES state,
                    D3D12_RESOURCE_FLAGS flags = D3D12_RESOURCE_FLAG_NONE)
    {
        D3D12_HEAP_PROPERTIES heap{};
        heap.Type = type; heap.CreationNodeMask = 1; heap.VisibleNodeMask = 1;
        D3D12_RESOURCE_DESC desc{};
        desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
        desc.Width = bytes; desc.Height = 1; desc.DepthOrArraySize = 1; desc.MipLevels = 1;
        desc.SampleDesc.Count = 1; desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR; desc.Flags = flags;
        Resource resource;
        check(device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, nullptr,
                                             IID_PPV_ARGS(&resource)), "CreateCommittedResource");
        return resource;
    }
    Resource as(UINT64 bytes)
    {
        return buffer(aligned(bytes), D3D12_HEAP_TYPE_DEFAULT,
                      D3D12_RESOURCE_STATE_RAYTRACING_ACCELERATION_STRUCTURE, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    }
    Resource upload(const void *data, size_t bytes)
    {
        Resource result = buffer(bytes, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
        write(result.Get(), data, bytes);
        return result;
    }
    static void write(ID3D12Resource *resource, const void *data, size_t bytes)
    {
        void *mapped = nullptr;
        D3D12_RANGE empty{};
        check(resource->Map(0, &empty, &mapped), "Map upload");
        memcpy(mapped, data, bytes);
        D3D12_RANGE written{0, bytes}; resource->Unmap(0, &written);
    }
    static std::vector<uint8_t> read(ID3D12Resource *resource, size_t bytes)
    {
        void *mapped = nullptr;
        D3D12_RANGE range{0, bytes};
        check(resource->Map(0, &range, &mapped), "Map completed readback");
        std::vector<uint8_t> out(bytes);
        memcpy(out.data(), mapped, bytes);
        D3D12_RANGE empty{}; resource->Unmap(0, &empty);
        return out;
    }
};

static void transition(ID3D12GraphicsCommandList4 *list, ID3D12Resource *resource,
                       D3D12_RESOURCE_STATES before, D3D12_RESOURCE_STATES after)
{
    D3D12_RESOURCE_BARRIER barrier{};
    barrier.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    barrier.Transition = {resource, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, before, after};
    list->ResourceBarrier(1, &barrier);
}
static void uav(ID3D12GraphicsCommandList4 *list, ID3D12Resource *resource)
{
    D3D12_RESOURCE_BARRIER barrier{};
    barrier.Type = D3D12_RESOURCE_BARRIER_TYPE_UAV; barrier.UAV.pResource = resource;
    list->ResourceBarrier(1, &barrier);
}
static void readback(ID3D12GraphicsCommandList4 *list, ID3D12Resource *source, ID3D12Resource *dest, UINT64 bytes)
{
    transition(list, source, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
    list->CopyBufferRegion(dest, 0, source, 0, bytes);
    transition(list, source, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
}

struct Pipeline {
    ComPtr<ID3D12RootSignature> global;
    ComPtr<ID3D12StateObject> state;
    ComPtr<ID3D12DescriptorHeap> heap;
    Resource table, output, result;
    D3D12_DISPATCH_RAYS_DESC dispatch{};
    UINT descriptor_size = 0;

    Pipeline(Context &ctx, const std::filesystem::path &path)
    {
        std::ifstream input(path, std::ios::binary | std::ios::ate);
        require(input.good(), "open DXIL library");
        const auto end = input.tellg();
        require(end > 0 && static_cast<uint64_t>(end) <= 64 * 1024 * 1024, "DXIL library extent");
        std::vector<char> dxil(static_cast<size_t>(end));
        input.seekg(0); require(static_cast<bool>(input.read(dxil.data(), static_cast<std::streamsize>(dxil.size()))), "read DXIL library");
        D3D12_DESCRIPTOR_RANGE ranges[2]{};
        ranges[0] = {D3D12_DESCRIPTOR_RANGE_TYPE_UAV, 1, 0, 0, 0};
        ranges[1] = {D3D12_DESCRIPTOR_RANGE_TYPE_SRV, 1, 0, 0, 1};
        D3D12_ROOT_PARAMETER params[2]{};
        params[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE;
        params[0].DescriptorTable = {2, ranges};
        params[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
        params[1].Constants = {0, 0, 1};
        D3D12_ROOT_SIGNATURE_DESC root_desc{2, params, 0, nullptr, D3D12_ROOT_SIGNATURE_FLAG_NONE};
        ComPtr<ID3DBlob> blob, errors;
        check(D3D12SerializeRootSignature(&root_desc, D3D_ROOT_SIGNATURE_VERSION_1, &blob, &errors), "serialize global root");
        check(ctx.device->CreateRootSignature(0, blob->GetBufferPointer(), blob->GetBufferSize(), IID_PPV_ARGS(&global)), "global root");
        D3D12_ROOT_PARAMETER local_param{};
        local_param.ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
        local_param.Constants = {1, 0, 1};
        root_desc = {1, &local_param, 0, nullptr, D3D12_ROOT_SIGNATURE_FLAG_LOCAL_ROOT_SIGNATURE};
        blob.Reset(); errors.Reset();
        check(D3D12SerializeRootSignature(&root_desc, D3D_ROOT_SIGNATURE_VERSION_1, &blob, &errors), "serialize local root");
        ComPtr<ID3D12RootSignature> local;
        check(ctx.device->CreateRootSignature(0, blob->GetBufferPointer(), blob->GetBufferSize(), IID_PPV_ARGS(&local)), "local root");
        D3D12_DXIL_LIBRARY_DESC library{{dxil.data(), dxil.size()}, 0, nullptr};
        D3D12_GLOBAL_ROOT_SIGNATURE global_desc{global.Get()};
        D3D12_LOCAL_ROOT_SIGNATURE local_desc{local.Get()};
        D3D12_RAYTRACING_SHADER_CONFIG shader_config{4, 8};
        D3D12_RAYTRACING_PIPELINE_CONFIG pipeline_config{1};
        D3D12_HIT_GROUP_DESC triangle{L"TriangleHit", D3D12_HIT_GROUP_TYPE_TRIANGLES, nullptr, L"ClosestHit", nullptr};
        D3D12_HIT_GROUP_DESC box{L"BoxHit", D3D12_HIT_GROUP_TYPE_PROCEDURAL_PRIMITIVE, nullptr, L"ClosestHit", L"Intersection"};
        std::array<D3D12_STATE_SUBOBJECT, 8> objects{{
            {D3D12_STATE_SUBOBJECT_TYPE_DXIL_LIBRARY, &library},
            {D3D12_STATE_SUBOBJECT_TYPE_GLOBAL_ROOT_SIGNATURE, &global_desc},
            {D3D12_STATE_SUBOBJECT_TYPE_LOCAL_ROOT_SIGNATURE, &local_desc},
            {D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_SHADER_CONFIG, &shader_config},
            {D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_PIPELINE_CONFIG, &pipeline_config},
            {D3D12_STATE_SUBOBJECT_TYPE_HIT_GROUP, &triangle},
            {D3D12_STATE_SUBOBJECT_TYPE_HIT_GROUP, &box},
            {D3D12_STATE_SUBOBJECT_TYPE_SUBOBJECT_TO_EXPORTS_ASSOCIATION, nullptr},
        }};
        const wchar_t *associated[] = {L"TriangleHit", L"BoxHit", L"Callable"};
        D3D12_SUBOBJECT_TO_EXPORTS_ASSOCIATION association{&objects[2], 3, associated};
        objects[7].pDesc = &association;
        D3D12_STATE_OBJECT_DESC desc{D3D12_STATE_OBJECT_TYPE_COLLECTION, static_cast<UINT>(objects.size()), objects.data()};
        ComPtr<ID3D12StateObject> collection;
        check(ctx.device->CreateStateObject(&desc, IID_PPV_ARGS(&collection)), "complete collection with export associations");
        D3D12_EXISTING_COLLECTION_DESC existing{collection.Get(), 0, nullptr};
        D3D12_STATE_SUBOBJECT sub{D3D12_STATE_SUBOBJECT_TYPE_EXISTING_COLLECTION, &existing};
        desc = {D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE, 1, &sub};
        check(ctx.device->CreateStateObject(&desc, IID_PPV_ARGS(&state)), "pipeline from existing collection");
        collection.Reset(); local.Reset(); // Pipeline must retain both ancestors.
        ComPtr<ID3D12StateObjectProperties> properties;
        check(state.As(&properties), "state properties");
        require(properties->GetShaderIdentifier(L"AbsentExport") == nullptr, "absent export has no identifier");
        UINT64 stack = properties->GetPipelineStackSize();
        require(stack != 0, "pipeline stack size");
        properties->SetPipelineStackSize(stack);
        require(properties->GetPipelineStackSize() == stack, "stack round trip");
        std::array<uint8_t, 320> records{};
        const wchar_t *exports[] = {L"RayGen", L"Miss", L"TriangleHit", L"BoxHit", L"Callable"};
        for (UINT i = 0; i < 5; ++i) {
            void *id = properties->GetShaderIdentifier(exports[i]);
            require(id != nullptr, "shader identifier");
            memcpy(records.data() + i * 64, id, D3D12_SHADER_IDENTIFIER_SIZE_IN_BYTES);
            require(!memcmp(records.data() + i * 64, properties->GetShaderIdentifier(exports[i]), 32), "stable identifier");
        }
        const UINT local_values[3] = {0x111, 0x222, 0x1000};
        for (UINT i = 0; i < 3; ++i) memcpy(records.data() + (i + 2) * 64 + 32, &local_values[i], 4);
        table = ctx.upload(records.data(), records.size());
        UINT64 va = table->GetGPUVirtualAddress();
        dispatch.RayGenerationShaderRecord = {va, 32};
        dispatch.MissShaderTable = {va + 64, 32, 32};
        dispatch.HitGroupTable = {va + 128, 128, 64};
        dispatch.CallableShaderTable = {va + 256, 64, 64};
        dispatch.Width = 4; dispatch.Height = 1; dispatch.Depth = 1;
        output = ctx.buffer(16, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
        result = ctx.buffer(16, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
        D3D12_DESCRIPTOR_HEAP_DESC heap_desc{D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV, 2, D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE, 0};
        check(ctx.device->CreateDescriptorHeap(&heap_desc, IID_PPV_ARGS(&heap)), "descriptor heap");
        descriptor_size = ctx.device->GetDescriptorHandleIncrementSize(heap_desc.Type);
        D3D12_UNORDERED_ACCESS_VIEW_DESC view{};
        view.ViewDimension = D3D12_UAV_DIMENSION_BUFFER;
        view.Buffer.NumElements = 4; view.Buffer.StructureByteStride = 4;
        ctx.device->CreateUnorderedAccessView(output.Get(), nullptr, &view, heap->GetCPUDescriptorHandleForHeapStart());
    }
    void scene(Context &ctx, ID3D12Resource *as)
    {
        D3D12_SHADER_RESOURCE_VIEW_DESC view{};
        view.ViewDimension = D3D12_SRV_DIMENSION_RAYTRACING_ACCELERATION_STRUCTURE;
        view.Shader4ComponentMapping = D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING;
        view.RaytracingAccelerationStructure.Location = as->GetGPUVirtualAddress();
        auto handle = heap->GetCPUDescriptorHandleForHeapStart(); handle.ptr += descriptor_size;
        ctx.device->CreateShaderResourceView(nullptr, &view, handle);
    }
    void bind(ID3D12GraphicsCommandList4 *list, UINT epoch)
    {
        ID3D12DescriptorHeap *heaps[] = {heap.Get()};
        list->SetDescriptorHeaps(1, heaps);
        list->SetComputeRootSignature(global.Get());
        list->SetComputeRootDescriptorTable(0, heap->GetGPUDescriptorHandleForHeapStart());
        list->SetComputeRoot32BitConstant(1, epoch, 0);
        list->SetPipelineState1(state.Get());
    }
    void record(Queue &queue, UINT epoch)
    {
        bind(queue.list.Get(), epoch);
        queue.list->DispatchRays(&dispatch);
        readback(queue.list.Get(), output.Get(), result.Get(), 16);
    }
    void bundle(Context &ctx, Queue &direct, UINT epoch, bool inherit_roots)
    {
        ComPtr<ID3D12CommandAllocator> allocator;
        ComPtr<ID3D12GraphicsCommandList4> commands;
        check(ctx.device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_BUNDLE, IID_PPV_ARGS(&allocator)), "bundle allocator");
        check(ctx.device->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_BUNDLE, allocator.Get(), nullptr,
                                          IID_PPV_ARGS(&commands)), "DXR bundle list");
        direct.reset(); // The previous exact completion has already been observed.
        if (inherit_roots) {
            bind(direct.list.Get(), epoch);
            // Root arguments are inherited; the pipeline state is not. Record
            // both bundle-legal DXR operations in either variant.
            commands->SetPipelineState1(state.Get());
        } else {
            ID3D12DescriptorHeap *heaps[] = {heap.Get()};
            direct.list->SetDescriptorHeaps(1, heaps);
            bind(commands.Get(), epoch); // Bundle heaps must match the parent.
        }
        commands->DispatchRays(&dispatch);
        check(commands->Close(), "Close DXR bundle");
        direct.list->ExecuteBundle(commands.Get());
        // AS operations and readback barriers/copies are not bundle commands.
        readback(direct.list.Get(), output.Get(), result.Get(), 16);
        ctx.submit(direct);
        verify(epoch);
        // Bundle/allocator, RTPSO, roots, descriptors, shader table and AS data
        // all outlive the direct queue's authenticated GPU completion.
    }
    void verify(UINT epoch)
    {
        auto bytes = Context::read(result.Get(), 16);
        const UINT expected[] = {0x1111 + epoch, 0xdead0000 + epoch, 0x1222 + epoch, 0xdead0000 + epoch};
        for (UINT i = 0; i < 4; ++i) {
            UINT value; memcpy(&value, bytes.data() + i * 4, 4);
            printf("PIXEL,%u,%u,%08x,%08x\n", epoch, i, value, expected[i]);
            require(value == expected[i], "DXR hit/miss/callable/local-root GPU readback mismatch");
        }
    }
};

static D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO prebuild(Context &ctx,
        const D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS &inputs)
{
    D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO result{};
    ctx.device->GetRaytracingAccelerationStructurePrebuildInfo(&inputs, &result);
    require(result.ResultDataMaxSizeInBytes && result.ScratchDataSizeInBytes, "nonzero prebuild sizes");
    return result;
}

static void run(const std::filesystem::path &dxil)
{
    Context ctx;
    Queue compute(ctx.device.Get(), D3D12_COMMAND_LIST_TYPE_COMPUTE);
    Queue direct(ctx.device.Get(), D3D12_COMMAND_LIST_TYPE_DIRECT);
    Pipeline pipeline(ctx, dxil);
    const float vertices[] = {-1, -1, 0, 1, -1, 0, 0, 1, 0};
    auto vertex = ctx.upload(vertices, sizeof(vertices));
    const D3D12_RAYTRACING_AABB aabb{-1, -1, 0, 1, 1, 0.5f};
    auto box_input = ctx.upload(&aabb, sizeof(aabb));
    D3D12_RAYTRACING_GEOMETRY_DESC geometry[2]{};
    geometry[0].Type = D3D12_RAYTRACING_GEOMETRY_TYPE_TRIANGLES;
    geometry[0].Flags = D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE;
    geometry[0].Triangles.VertexFormat = DXGI_FORMAT_R32G32B32_FLOAT;
    geometry[0].Triangles.VertexCount = 3;
    geometry[0].Triangles.VertexBuffer = {vertex->GetGPUVirtualAddress(), 12};
    geometry[1].Type = D3D12_RAYTRACING_GEOMETRY_TYPE_PROCEDURAL_PRIMITIVE_AABBS;
    geometry[1].Flags = D3D12_RAYTRACING_GEOMETRY_FLAG_OPAQUE;
    geometry[1].AABBs.AABBCount = 1;
    geometry[1].AABBs.AABBs = {box_input->GetGPUVirtualAddress(), 24};
    const D3D12_RAYTRACING_GEOMETRY_DESC *box_pointer = &geometry[1];
    D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS blas_inputs[2]{};
    D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO sizes[3]{};
    Resource structures[3], scratch[3];
    for (UINT i = 0; i < 2; ++i) {
        auto &input = blas_inputs[i];
        input.Type = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL;
        input.Flags = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_COMPACTION;
        input.NumDescs = 1;
        input.DescsLayout = i ? D3D12_ELEMENTS_LAYOUT_ARRAY_OF_POINTERS : D3D12_ELEMENTS_LAYOUT_ARRAY;
        if (i) input.ppGeometryDescs = &box_pointer; else input.pGeometryDescs = &geometry[0];
        sizes[i] = prebuild(ctx, input);
        structures[i] = ctx.as(sizes[i].ResultDataMaxSizeInBytes);
        scratch[i] = ctx.buffer(aligned(sizes[i].ScratchDataSizeInBytes), D3D12_HEAP_TYPE_DEFAULT,
                               D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    }
    D3D12_RAYTRACING_INSTANCE_DESC instances[2]{};
    for (UINT i = 0; i < 2; ++i) {
        instances[i].Transform[0][0] = instances[i].Transform[1][1] = instances[i].Transform[2][2] = 1;
        instances[i].Transform[0][3] = i ? 3.0f : 0.0f;
        instances[i].InstanceMask = 1; instances[i].InstanceContributionToHitGroupIndex = i;
        instances[i].AccelerationStructure = structures[i]->GetGPUVirtualAddress();
    }
    auto instance_buffer = ctx.upload(instances, sizeof(instances));
    UINT64 instance_pointers[2] = {instance_buffer->GetGPUVirtualAddress(), instance_buffer->GetGPUVirtualAddress() + sizeof(instances[0])};
    auto pointer_buffer = ctx.upload(instance_pointers, sizeof(instance_pointers));
    D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS tlas_inputs{};
    tlas_inputs.Type = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL;
    tlas_inputs.Flags = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_ALLOW_UPDATE;
    tlas_inputs.NumDescs = 2; tlas_inputs.DescsLayout = D3D12_ELEMENTS_LAYOUT_ARRAY_OF_POINTERS;
    tlas_inputs.InstanceDescs = pointer_buffer->GetGPUVirtualAddress();
    sizes[2] = prebuild(ctx, tlas_inputs); structures[2] = ctx.as(sizes[2].ResultDataMaxSizeInBytes);
    scratch[2] = ctx.buffer(aligned((std::max)(sizes[2].ScratchDataSizeInBytes, sizes[2].UpdateScratchDataSizeInBytes)),
                           D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    auto query = ctx.buffer(64, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    auto query_result = ctx.buffer(64, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    for (UINT i = 0; i < 3; ++i) {
        D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC desc{};
        desc.Inputs = i < 2 ? blas_inputs[i] : tlas_inputs;
        desc.DestAccelerationStructureData = structures[i]->GetGPUVirtualAddress();
        desc.ScratchAccelerationStructureData = scratch[i]->GetGPUVirtualAddress();
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC info{query->GetGPUVirtualAddress() + i * 8,
            D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE};
        compute.list->BuildRaytracingAccelerationStructure(&desc, i < 2 ? 1 : 0, i < 2 ? &info : nullptr);
        uav(compute.list.Get(), structures[i].Get());
    }
    UINT64 initial_addresses[3];
    for (UINT i = 0; i < 3; ++i) initial_addresses[i] = structures[i]->GetGPUVirtualAddress();
    D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC current_info{query->GetGPUVirtualAddress() + 32,
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_CURRENT_SIZE};
    compute.list->EmitRaytracingAccelerationStructurePostbuildInfo(&current_info, 3, initial_addresses);
    readback(compute.list.Get(), query.Get(), query_result.Get(), 64);
    pipeline.scene(ctx, structures[2].Get()); pipeline.record(direct, 1);
    compute.close(); direct.close();
    // Enqueue the consumer before the producer and its signal. A queue Wait
    // must not CPU-block or release work before authenticated GPU completion.
    gpu_work_may_be_pending = true;
    check_pending(direct.queue->Wait(ctx.fence.Get(), ++ctx.value), "future cross-queue Wait");
    direct.execute(); compute.execute();
    check_pending(compute.queue->Signal(ctx.fence.Get(), ctx.value), "producer Signal after submission");
    check_pending(direct.queue->Signal(ctx.fence.Get(), ++ctx.value), "consumer Signal after submission");
    ctx.wait(ctx.value); pipeline.verify(1);
    printf("PASS,cross_queue_build_dispatch\n");
    pipeline.bundle(ctx, direct, 4, false);
    printf("PASS,bundle_pipeline_and_dispatch\n");
    pipeline.bundle(ctx, direct, 5, true);
    printf("PASS,bundle_inherited_root_dispatch\n");
    UINT64 compact_size = 0;
    auto query_bytes = Context::read(query_result.Get(), 64);
    for (UINT i = 0; i < 3; ++i) {
        UINT64 current_size = 0;
        memcpy(&current_size, query_bytes.data() + 32 + i * 8, 8);
        printf("SIZE,%u,%llu,%llu\n", i, current_size, sizes[i].ResultDataMaxSizeInBytes);
        require(current_size == sizes[i].ResultDataMaxSizeInBytes, "uncompacted current/prebuild size agreement");
    }
    memcpy(&compact_size, query_bytes.data(), 8);
    require(compact_size && compact_size <= sizes[0].ResultDataMaxSizeInBytes, "compaction size bound");
    auto compact = ctx.as(compact_size), clone = ctx.as(sizes[1].ResultDataMaxSizeInBytes);
    compute.reset();
    compute.list->CopyRaytracingAccelerationStructure(compact->GetGPUVirtualAddress(), structures[0]->GetGPUVirtualAddress(),
                                                     D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT);
    compute.list->CopyRaytracingAccelerationStructure(clone->GetGPUVirtualAddress(), structures[1]->GetGPUVirtualAddress(),
                                                     D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_CLONE);
    ctx.submit(compute);
    structures[0] = compact; structures[1] = clone;
    for (UINT i = 0; i < 2; ++i) instances[i].AccelerationStructure = structures[i]->GetGPUVirtualAddress();
    Context::write(instance_buffer.Get(), instances, sizeof(instances));
    compute.reset();
    D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC update{};
    update.Inputs = tlas_inputs;
    update.Inputs.Flags |= D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAG_PERFORM_UPDATE;
    update.SourceAccelerationStructureData = update.DestAccelerationStructureData = structures[2]->GetGPUVirtualAddress();
    update.ScratchAccelerationStructureData = scratch[2]->GetGPUVirtualAddress();
    compute.list->BuildRaytracingAccelerationStructure(&update, 0, nullptr);
    uav(compute.list.Get(), structures[2].Get()); ctx.submit(compute);
    direct.reset(); pipeline.record(direct, 2); ctx.submit(direct); pipeline.verify(2);
    printf("PASS,compact_clone_update_source_lifetime\n");

    // Serialized pointer-list size is queried independently from the serialized
    // header. Both must agree, and relocated structures must render identically.
    compute.reset();
    UINT64 addresses[3];
    for (UINT i = 0; i < 3; ++i) addresses[i] = structures[i]->GetGPUVirtualAddress();
    D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC serial_info{query->GetGPUVirtualAddress(),
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_SERIALIZATION};
    compute.list->EmitRaytracingAccelerationStructurePostbuildInfo(&serial_info, 3, addresses);
    readback(compute.list.Get(), query.Get(), query_result.Get(), 64); ctx.submit(compute);
    query_bytes = Context::read(query_result.Get(), 64);
    Resource serialized[3], serialized_results[3], restored[3];
    std::vector<uint8_t> blobs[3];
    D3D12_SERIALIZED_RAYTRACING_ACCELERATION_STRUCTURE_HEADER headers[3]{};
    D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_SERIALIZATION_DESC infos[3]{};
    compute.reset();
    for (UINT i = 0; i < 3; ++i) {
        memcpy(&infos[i], query_bytes.data() + i * sizeof(infos[i]), sizeof(infos[i]));
        require(infos[i].SerializedSizeInBytes >= sizeof(headers[i]) && infos[i].SerializedSizeInBytes < 64 * 1024 * 1024,
                "serialized size bound");
        serialized[i] = ctx.buffer(aligned(infos[i].SerializedSizeInBytes), D3D12_HEAP_TYPE_DEFAULT,
                                   D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
        serialized_results[i] = ctx.buffer(aligned(infos[i].SerializedSizeInBytes), D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
        compute.list->CopyRaytracingAccelerationStructure(serialized[i]->GetGPUVirtualAddress(), addresses[i],
                                                        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_SERIALIZE);
        readback(compute.list.Get(), serialized[i].Get(), serialized_results[i].Get(), infos[i].SerializedSizeInBytes);
        transition(compute.list.Get(), serialized[i].Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
    }
    ctx.submit(compute);
    for (UINT i = 0; i < 3; ++i) {
        blobs[i] = Context::read(serialized_results[i].Get(), static_cast<size_t>(infos[i].SerializedSizeInBytes));
        memcpy(&headers[i], blobs[i].data(), sizeof(headers[i]));
        require(headers[i].SerializedSizeInBytesIncludingHeader == infos[i].SerializedSizeInBytes, "serialized size header/query agreement");
        require(headers[i].DeserializedSizeInBytes && headers[i].DeserializedSizeInBytes <= structures[i]->GetDesc().Width,
                "deserialized size bound");
        require(headers[i].NumBottomLevelAccelerationStructurePointersAfterHeader == infos[i].NumBottomLevelAccelerationStructurePointers,
                "serialized BLAS pointer header/query agreement");
        require(i == 2 || infos[i].NumBottomLevelAccelerationStructurePointers == 0, "ordinary BLAS has no pointer postamble");
        require(ctx.device->CheckDriverMatchingIdentifier(D3D12_SERIALIZED_DATA_RAYTRACING_ACCELERATION_STRUCTURE,
                                                         &headers[i].DriverMatchingIdentifier) == D3D12_DRIVER_MATCHING_IDENTIFIER_COMPATIBLE_WITH_DEVICE,
                "serialized producer identifier compatibility");
        restored[i] = ctx.as(headers[i].DeserializedSizeInBytes);
        std::ofstream out("serialized-" + std::to_string(i) + ".bin", std::ios::binary);
        require(static_cast<bool>(out.write(reinterpret_cast<const char *>(blobs[i].data()), static_cast<std::streamsize>(blobs[i].size()))), "archive serialization");
    }
    auto foreign = headers[0].DriverMatchingIdentifier; foreign.DriverOpaqueGUID.Data1 ^= 1;
    require(ctx.device->CheckDriverMatchingIdentifier(D3D12_SERIALIZED_DATA_RAYTRACING_ACCELERATION_STRUCTURE, &foreign) !=
            D3D12_DRIVER_MATCHING_IDENTIFIER_COMPATIBLE_WITH_DEVICE, "foreign serialized producer rejected");
    const UINT64 pointer_count = headers[2].NumBottomLevelAccelerationStructurePointersAfterHeader;
    require(pointer_count <= (blobs[2].size() - sizeof(headers[2])) / sizeof(UINT64), "serialized pointer extent");
    bool relocated[2]{};
    for (UINT64 i = 0; i < pointer_count; ++i) {
        size_t offset = sizeof(headers[2]) + static_cast<size_t>(i) * 8;
        UINT64 pointer; memcpy(&pointer, blobs[2].data() + offset, 8);
        if (!pointer) continue;
        UINT which = pointer == addresses[0] ? 0 : pointer == addresses[1] ? 1 : 2;
        require(which < 2, "serialized pointer belongs to original BLAS");
        pointer = restored[which]->GetGPUVirtualAddress(); relocated[which] = true;
        memcpy(blobs[2].data() + offset, &pointer, 8);
    }
    require(relocated[0] && relocated[1], "both BLAS references relocated");
    auto relocated_upload = ctx.upload(blobs[2].data(), blobs[2].size());
    compute.reset();
    transition(compute.list.Get(), serialized[2].Get(), D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_COPY_DEST);
    compute.list->CopyBufferRegion(serialized[2].Get(), 0, relocated_upload.Get(), 0, blobs[2].size());
    transition(compute.list.Get(), serialized[2].Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
    // Complete the TLAS restore before recording either BLAS restore. Recording
    // all three in one list would already create the future Vulkan BLAS views
    // and would miss the cross-submission reference-lifetime obligation.
    compute.list->CopyRaytracingAccelerationStructure(restored[2]->GetGPUVirtualAddress(), serialized[2]->GetGPUVirtualAddress(),
                                                    D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_DESERIALIZE);
    uav(compute.list.Get(), restored[2].Get());
    ctx.submit(compute);
    printf("PASS,tlas_restore_completed_before_blas_recording\n");
    compute.reset();
    UINT64 restored_tlas = restored[2]->GetGPUVirtualAddress();
    // The source type came from GPU deserialization. Query before any BLAS
    // restoration is recorded, then verify after exact queue completion.
    compute.list->EmitRaytracingAccelerationStructurePostbuildInfo(&serial_info, 1, &restored_tlas);
    readback(compute.list.Get(), query.Get(), query_result.Get(), 16);
    ctx.submit(compute);
    auto restored_query_bytes = Context::read(query_result.Get(), 16);
    D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_SERIALIZATION_DESC restored_info{};
    memcpy(&restored_info, restored_query_bytes.data(), sizeof(restored_info));
    require(restored_info.SerializedSizeInBytes >= sizeof(headers[2]) &&
            restored_info.NumBottomLevelAccelerationStructurePointers == pointer_count,
            "TLAS serialization query after GPU restore and before BLAS contents");
    printf("PASS,serialization_query_after_tlas_restore\n");
    compute.reset();
    for (UINT i = 0; i < 2; ++i) {
        compute.list->CopyRaytracingAccelerationStructure(restored[i]->GetGPUVirtualAddress(), serialized[i]->GetGPUVirtualAddress(),
                                                        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_DESERIALIZE);
        uav(compute.list.Get(), restored[i].Get());
    }
    ctx.submit(compute);
    for (auto &structure : structures) structure.Reset();
    compact.Reset(); clone.Reset();
    pipeline.scene(ctx, restored[2].Get());
    direct.reset(); pipeline.record(direct, 3); ctx.submit(direct); pipeline.verify(3);
    printf("PASS,serialize_relocate_tlas_first_deserialize_lifetime\n");
    printf("PASS,native_dxr_probe_completed\n");
}

int wmain(int argc, wchar_t **argv)
{
    try {
        require(argc == 2, "usage: d3d12_raytracing_probe.exe <library.dxil>");
        run(argv[1]);
        return 0;
    } catch (const AdmissionBlocked &error) {
        fprintf(stderr, "BLOCKED77,%s\n", error.what());
        return 77;
    } catch (const std::exception &error) {
        fprintf(stderr, "FAIL,%s\n", error.what());
        return 1;
    }
}
