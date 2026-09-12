// Native Windows D3D12 / Helios stream-output acceptance. Run interactively
// using d3d12-stream-output-probe.ps1 after the integrated driver review.
#include <windows.h>
#include <d3d12.h>
#include <dxgi1_4.h>
#include <tlhelp32.h>
#include <wrl/client.h>
#include "d3d12_native_identity.h"
#include <array>
#include <cstdlib>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <stdexcept>
#include <string>
#include <vector>
using Microsoft::WRL::ComPtr;

static void check(HRESULT hr, const char *operation) {
    if (FAILED(hr)) {
        std::fprintf(stderr, "FAIL,%s,hr=%08lx\n", operation, static_cast<unsigned long>(hr));
        throw std::runtime_error(operation);
    }
}
static void require(bool ok, const char *message) {
    if (!ok) throw std::runtime_error(message);
}
[[noreturn]] static void pending_submission_failure(const char *operation, HRESULT hr) noexcept {
    std::fprintf(stderr, "FAIL,pending-submission,%s,hr=%08lx,terminating-without-unwind\n",
        operation, static_cast<unsigned long>(hr));
    std::fflush(stdout); std::fflush(stderr);
    // No completion proof exists. Normal exception unwinding would release
    // run_case's submitted buffers/PSO before the GPU has finished using them.
    // Process termination delegates teardown to the driver; it is not a GPU
    // completion witness and never turns this run into a pass.
    TerminateProcess(GetCurrentProcess(), 2);
    std::_Exit(2);
}
static std::vector<char> read(const wchar_t *file) {
    std::ifstream stream(file, std::ios::binary | std::ios::ate);
    require(static_cast<bool>(stream), "shader file missing");
    const auto length = stream.tellg();
    require(length > 0 && length < 4 * 1024 * 1024, "invalid shader extent");
    std::vector<char> bytes(static_cast<size_t>(length));
    stream.seekg(0);
    stream.read(bytes.data(), static_cast<std::streamsize>(length));
    require(static_cast<bool>(stream), "shader read failed");
    return bytes;
}
static void native_modules() {
    wchar_t system[MAX_PATH] = {};
    require(GetSystemDirectoryW(system, MAX_PATH) != 0, "System directory unavailable");
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
    require(snapshot != INVALID_HANDLE_VALUE, "module snapshot failed");
    MODULEENTRY32W module = {}; module.dwSize = sizeof(module);
    bool d3d = false, core = false, dxgi = false, icd = false;
    unsigned umd_count = 0;
    bool valid = true;
    for (BOOL next = Module32FirstW(snapshot, &module); next; next = Module32NextW(snapshot, &module)) {
        const std::wstring name(module.szModule);
        bool *native = !_wcsicmp(name.c_str(), L"d3d12.dll") ? &d3d :
            !_wcsicmp(name.c_str(), L"D3D12Core.dll") ? &core :
            !_wcsicmp(name.c_str(), L"dxgi.dll") ? &dxgi : nullptr;
        const bool is_umd = helios_native_umd12_name(name.c_str());
        const bool is_icd = !_wcsnicmp(name.c_str(), L"vulkan_virtio", 13);
        if (native || is_umd || is_icd) {
            std::fwprintf(stderr, L"MODULE,%ls,%ls\n", name.c_str(), module.szExePath);
            if (native) {
                *native = true;
                const std::wstring expected = std::wstring(system) + L"\\" + name;
                valid = valid && !_wcsicmp(expected.c_str(), module.szExePath);
            }
            if (is_umd) ++umd_count;
            icd |= is_icd;
        }
        valid = valid && _wcsicmp(name.c_str(), L"helios_vkd3d.dll") != 0;
    }
    CloseHandle(snapshot);
    require(valid && d3d && core && dxgi && umd_count == 1 && icd, "native module identity check failed");
}
struct Context {
    ComPtr<ID3D12Device> device;
    ComPtr<ID3D12CommandQueue> queue;
    ComPtr<ID3D12CommandAllocator> allocator;
    ComPtr<ID3D12GraphicsCommandList> list;
    ComPtr<ID3D12Fence> fence;
    ComPtr<ID3D12RootSignature> root;
    UINT64 next = 0;
    HANDLE event = nullptr;
    ~Context() { if (event) CloseHandle(event); }
    void submit() {
        check(list->Close(), "Close");
        // This guard leaves scope before any caller-owned submitted resources.
        // It also catches an unexpected C++ exception in a COM implementation,
        // rather than relying only on the documented HRESULT failure paths.
        struct PendingSubmission {
            bool pending = true;
            ~PendingSubmission() noexcept {
                if (pending) pending_submission_failure("exception before completion", E_UNEXPECTED);
            }
        } submission;
        ID3D12CommandList *lists[] = {list.Get()}; queue->ExecuteCommandLists(1, lists);
        HRESULT hr = queue->Signal(fence.Get(), ++next);
        if (FAILED(hr)) pending_submission_failure("Signal", hr);
        hr = fence->SetEventOnCompletion(next, event);
        if (FAILED(hr)) pending_submission_failure("SetEventOnCompletion", hr);
        const DWORD wait = WaitForSingleObject(event, 60000);
        if (wait != WAIT_OBJECT_0) pending_submission_failure("completion wait",
            HRESULT_FROM_WIN32(wait == WAIT_FAILED ? GetLastError() : ERROR_TIMEOUT));
        const UINT64 completed = fence->GetCompletedValue();
        if (completed == UINT64_MAX || completed < next)
            pending_submission_failure("unauthenticated fence completion", DXGI_ERROR_DEVICE_REMOVED);
        hr = device->GetDeviceRemovedReason();
        if (FAILED(hr)) pending_submission_failure("GetDeviceRemovedReason", hr);
        submission.pending = false;
    }
    void reset() {
        check(allocator->Reset(), "Allocator Reset");
        check(list->Reset(allocator.Get(), nullptr), "List Reset");
    }
    ComPtr<ID3D12Resource> buffer(UINT64 bytes, D3D12_HEAP_TYPE type, D3D12_RESOURCE_STATES state) {
        D3D12_HEAP_PROPERTIES heap = {}; heap.Type = type; heap.CreationNodeMask = heap.VisibleNodeMask = 1;
        D3D12_RESOURCE_DESC desc = {}; desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
        desc.Width = bytes; desc.Height = 1; desc.DepthOrArraySize = 1; desc.MipLevels = 1;
        desc.SampleDesc.Count = 1; desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
        ComPtr<ID3D12Resource> resource;
        check(device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state,
            nullptr, IID_PPV_ARGS(&resource)), "Create buffer");
        return resource;
    }
};
static void barrier(ID3D12GraphicsCommandList *list, ID3D12Resource *resource,
                    D3D12_RESOURCE_STATES before, D3D12_RESOURCE_STATES after) {
    D3D12_RESOURCE_BARRIER b = {}; b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    b.Transition = {resource, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, before, after};
    list->ResourceBarrier(1, &b);
}
static D3D12_ROOT_PARAMETER draw_tag_parameter() {
    D3D12_ROOT_PARAMETER parameter = {};
    parameter.ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
    parameter.Constants = {0, 0, 1};
    parameter.ShaderVisibility = D3D12_SHADER_VISIBILITY_VERTEX;
    return parameter;
}
static void initialize(Context &c) {
    ComPtr<IDXGIFactory4> factory;
    check(CreateDXGIFactory2(0, IID_PPV_ARGS(&factory)), "CreateDXGIFactory2");
    for (UINT i = 0; ; ++i) {
        ComPtr<IDXGIAdapter1> adapter;
        const auto hr = factory->EnumAdapters1(i, &adapter);
        if (hr == DXGI_ERROR_NOT_FOUND) break;
        check(hr, "EnumAdapters1");
        DXGI_ADAPTER_DESC1 desc; check(adapter->GetDesc1(&desc), "GetDesc1");
        if (desc.VendorId != 0x1af4 || desc.DeviceId != 0x1050 ||
            desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) continue;
        check(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&c.device)), "D3D12CreateDevice Helios");
        std::printf("ADAPTER,%04x,%04x,%08lx:%08lx\n", desc.VendorId, desc.DeviceId,
            static_cast<unsigned long>(desc.AdapterLuid.HighPart), desc.AdapterLuid.LowPart);
        break;
    }
    require(c.device != nullptr, "Helios adapter not found"); native_modules();
    D3D12_COMMAND_QUEUE_DESC desc = {};
    check(c.device->CreateCommandQueue(&desc, IID_PPV_ARGS(&c.queue)), "CreateCommandQueue");
    check(c.device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT, IID_PPV_ARGS(&c.allocator)), "CreateCommandAllocator");
    check(c.device->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, c.allocator.Get(), nullptr,
        IID_PPV_ARGS(&c.list)), "CreateCommandList");
    check(c.device->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&c.fence)), "CreateFence");
    c.event = CreateEventW(nullptr, FALSE, FALSE, nullptr); require(c.event != nullptr, "CreateEvent");
    const auto parameter = draw_tag_parameter();
    D3D12_ROOT_SIGNATURE_DESC root = {}; root.Flags = D3D12_ROOT_SIGNATURE_FLAG_ALLOW_STREAM_OUTPUT;
    root.NumParameters = 1; root.pParameters = &parameter;
    ComPtr<ID3DBlob> blob, errors;
    check(D3D12SerializeRootSignature(&root, D3D_ROOT_SIGNATURE_VERSION_1, &blob, &errors), "SerializeRootSignature");
    check(c.device->CreateRootSignature(0, blob->GetBufferPointer(), blob->GetBufferSize(), IID_PPV_ARGS(&c.root)), "CreateRootSignature");
}
static D3D12_GRAPHICS_PIPELINE_STATE_DESC so_pipeline_desc(Context &c, const std::vector<char> &vs) {
    D3D12_GRAPHICS_PIPELINE_STATE_DESC desc = {};
    desc.pRootSignature = c.root.Get(); desc.VS = {vs.data(), vs.size()};
    desc.SampleMask = UINT_MAX; desc.SampleDesc.Count = 1;
    desc.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID;
    desc.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
    desc.RasterizerState.DepthClipEnable = TRUE;
    desc.DepthStencilState.DepthFunc = D3D12_COMPARISON_FUNC_ALWAYS;
    desc.DepthStencilState.StencilReadMask = desc.DepthStencilState.StencilWriteMask = 0xff;
    desc.DepthStencilState.FrontFace = {D3D12_STENCIL_OP_KEEP, D3D12_STENCIL_OP_KEEP,
        D3D12_STENCIL_OP_KEEP, D3D12_COMPARISON_FUNC_ALWAYS};
    desc.DepthStencilState.BackFace = desc.DepthStencilState.FrontFace;
    for (auto &blend : desc.BlendState.RenderTarget) {
        blend.SrcBlend = blend.SrcBlendAlpha = D3D12_BLEND_ONE;
        blend.DestBlend = blend.DestBlendAlpha = D3D12_BLEND_ZERO;
        blend.BlendOp = blend.BlendOpAlpha = D3D12_BLEND_OP_ADD;
        blend.LogicOp = D3D12_LOGIC_OP_NOOP; blend.RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL;
    }
    desc.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_POINT;
    return desc;
}
static void run_case(Context &c, const std::vector<char> &vs, const std::vector<char> &gs,
                     const std::vector<char> &ps, bool multiple_streams, bool overflow, unsigned iteration,
                     bool rasterized = false) {
    // Declaration and stride arrays die immediately after PSO creation. Later
    // execution must retain the engine's own copy, including valid null gaps.
    ComPtr<ID3D12PipelineState> pso;
    const UINT stride0 = multiple_streams ? 16 : 32;
    const UINT capacity = overflow ? 2 : 5;
    const bool null_unbind_witness = !multiple_streams && !overflow && !rasterized;
    const UINT view_capacity = null_unbind_witness ? 6 : capacity;
    {
        const std::array<D3D12_SO_DECLARATION_ENTRY, 5> declarations = {{
            {0, "PACKED", 0, 1, 1, 0}, {0, nullptr, 0, 0, 1, 0},
            {0, "PACKED", 1, 0, 2, 0},
            {multiple_streams ? 1u : 0u, multiple_streams ? "PACKED" : "SV_Position", 0, 0,
                static_cast<BYTE>(multiple_streams ? 2 : 4), 1},
            {1, "PACKED", 1, 0, 2, 1},
        }};
        const UINT strides[] = {stride0, 16};
        auto desc = so_pipeline_desc(c, vs);
        if (multiple_streams) desc.GS = {gs.data(), gs.size()};
        desc.StreamOutput = {declarations.data(), multiple_streams ? 5u : 4u, strides, 2, rasterized ? 0u : D3D12_SO_NO_RASTERIZED_STREAM};
        if (rasterized) { desc.PS = {ps.data(), ps.size()}; desc.NumRenderTargets = 1; desc.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM; }
        check(c.device->CreateGraphicsPipelineState(&desc, IID_PPV_ARGS(&pso)), "CreateGraphicsPipelineState SO");
        if (!iteration && !multiple_streams && !overflow && !rasterized) {
            // Keep shader/root compatibility identical; only the SO permission
            // differs. Otherwise a missing b0 could falsely pass this test.
            const auto parameter = draw_tag_parameter();
            D3D12_ROOT_SIGNATURE_DESC empty_root = {};
            empty_root.NumParameters = 1; empty_root.pParameters = &parameter;
            ComPtr<ID3DBlob> blob, errors;
            check(D3D12SerializeRootSignature(&empty_root, D3D_ROOT_SIGNATURE_VERSION_1, &blob, &errors), "Serialize forbidden SO root");
            ComPtr<ID3D12RootSignature> no_so;
            check(c.device->CreateRootSignature(0, blob->GetBufferPointer(), blob->GetBufferSize(),
                IID_PPV_ARGS(&no_so)), "Create forbidden SO root");
            desc.pRootSignature = no_so.Get();
            ComPtr<ID3D12PipelineState> refused;
            const HRESULT hr = c.device->CreateGraphicsPipelineState(&desc, IID_PPV_ARGS(&refused));
            require(hr == E_INVALIDARG && !refused, "missing ALLOW_STREAM_OUTPUT was accepted");
            check(c.device->GetDeviceRemovedReason(), "negative PSO path removed device");
            std::printf("PASS,negative-root-allow-stream-output,hr=%08lx\n", static_cast<unsigned long>(hr));
        }
    }
    auto upload = c.buffer(1024, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    void *mapped = nullptr; D3D12_RANGE empty = {0, 0};
    check(upload->Map(0, &empty, &mapped), "Map upload");
    std::memset(mapped, 0xcd, 1024);
    static_cast<uint32_t *>(mapped)[0] = static_cast<uint32_t *>(mapped)[2] = 0;
    upload->Unmap(0, nullptr);
    auto first = c.buffer(256, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    auto second = c.buffer(256, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    auto counters = c.buffer(16, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    auto result = c.buffer(2048, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    ComPtr<ID3D12Resource> target;
    ComPtr<ID3D12DescriptorHeap> target_heap;
    if (rasterized) {
        D3D12_RESOURCE_DESC desc = {}; desc.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
        desc.Width = desc.Height = desc.DepthOrArraySize = desc.MipLevels = 1;
        desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM; desc.SampleDesc.Count = 1;
        desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
        D3D12_HEAP_PROPERTIES heap = {}; heap.Type = D3D12_HEAP_TYPE_DEFAULT;
        heap.CreationNodeMask = heap.VisibleNodeMask = 1;
        check(c.device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc,
            D3D12_RESOURCE_STATE_RENDER_TARGET, nullptr, IID_PPV_ARGS(&target)), "Create raster target");
        D3D12_DESCRIPTOR_HEAP_DESC hd = {}; hd.Type = D3D12_DESCRIPTOR_HEAP_TYPE_RTV; hd.NumDescriptors = 1;
        check(c.device->CreateDescriptorHeap(&hd, IID_PPV_ARGS(&target_heap)), "Create RTV heap");
        const auto rtv = target_heap->GetCPUDescriptorHandleForHeapStart();
        c.device->CreateRenderTargetView(target.Get(), nullptr, rtv);
        const float black[4] = {};
        c.list->ClearRenderTargetView(rtv, black, 0, nullptr);
        c.list->OMSetRenderTargets(1, &rtv, TRUE, nullptr);
    }
    const D3D12_VIEWPORT viewport = {0, 0, 1, 1, 0, 1}; const D3D12_RECT scissor = {0, 0, 1, 1};
    c.list->RSSetViewports(1, &viewport); c.list->RSSetScissorRects(1, &scissor);
    ComPtr<ID3D12QueryHeap> queries;
    D3D12_QUERY_HEAP_DESC qdesc = {D3D12_QUERY_HEAP_TYPE_SO_STATISTICS, 3, 0};
    check(c.device->CreateQueryHeap(&qdesc, IID_PPV_ARGS(&queries)), "Create SO query heap");
    c.list->CopyBufferRegion(first.Get(), 0, upload.Get(), 16, 256);
    c.list->CopyBufferRegion(second.Get(), 0, upload.Get(), 16, 256);
    c.list->CopyBufferRegion(counters.Get(), 0, upload.Get(), 0, 16);
    for (auto *resource : {first.Get(), second.Get(), counters.Get()})
        barrier(c.list.Get(), resource, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_STREAM_OUT);
    D3D12_STREAM_OUTPUT_BUFFER_VIEW views[] = {
        {first->GetGPUVirtualAddress(), UINT64(view_capacity) * stride0, counters->GetGPUVirtualAddress()},
        {second->GetGPUVirtualAddress(), UINT64(view_capacity) * 16, counters->GetGPUVirtualAddress() + 8},
    };
    c.list->SetPipelineState(pso.Get()); c.list->SetGraphicsRootSignature(c.root.Get());
    c.list->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_POINTLIST);
    c.list->SOSetTargets(0, 2, views);
    c.list->BeginQuery(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0, 0);
    if (multiple_streams) c.list->BeginQuery(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM1, 1);
    // HLSL Extended Command Information specifies zero-based SV_VertexID,
    // excluding StartVertexLocation. Keep nonzero starts and independent tags:
    // adding the start twice or losing a tag must fail the same readback.
    c.list->SetGraphicsRoot32BitConstant(0, iteration * 10, 0);
    c.list->DrawInstanced(5, 1, iteration * 10, 0);
    c.list->EndQuery(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0, 0);
    if (multiple_streams) c.list->EndQuery(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM1, 1);
    if (null_unbind_witness) {
        // Both former views have capacity. The declaration still writes slot
        // 0, so unbinding it acts as a full buffer and stops this whole stream.
        // Storage-needed must advance even though neither counter appends.
        c.list->SOSetTargets(0, 1, nullptr);
        c.list->BeginQuery(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0, 2);
        c.list->SetGraphicsRoot32BitConstant(0, 99, 0);
        c.list->DrawInstanced(1, 1, 99, 0);
        c.list->EndQuery(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0, 2);
    }
    D3D12_STREAM_OUTPUT_BUFFER_VIEW unbound[2] = {};
    c.list->SOSetTargets(0, 2, unbound);
    c.list->ResolveQueryData(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0, 0, 1, result.Get(), 544);
    if (multiple_streams) c.list->ResolveQueryData(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM1, 1, 1, result.Get(), 560);
    if (null_unbind_witness) c.list->ResolveQueryData(queries.Get(), D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0, 2, 1, result.Get(), 576);
    for (auto *resource : {first.Get(), second.Get(), counters.Get()})
        barrier(c.list.Get(), resource, D3D12_RESOURCE_STATE_STREAM_OUT, D3D12_RESOURCE_STATE_COPY_SOURCE);
    c.list->CopyBufferRegion(result.Get(), 0, first.Get(), 0, 256);
    c.list->CopyBufferRegion(result.Get(), 256, second.Get(), 0, 256);
    c.list->CopyBufferRegion(result.Get(), 512, counters.Get(), 0, 16);
    if (rasterized) {
        barrier(c.list.Get(), target.Get(), D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_SOURCE);
        D3D12_TEXTURE_COPY_LOCATION source = {}; source.pResource = target.Get();
        source.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
        D3D12_TEXTURE_COPY_LOCATION destination = {}; destination.pResource = result.Get();
        destination.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT;
        destination.PlacedFootprint = {1024, {DXGI_FORMAT_R8G8B8A8_UNORM, 1, 1, 1, 256}};
        c.list->CopyTextureRegion(&destination, 0, 0, 0, &source, nullptr);
    }
    c.submit();
    D3D12_RANGE range = {0, 1280}; check(result->Map(0, &range, &mapped), "Map GPU readback");
    const auto *words = static_cast<const uint32_t *>(mapped);
    const auto *values = static_cast<const float *>(mapped);
    require(words[128] == capacity * stride0, "first filled size");
    require(words[130] == capacity * 16, "second filled size / NULL unbind witness");
    require(words[129] == 0xcdcdcdcd && words[131] == 0xcdcdcdcd, "32-bit counter guards overwritten");
    for (UINT vertex = 0; vertex < capacity; ++vertex) {
        const UINT id = vertex + iteration * 10, off = vertex * (stride0 / 4);
        if (values[off] != float(20 + id) || values[off + 2] != float(30 + id) ||
            values[off + 3] != float(40 + id)) {
            std::fprintf(stderr, "FAIL,so-payload,streams=%u,overflow=%u,rasterized=%u,iteration=%u,vertex=%u,"
                "actual=%08x:%08x:%08x:%08x,expected=%u:gap:%u:%u\n",
                multiple_streams ? 2u : 1u, overflow ? 1u : 0u, rasterized ? 1u : 0u,
                iteration, vertex, words[off], words[off + 1], words[off + 2], words[off + 3],
                20 + id, 30 + id, 40 + id);
            require(false, "packed/partial SO readback");
        }
        require(words[off + 1] == 0xcdcdcdcd, "SO gap overwritten");
        if (stride0 == 32) for (UINT pad = 4; pad < 8; ++pad)
            require(words[off + pad] == 0xcdcdcdcd, "stride padding overwritten");
        const UINT second_off = 64 + vertex * 4;
        if (multiple_streams) {
            for (UINT component = 0; component < 4; ++component)
                require(values[second_off + component] == float(110 + component * 10 + id), "stream 1 readback");
        } else require(values[second_off] == 0 && values[second_off + 1] == 0 &&
            values[second_off + 2] == 0.5f && values[second_off + 3] == 1, "VS passthrough position");
    }
    if (null_unbind_witness) {
        const auto *null_stats = reinterpret_cast<const D3D12_QUERY_DATA_SO_STATISTICS *>(words + 144);
        require(null_stats->NumPrimitivesWritten == 0 && null_stats->PrimitivesStorageNeeded == 1,
            "declared NULL buffer did not overflow the stream");
        std::puts("PASS,null-so-array,count=1,same-stream-stopped,written=0,needed=1");
    }
    for (UINT offset = capacity * stride0 / 4; offset < 64; ++offset)
        require(words[offset] == 0xcdcdcdcd, "SO overflow or tail overwritten");
    for (UINT offset = 64 + capacity * 4; offset < 128; ++offset)
        require(words[offset] == 0xcdcdcdcd, "second SO overflow or tail overwritten");
    const auto *stats = reinterpret_cast<const D3D12_QUERY_DATA_SO_STATISTICS *>(words + 136);
    require(stats[0].NumPrimitivesWritten == capacity && stats[0].PrimitivesStorageNeeded == 5, "stream 0 statistics");
    if (multiple_streams) require(stats[1].NumPrimitivesWritten == capacity &&
        stats[1].PrimitivesStorageNeeded == 5, "stream 1 statistics");
    if (rasterized) {
        const UINT last_id = iteration * 10 + 4;
        const UINT expected = (14 + iteration * 10) | ((30 + last_id) << 8) | ((40 + last_id) << 16) | 0xff000000u;
        require(words[256] == expected, "rasterized packed output pixel");
    }
    result->Unmap(0, &empty);
    std::printf("PASS,%s,%s,iteration=%u,written=%u,needed=5\n", rasterized ? "raster-and-so" :
        multiple_streams ? "gs-two-streams" : "vs-passthrough",
        overflow ? "overflow" : "full", iteration, capacity);
    c.reset();
}
static void run_null_case(Context &c, const std::vector<char> &vs, const std::vector<char> &gs,
                          UINT stride0, bool multiple_streams, UINT mode) {
    require(mode < 4, "invalid NULL test mode");
    const UINT strides[] = {stride0, 16};
    ComPtr<ID3D12PipelineState> pso;
    {
        const D3D12_SO_DECLARATION_ENTRY entries[] = {
            {0, "PACKED", 0, 1, 1, 0},
            {multiple_streams ? 1u : 0u, multiple_streams ? "PACKED" : "SV_Position", 0, 0,
                static_cast<BYTE>(multiple_streams ? 2 : 4), 1},
            {1, "PACKED", 1, 0, 2, 1},
        };
        auto desc = so_pipeline_desc(c, vs);
        if (multiple_streams) desc.GS = {gs.data(), gs.size()};
        desc.StreamOutput = {entries, multiple_streams ? 3u : 2u, strides, 2, D3D12_SO_NO_RASTERIZED_STREAM};
        check(c.device->CreateGraphicsPipelineState(&desc, IID_PPV_ARGS(&pso)), "Create NULL SO PSO");
    }
    const UINT64 counter_heap_size = 4 * 1024 * 1024, counter_base = counter_heap_size - 16;
    ComPtr<ID3D12Heap> counter_heap;
    D3D12_HEAP_DESC hd = {}; hd.SizeInBytes = counter_heap_size;
    hd.Properties.Type = D3D12_HEAP_TYPE_DEFAULT;
    hd.Properties.CreationNodeMask = hd.Properties.VisibleNodeMask = 1;
    hd.Flags = D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS;
    check(c.device->CreateHeap(&hd, IID_PPV_ARGS(&counter_heap)), "Create counter boundary heap");
    D3D12_RESOURCE_DESC rd = {}; rd.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
    rd.Width = counter_heap_size; rd.Height = rd.DepthOrArraySize = rd.MipLevels = 1;
    rd.SampleDesc.Count = 1; rd.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    ComPtr<ID3D12Resource> counters;
    check(c.device->CreatePlacedResource(counter_heap.Get(), 0, &rd, D3D12_RESOURCE_STATE_COPY_DEST,
        nullptr, IID_PPV_ARGS(&counters)), "Create counter boundary buffer");
    auto upload = c.buffer(1024, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    auto first = c.buffer(256, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    auto second = c.buffer(256, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    auto result = c.buffer(640, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    void *mapped = nullptr; const D3D12_RANGE empty = {0, 0};
    check(upload->Map(0, &empty, &mapped), "Map NULL SO upload");
    std::memset(mapped, 0xcd, 1024);
    static_cast<uint32_t *>(mapped)[1] = static_cast<uint32_t *>(mapped)[3] = 0;
    upload->Unmap(0, nullptr);
    c.list->CopyBufferRegion(first.Get(), 0, upload.Get(), 16, 256);
    c.list->CopyBufferRegion(second.Get(), 0, upload.Get(), 16, 256);
    c.list->CopyBufferRegion(counters.Get(), counter_base, upload.Get(), 0, 16);
    for (auto *resource : {first.Get(), second.Get(), counters.Get()})
        barrier(c.list.Get(), resource, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_STREAM_OUT);
    const D3D12_STREAM_OUTPUT_BUFFER_VIEW views[] = {
        {first->GetGPUVirtualAddress(), 256, counters->GetGPUVirtualAddress() + counter_base + 4},
        {second->GetGPUVirtualAddress(), 256, counters->GetGPUVirtualAddress() + counter_heap_size - 4},
    };
    ComPtr<ID3D12QueryHeap> queries;
    D3D12_QUERY_HEAP_DESC qdesc = {D3D12_QUERY_HEAP_TYPE_SO_STATISTICS, 4, 0};
    check(c.device->CreateQueryHeap(&qdesc, IID_PPV_ARGS(&queries)), "Create NULL SO queries");
    const D3D12_VIEWPORT viewport = {0, 0, 1, 1, 0, 1}; const D3D12_RECT scissor = {0, 0, 1, 1};
    c.list->RSSetViewports(1, &viewport); c.list->RSSetScissorRects(1, &scissor);
    c.list->SetPipelineState(pso.Get()); c.list->SetGraphicsRootSignature(c.root.Get());
    c.list->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_POINTLIST);
    const UINT initial = mode ? 3 : 0;
    const UINT blocked[] = {mode == 3 ? 1u : 0u, multiple_streams ? 3u : mode == 3 ? 1u : 0u};
    if (mode) {
        c.list->SOSetTargets(0, 2, views);
        c.list->SetGraphicsRoot32BitConstant(0, 10, 0);
        c.list->DrawInstanced(3, 1, 10, 0);
    } else c.list->SOSetTargets(1, 1, &views[1]);
    if (mode == 1) c.list->SOSetTargets(0, 1, nullptr);
    else if (mode == 2) {
        // A size-zero view ignores both addresses, including misalignment.
        const D3D12_STREAM_OUTPUT_BUFFER_VIEW zero = {1, 0, UINT64_MAX};
        c.list->SOSetTargets(0, 1, &zero);
    } else if (mode == 3) {
        auto limited = views[0]; limited.SizeInBytes = (initial + 1) * stride0;
        c.list->SOSetTargets(0, 1, &limited);
    }
    const UINT streams = multiple_streams ? 2 : 1;
    for (UINT stream = 0; stream < streams; ++stream)
        c.list->BeginQuery(queries.Get(), static_cast<D3D12_QUERY_TYPE>(D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0 + stream), stream);
    c.list->SetGraphicsRoot32BitConstant(0, 30, 0);
    c.list->DrawInstanced(3, 1, 30, 0);
    for (UINT stream = 0; stream < streams; ++stream) {
        const auto type = static_cast<D3D12_QUERY_TYPE>(D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0 + stream);
        c.list->EndQuery(queries.Get(), type, stream);
        c.list->ResolveQueryData(queries.Get(), type, stream, 1, result.Get(), 544 + stream * 16);
    }
    barrier(c.list.Get(), counters.Get(), D3D12_RESOURCE_STATE_STREAM_OUT, D3D12_RESOURCE_STATE_COPY_SOURCE);
    c.list->CopyBufferRegion(result.Get(), 528, counters.Get(), counter_base, 16);
    barrier(c.list.Get(), counters.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_STREAM_OUT);
    // Rebind only the formerly unbound/full slot. Its neighbour must retain its
    // binding and append from the counter saved before this render-pass break.
    c.list->SOSetTargets(0, 1, &views[0]);
    for (UINT stream = 0; stream < streams; ++stream)
        c.list->BeginQuery(queries.Get(), static_cast<D3D12_QUERY_TYPE>(D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0 + stream), 2 + stream);
    c.list->SetGraphicsRoot32BitConstant(0, 50, 0);
    c.list->DrawInstanced(3, 1, 50, 0);
    for (UINT stream = 0; stream < streams; ++stream) {
        const auto type = static_cast<D3D12_QUERY_TYPE>(D3D12_QUERY_TYPE_SO_STATISTICS_STREAM0 + stream);
        c.list->EndQuery(queries.Get(), type, 2 + stream);
        c.list->ResolveQueryData(queries.Get(), type, 2 + stream, 1, result.Get(), 576 + stream * 16);
    }
    c.list->SOSetTargets(0, 2, nullptr);
    for (auto *resource : {first.Get(), second.Get(), counters.Get()})
        barrier(c.list.Get(), resource, D3D12_RESOURCE_STATE_STREAM_OUT, D3D12_RESOURCE_STATE_COPY_SOURCE);
    c.list->CopyBufferRegion(result.Get(), 0, first.Get(), 0, 256);
    c.list->CopyBufferRegion(result.Get(), 256, second.Get(), 0, 256);
    c.list->CopyBufferRegion(result.Get(), 512, counters.Get(), counter_base, 16);
    c.submit();
    const D3D12_RANGE range = {0, 640}; check(result->Map(0, &range, &mapped), "Map NULL SO readback");
    const auto *words = static_cast<const uint32_t *>(mapped);
    const auto *values = static_cast<const float *>(mapped);
    for (UINT slot = 0; slot < 2; ++slot) {
        const UINT total = initial + blocked[slot] + 3;
        require(words[129 + slot * 2] == total * strides[slot], "NULL SO final counter");
        require(words[133 + slot * 2] == (initial + blocked[slot]) * strides[slot], "NULL SO limited counter");
        require(words[128 + slot * 2] == 0xcdcdcdcd && words[132 + slot * 2] == 0xcdcdcdcd,
            "counter32 guard changed");
        for (UINT word = 0; word < 64; ++word) {
            const UINT vertex = word / (strides[slot] / 4), component = word % (strides[slot] / 4);
            if (vertex >= total || component >= (slot ? 4u : 1u)) {
                require(words[slot * 64 + word] == 0xcdcdcdcd, "NULL SO padding/tail overwritten");
                continue;
            }
            const UINT id = vertex < initial ? 10 + vertex : vertex < initial + blocked[slot] ?
                30 + vertex - initial : 50 + vertex - initial - blocked[slot];
            const float position[] = {0, 0, 0.5f, 1};
            const float expected = !slot ? float(20 + id) : multiple_streams ?
                float(110 + component * 10 + id) : position[component];
            require(values[slot * 64 + word] == expected, "NULL SO payload/order mismatch");
        }
    }
    const auto *stats = reinterpret_cast<const D3D12_QUERY_DATA_SO_STATISTICS *>(words + 136);
    for (UINT stream = 0; stream < streams; ++stream) {
        require(stats[stream].NumPrimitivesWritten == blocked[stream] && stats[stream].PrimitivesStorageNeeded == 3,
            "NULL SO overflow statistics");
        require(stats[2 + stream].NumPrimitivesWritten == 3 && stats[2 + stream].PrimitivesStorageNeeded == 3,
            "NULL SO resumed statistics");
    }
    result->Unmap(0, &empty);
    std::printf("PASS,null-so-contract,streams=%u,stride=%u,mode=%u,counter32-heap-end\n", streams, stride0, mode);
    c.reset();
}
int wmain() {
    try {
        DWORD session = 0; require(ProcessIdToSessionId(GetCurrentProcessId(), &session) && session, "interactive session required");
        require(!GetEnvironmentVariableW(L"VKD3D_FEATURE_LEVEL", nullptr, 0) &&
            !GetEnvironmentVariableW(L"VKD3D_SHADER_MODEL", nullptr, 0) &&
            !GetEnvironmentVariableW(L"VKD3D_SHADER_OVERRIDE", nullptr, 0), "capability/shader override rejected");
        std::printf("PROCESS,%lu,%lu\n", GetCurrentProcessId(), session);
        const auto vs = read(L"stream-output-vs.dxil"), gs = read(L"stream-output-gs.dxil"), ps = read(L"stream-output-ps.dxil");
        Context context; initialize(context);
        for (unsigned i = 0; i < 2; ++i)
            for (bool multi : {false, true}) for (bool overflow : {false, true})
                run_case(context, vs, gs, ps, multi, overflow, i);
        for (unsigned i = 0; i < 2; ++i) run_case(context, vs, gs, ps, false, false, i, true);
        for (bool multi : {false, true}) for (UINT stride : {4u, 16u, 32u})
            for (UINT mode = 0; mode < 4; ++mode) run_null_case(context, vs, gs, stride, multi, mode);
        check(context.list->Close(), "Close final reset");
        std::puts("PASS,stream-output,34 cases");
        return 0;
    } catch (const std::exception &error) {
        std::fprintf(stderr, "FAIL,%s\n", error.what()); return 1;
    }
}
