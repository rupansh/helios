// Native Windows D3D12 / Helios ExecuteIndirect root-state acceptance. Run interactively
// using d3d12-indirect-probe.ps1 after the integrated driver review.
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
#include <cwchar>
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
static UINT64 qpc() {
    LARGE_INTEGER value = {};
    require(QueryPerformanceCounter(&value) != FALSE, "QueryPerformanceCounter");
    return static_cast<UINT64>(value.QuadPart);
}
struct SubmissionTiming {
    UINT64 submit_signal_ticks = 0;
    UINT64 completion_wait_ticks = 0;
};
[[noreturn]] static void pending_submission_failure(const char *operation, HRESULT hr) noexcept {
    std::fprintf(stderr, "FAIL,pending-submission,%s,hr=%08lx,terminating-without-unwind\n",
        operation, static_cast<unsigned long>(hr));
    std::fflush(stdout); std::fflush(stderr);
    // No completion proof exists. Normal exception unwinding would release
    // the submitted buffers/PSO before the GPU has finished using them.
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
    HANDLE event = nullptr;
    UINT64 next = 0;
    ~Context() { if (event) CloseHandle(event); }
    void execute_closed(SubmissionTiming *timing = nullptr, bool reset_while_pending = false) {
        ComPtr<ID3D12Fence> dependency;
        ComPtr<ID3D12CommandQueue> release_queue;
        ComPtr<ID3D12CommandAllocator> release_allocator, replacement_allocator;
        ComPtr<ID3D12GraphicsCommandList> release_list;
        if (reset_while_pending) {
            D3D12_COMMAND_QUEUE_DESC desc = {};
            check(device->CreateCommandQueue(&desc, IID_PPV_ARGS(&release_queue)), "Create release queue");
            check(device->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&dependency)), "Create dependency");
            check(device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT,
                IID_PPV_ARGS(&release_allocator)), "Create release allocator");
            check(device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT,
                IID_PPV_ARGS(&replacement_allocator)), "Create replacement allocator");
            check(device->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, release_allocator.Get(),
                nullptr, IID_PPV_ARGS(&release_list)), "Create release list");
            check(release_list->Close(), "Close release list");
            check(queue->Wait(dependency.Get(), 1), "Enqueue dependency wait");
        }
        struct PendingSubmission {
            bool pending = true;
            ~PendingSubmission() noexcept {
                if (pending) pending_submission_failure("exception before completion", E_UNEXPECTED);
            }
        } submission;
        const UINT64 before = timing ? qpc() : 0;
        ID3D12CommandList *lists[] = {list.Get()};
        queue->ExecuteCommandLists(1, lists);
        HRESULT hr = queue->Signal(fence.Get(), ++next);
        if (FAILED(hr)) pending_submission_failure("Signal", hr);
        const UINT64 submitted = timing ? qpc() : 0;
        hr = fence->SetEventOnCompletion(next, event);
        if (FAILED(hr)) pending_submission_failure("SetEventOnCompletion", hr);
        if (reset_while_pending) {
            const DWORD negative_wait = WaitForSingleObject(event, 20);
            const UINT64 completed_before = fence->GetCompletedValue();
            const bool stayed_pending = negative_wait == WAIT_TIMEOUT && completed_before == next - 1;
            // Resetting the public list is legal while its previous execution
            // is pending. Its allocator is kept alive and is never Reset here.
            hr = list->Reset(replacement_allocator.Get(), nullptr);
            if (FAILED(hr)) pending_submission_failure("pending List Reset", hr);
            hr = list->Close();
            if (FAILED(hr)) pending_submission_failure("Close replacement list", hr);
            ID3D12CommandList *release_lists[] = {release_list.Get()};
            release_queue->ExecuteCommandLists(1, release_lists);
            hr = release_queue->Signal(dependency.Get(), 1);
            if (FAILED(hr)) pending_submission_failure("release queue Signal", hr);
            std::printf("LIFETIME,negative_wait=%lu,completed_before=%llu,expected=%llu,list_reset=00000000,other_queue=1\n",
                static_cast<unsigned long>(negative_wait), static_cast<unsigned long long>(completed_before),
                static_cast<unsigned long long>(next));
            // An early signal invalidates the completion proof. Do not unwind
            // queued owners using that same erroneous signal as GPU retirement.
            if (!stayed_pending) pending_submission_failure("completion passed an unsignaled dependency", E_FAIL);
        }
        const DWORD wait = WaitForSingleObject(event, 60000);
        if (wait != WAIT_OBJECT_0) pending_submission_failure("completion wait",
            HRESULT_FROM_WIN32(wait == WAIT_FAILED ? GetLastError() : ERROR_TIMEOUT));
        const UINT64 completed = fence->GetCompletedValue();
        if (completed == UINT64_MAX || completed < next)
            pending_submission_failure("unauthenticated completion", DXGI_ERROR_DEVICE_REMOVED);
        hr = device->GetDeviceRemovedReason();
        if (FAILED(hr)) pending_submission_failure("GetDeviceRemovedReason", hr);
        submission.pending = false;
        if (timing) {
            timing->submit_signal_ticks = submitted - before;
            timing->completion_wait_ticks = qpc() - submitted;
        }
    }
    void reset() {
        check(allocator->Reset(), "Allocator Reset");
        check(list->Reset(allocator.Get(), nullptr), "List Reset");
    }
    ComPtr<ID3D12Resource> buffer(UINT64 bytes, D3D12_HEAP_TYPE type,
            D3D12_RESOURCE_STATES state, D3D12_RESOURCE_FLAGS flags = D3D12_RESOURCE_FLAG_NONE,
            ID3D12Heap *placed_heap = nullptr) {
        D3D12_HEAP_PROPERTIES heap = {}; heap.Type = type;
        heap.CreationNodeMask = heap.VisibleNodeMask = 1;
        D3D12_RESOURCE_DESC desc = {}; desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
        desc.Width = bytes; desc.Height = 1; desc.DepthOrArraySize = 1; desc.MipLevels = 1;
        desc.SampleDesc.Count = 1; desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR; desc.Flags = flags;
        ComPtr<ID3D12Resource> result;
        if (placed_heap) check(device->CreatePlacedResource(placed_heap, 0, &desc, state,
            nullptr, IID_PPV_ARGS(&result)), "CreatePlacedResource");
        else check(device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state,
            nullptr, IID_PPV_ARGS(&result)), "CreateCommittedResource");
        return result;
    }
};
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
        check(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_11_0,
            IID_PPV_ARGS(&c.device)), "D3D12CreateDevice Helios");
        std::printf("ADAPTER,%04x,%04x,%08lx:%08lx\n", desc.VendorId, desc.DeviceId,
            static_cast<unsigned long>(desc.AdapterLuid.HighPart), desc.AdapterLuid.LowPart);
        break;
    }
    require(c.device != nullptr, "Helios adapter not found");
    native_modules();
    D3D12_COMMAND_QUEUE_DESC queue = {};
    check(c.device->CreateCommandQueue(&queue, IID_PPV_ARGS(&c.queue)), "CreateCommandQueue");
    check(c.device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT,
        IID_PPV_ARGS(&c.allocator)), "CreateCommandAllocator");
    check(c.device->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT,
        c.allocator.Get(), nullptr, IID_PPV_ARGS(&c.list)), "CreateCommandList");
    check(c.device->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&c.fence)), "CreateFence");
    c.event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    require(c.event != nullptr, "CreateEvent");
}
static ComPtr<ID3D12RootSignature> root_signature(Context &c,
        const D3D12_ROOT_PARAMETER *parameters, UINT count, D3D12_ROOT_SIGNATURE_FLAGS flags = D3D12_ROOT_SIGNATURE_FLAG_NONE) {
    D3D12_ROOT_SIGNATURE_DESC desc = {}; desc.NumParameters = count; desc.pParameters = parameters; desc.Flags = flags;
    ComPtr<ID3DBlob> blob, errors;
    check(D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1,
        &blob, &errors), "SerializeRootSignature");
    ComPtr<ID3D12RootSignature> result;
    check(c.device->CreateRootSignature(0, blob->GetBufferPointer(), blob->GetBufferSize(),
        IID_PPV_ARGS(&result)), "CreateRootSignature");
    return result;
}
static void barrier(Context &c, ID3D12Resource *resource,
        D3D12_RESOURCE_STATES before, D3D12_RESOURCE_STATES after) {
    D3D12_RESOURCE_BARRIER b = {}; b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    b.Transition = {resource, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, before, after};
    c.list->ResourceBarrier(1, &b);
}
static void alias(Context &c, ID3D12Resource *before, ID3D12Resource *after) {
    D3D12_RESOURCE_BARRIER b = {}; b.Type = D3D12_RESOURCE_BARRIER_TYPE_ALIASING;
    b.Aliasing = {before, after}; c.list->ResourceBarrier(1, &b);
}
static void run_roots(Context &c, bool ia) {
    ComPtr<ID3D12RootSignature> graphics_root, producer_root;
    ComPtr<ID3D12PipelineState> graphics, producer;
    ComPtr<ID3D12CommandSignature> signature;
    // Creation descriptions and shader bytes die before recording. The lazy
    // private pipeline must own all creation inputs it needs subsequently.
    {
        D3D12_ROOT_PARAMETER parameters[3] = {};
        parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
        parameters[0].Constants = {0, 0, 1};
        parameters[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_CBV;
        parameters[1].Descriptor.ShaderRegister = 1;
        parameters[2].ParameterType = D3D12_ROOT_PARAMETER_TYPE_UAV;
        graphics_root = root_signature(c, parameters, 3, ia ?
            D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT : D3D12_ROOT_SIGNATURE_FLAG_NONE);
        parameters[0] = {}; parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_CBV;
        parameters[1] = {}; parameters[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_UAV;
        producer_root = root_signature(c, parameters, 2);
        const auto cs = read(ia ? L"indirect-ia-producer.dxil" : L"indirect-producer.dxil");
        D3D12_COMPUTE_PIPELINE_STATE_DESC compute = {};
        compute.pRootSignature = producer_root.Get(); compute.CS = {cs.data(), cs.size()};
        check(c.device->CreateComputePipelineState(&compute, IID_PPV_ARGS(&producer)), "Create producer PSO");
        const auto vs = read(ia ? L"indirect-ia-vs.dxil" : L"indirect-consumer-vs.dxil"),
            ps = read(ia ? L"indirect-ia-ps.dxil" : L"indirect-consumer-ps.dxil");
        D3D12_GRAPHICS_PIPELINE_STATE_DESC desc = {};
        desc.pRootSignature = graphics_root.Get(); desc.VS = {vs.data(), vs.size()}; desc.PS = {ps.data(), ps.size()};
        D3D12_INPUT_ELEMENT_DESC element = {"COLOR", 0, DXGI_FORMAT_R32_UINT, 0, 0,
            D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA, 0};
        if (ia) desc.InputLayout = {&element, 1};
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
        desc.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        check(c.device->CreateGraphicsPipelineState(&desc, IID_PPV_ARGS(&graphics)), "Create consumer PSO");
        D3D12_INDIRECT_ARGUMENT_DESC args[5] = {};
        args[0].Type = D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT; args[0].Constant.Num32BitValuesToSet = 1;
        args[1].Type = D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT_BUFFER_VIEW; args[1].ConstantBufferView.RootParameterIndex = 1;
        args[2].Type = D3D12_INDIRECT_ARGUMENT_TYPE_DRAW;
        if (ia) {
            args[2].Type = D3D12_INDIRECT_ARGUMENT_TYPE_VERTEX_BUFFER_VIEW;
            args[2].VertexBuffer.Slot = 0;
            args[3].Type = D3D12_INDIRECT_ARGUMENT_TYPE_INDEX_BUFFER_VIEW;
            args[4].Type = D3D12_INDIRECT_ARGUMENT_TYPE_DRAW_INDEXED;
        }
        D3D12_COMMAND_SIGNATURE_DESC sig = {ia ? 64u : 28u, ia ? 5u : 3u, args, 0};
        check(c.device->CreateCommandSignature(&sig, graphics_root.Get(), IID_PPV_ARGS(&signature)),
            "Create root-state command signature");
    }
    const auto reads = D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER | D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT |
        (ia ? D3D12_RESOURCE_STATE_INDEX_BUFFER : D3D12_RESOURCE_STATE_COMMON);
    D3D12_HEAP_DESC heap = {}; heap.SizeInBytes = D3D12_DEFAULT_RESOURCE_PLACEMENT_ALIGNMENT;
    heap.Properties.Type = D3D12_HEAP_TYPE_DEFAULT;
    heap.Properties.CreationNodeMask = heap.Properties.VisibleNodeMask = 1;
    heap.Flags = D3D12_HEAP_FLAG_ALLOW_ONLY_BUFFERS;
    ComPtr<ID3D12Heap> alias_heap;
    check(c.device->CreateHeap(&heap, IID_PPV_ARGS(&alias_heap)), "Create alias heap");
    auto packet = c.buffer(1536, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    auto source = c.buffer(1536, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, alias_heap.Get());
    auto target = c.buffer(1536, D3D12_HEAP_TYPE_DEFAULT, reads, D3D12_RESOURCE_FLAG_NONE, alias_heap.Get());
    auto output = c.buffer(16, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    auto input = c.buffer(256, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    auto zero = c.buffer(16, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    const SIZE_T readback_size = 16 + (ia ? sizeof(D3D12_QUERY_DATA_PIPELINE_STATISTICS) : 0);
    auto readback = c.buffer(readback_size, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    ComPtr<ID3D12QueryHeap> statistics;
    if (ia) {
        D3D12_QUERY_HEAP_DESC desc = {}; desc.Type = D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS; desc.Count = 1;
        check(c.device->CreateQueryHeap(&desc, IID_PPV_ARGS(&statistics)), "Create IA statistics heap");
    }
    D3D12_RANGE no_read = {0, 0}, written = {0, 16}, read_range = {0, readback_size};
    void *mapped = nullptr;
    check(zero->Map(0, &no_read, &mapped), "Map zero"); std::memset(mapped, 0, 16); zero->Unmap(0, &written);
    const UINT counts[] = {3, 1, 0, 7};
    for (UINT route = 0; route < 3; ++route) {
        if (route) c.reset();
        ID3D12Resource *consumer = route == 2 ? target.Get() : packet.Get();
        const UINT64 address = consumer->GetGPUVirtualAddress();
        barrier(c, output.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_DEST);
        c.list->CopyBufferRegion(output.Get(), 0, zero.Get(), 0, 16);
        barrier(c, output.Get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        c.list->SetComputeRootSignature(producer_root.Get()); c.list->SetPipelineState(producer.Get());
        c.list->SetComputeRootConstantBufferView(0, input->GetGPUVirtualAddress());
        c.list->SetComputeRootUnorderedAccessView(1, route == 2 ? source->GetGPUVirtualAddress() : address);
        if (route == 2) alias(c, nullptr, source.Get());
        c.list->Dispatch(1, 1, 1);
        if (route == 2) alias(c, source.Get(), target.Get());
        else barrier(c, consumer, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
            route == 1 ? D3D12_RESOURCE_STATE_COMMON : reads);
        D3D12_VIEWPORT viewport = {0, 0, 1, 1, 0, 1}; D3D12_RECT scissor = {0, 0, 1, 1};
        c.list->RSSetViewports(1, &viewport); c.list->RSSetScissorRects(1, &scissor);
        c.list->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        c.list->SetGraphicsRootSignature(graphics_root.Get()); c.list->SetPipelineState(graphics.Get());
        c.list->SetGraphicsRoot32BitConstant(0, 3, 0);
        c.list->SetGraphicsRootConstantBufferView(1, address + 512);
        c.list->SetGraphicsRootUnorderedAccessView(2, output->GetGPUVirtualAddress());
        D3D12_VERTEX_BUFFER_VIEW initial_vb = {address + 528, 12, 4};
        if (ia) c.list->IASetVertexBuffers(0, 1, &initial_vb);
        if (ia) c.list->BeginQuery(statistics.Get(), D3D12_QUERY_TYPE_PIPELINE_STATISTICS, 0);
        c.list->SetPredication(consumer, 256, D3D12_PREDICATION_OP_EQUAL_ZERO);
        c.list->DrawInstanced(3, 1, 0, 0);
        if (ia) c.list->ExecuteIndirect(signature.Get(), 3, consumer, 284, consumer, 280);
        c.list->SetPredication(consumer, 264, D3D12_PREDICATION_OP_EQUAL_ZERO);
        c.list->ExecuteIndirect(signature.Get(), 3, consumer, 284, consumer, 280);
        // NULL root descriptors may not be dereferenced. Restore a valid CBV
        // before drawing; the output witnesses root-constant clearing only.
        c.list->SetGraphicsRootConstantBufferView(1, address + 512);
        if (ia) c.list->IASetVertexBuffers(0, 1, &initial_vb);
        c.list->DrawInstanced(3, 1, 0, 0);
        if (ia) c.list->EndQuery(statistics.Get(), D3D12_QUERY_TYPE_PIPELINE_STATISTICS, 0);
        c.list->SetPredication(nullptr, 0, D3D12_PREDICATION_OP_EQUAL_ZERO);
        barrier(c, output.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
        if (ia) {
            // The barrier ended rendering. Resolve immediately under a false
            // predicate; it must include the ordinary prefix/suffix and replay.
            c.list->SetPredication(consumer, 256, D3D12_PREDICATION_OP_EQUAL_ZERO);
            c.list->ResolveQueryData(statistics.Get(), D3D12_QUERY_TYPE_PIPELINE_STATISTICS, 0, 1, readback.Get(), 16);
            c.list->SetPredication(nullptr, 0, D3D12_PREDICATION_OP_EQUAL_ZERO);
        }
        if (route != 2) barrier(c, consumer, reads, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        c.list->CopyBufferRegion(readback.Get(), 0, output.Get(), 0, 16);
        barrier(c, output.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        check(c.list->Close(), "Close roots list");
        for (UINT replay = 0; replay < 4; ++replay) {
            const UINT seed = 37 + 16 * replay + 100 * route;
            const UINT parameters[] = {seed, counts[replay], static_cast<UINT>(address), static_cast<UINT>(address >> 32)};
            check(input->Map(0, &no_read, &mapped), "Map producer input");
            std::memcpy(mapped, parameters, sizeof(parameters)); input->Unmap(0, &written);
            c.execute_closed(nullptr, ia && route == 2 && replay == 3);
            check(readback->Map(0, &read_range, &mapped), "Map readback");
            UINT values[4]; std::memcpy(values, mapped, sizeof(values));
            D3D12_QUERY_DATA_PIPELINE_STATISTICS query = {};
            if (ia) std::memcpy(&query, static_cast<const char *>(mapped) + 16, sizeof(query));
            readback->Unmap(0, &no_read);
            bool readback_matches = true;
            for (UINT word = 0; word < 4; ++word) {
                const UINT enabled = counts[replay] < 3 ? counts[replay] : 3;
                const UINT expected = (word < enabled ? (ia ? 3 * seed + 7 * word : 2 * (seed + word)) : 0) +
                    (!word ? (ia ? 3 * seed : 2 * seed) : 0);
                std::printf("READBACK,route=%u,replay=%u,word=%u,value=%u,expected=%u,fence=%llu\n",
                    route, replay, word, values[word], expected, static_cast<unsigned long long>(c.next));
                readback_matches = readback_matches && values[word] == expected;
            }
            if (ia) {
                const UINT64 primitives = 1 + (counts[replay] < 3 ? counts[replay] : 3);
                std::printf("QUERY,route=%u,replay=%u,ia_vertices=%llu,ia_primitives=%llu,cs=%llu,ps=%llu\n",
                    route, replay, static_cast<unsigned long long>(query.IAVertices),
                    static_cast<unsigned long long>(query.IAPrimitives), static_cast<unsigned long long>(query.CSInvocations),
                    static_cast<unsigned long long>(query.PSInvocations));
                require(query.IAVertices == 3 * primitives && query.IAPrimitives == primitives &&
                    query.CSInvocations == 0 && query.PSInvocations > 0, "IA query continuation or unconditional resolve mismatch");
            }
            require(readback_matches, "GPU-produced indirect root/IA readback mismatch");
            std::printf("PASS,indirect-%s,route=%u,replay=%u,count=%u\n", ia ? "ia" : "roots", route, replay, counts[replay]);
        }
    }
    // Invalid recording is never submitted. Failure must poison Reset while
    // leaving the device alive. Core-runtime validation can intercept this
    // before the DDI: this is no engine-error-latch or OOM-injection witness.
    c.reset(); c.list->SetGraphicsRootSignature(graphics_root.Get()); c.list->SetPipelineState(graphics.Get());
    c.list->ExecuteIndirect(signature.Get(), 3, target.Get(), 1536, nullptr, 0);
    const HRESULT close = c.list->Close();
    const HRESULT reset = c.list->Reset(c.allocator.Get(), nullptr);
    require(close == E_INVALIDARG && reset == close, "failed Close did not quarantine Reset");
    check(c.device->GetDeviceRemovedReason(), "negative case device health");
    std::printf("PASS,indirect-invalid-extent,close=%08lx,reset=%08lx\n",
        static_cast<unsigned long>(close), static_cast<unsigned long>(reset));
}
// Equivalent-output overhead measurement, not an optimization comparison. The
// direct reference knows the deterministic count on the CPU. The indirect arm
// reads GPU-produced arguments/counts and still records MaxCommandCount slots.
static void measure_roots(Context &c, const wchar_t *witness_path) {
    constexpr UINT max_commands = 1024, width = max_commands + 1;
    constexpr UINT argument_offset = 256, argument_stride = 28;
    constexpr UINT cbv_offset = (argument_offset + max_commands * argument_stride + 255) & ~255u;
    constexpr UINT packet_bytes = cbv_offset + (max_commands + 1) * 256;
    const UINT counts[] = {max_commands, 1, 0};
    LARGE_INTEGER cpu_frequency = {};
    require(QueryPerformanceFrequency(&cpu_frequency) && cpu_frequency.QuadPart > 0,
        "QueryPerformanceFrequency");
    UINT64 gpu_frequency = 0;
    check(c.queue->GetTimestampFrequency(&gpu_frequency), "GetTimestampFrequency");
    require(gpu_frequency != 0, "zero GPU timestamp frequency");
    std::printf("PERF_CONFIG,max_commands=%u,width=%u,warm_samples=5,cpu_frequency=%llu,gpu_frequency=%llu\n",
        max_commands, width, static_cast<unsigned long long>(cpu_frequency.QuadPart),
        static_cast<unsigned long long>(gpu_frequency));

    D3D12_ROOT_PARAMETER parameters[2] = {};
    parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
    parameters[0].Constants = {0, 0, 1};
    parameters[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_CBV;
    parameters[1].Descriptor.ShaderRegister = 1;
    auto graphics_root = root_signature(c, parameters, 2);
    parameters[0] = {}; parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_CBV;
    parameters[1] = {}; parameters[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_UAV;
    auto producer_root = root_signature(c, parameters, 2);
    ComPtr<ID3D12PipelineState> graphics, producer;
    {
        const auto cs = read(L"indirect-perf-producer.dxil");
        D3D12_COMPUTE_PIPELINE_STATE_DESC desc = {};
        desc.pRootSignature = producer_root.Get(); desc.CS = {cs.data(), cs.size()};
        check(c.device->CreateComputePipelineState(&desc, IID_PPV_ARGS(&producer)), "Create perf producer PSO");
        const auto vs = read(L"indirect-perf-vs.dxil"), ps = read(L"indirect-perf-ps.dxil");
        D3D12_GRAPHICS_PIPELINE_STATE_DESC graphics_desc = {};
        graphics_desc.pRootSignature = graphics_root.Get();
        graphics_desc.VS = {vs.data(), vs.size()}; graphics_desc.PS = {ps.data(), ps.size()};
        graphics_desc.SampleMask = UINT_MAX; graphics_desc.SampleDesc.Count = 1;
        graphics_desc.NumRenderTargets = 1; graphics_desc.RTVFormats[0] = DXGI_FORMAT_R32_UINT;
        graphics_desc.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        graphics_desc.RasterizerState.FillMode = D3D12_FILL_MODE_SOLID;
        graphics_desc.RasterizerState.CullMode = D3D12_CULL_MODE_NONE;
        graphics_desc.RasterizerState.DepthClipEnable = TRUE;
        graphics_desc.DepthStencilState.DepthFunc = D3D12_COMPARISON_FUNC_ALWAYS;
        graphics_desc.DepthStencilState.StencilReadMask = graphics_desc.DepthStencilState.StencilWriteMask = 0xff;
        graphics_desc.DepthStencilState.FrontFace = {D3D12_STENCIL_OP_KEEP, D3D12_STENCIL_OP_KEEP,
            D3D12_STENCIL_OP_KEEP, D3D12_COMPARISON_FUNC_ALWAYS};
        graphics_desc.DepthStencilState.BackFace = graphics_desc.DepthStencilState.FrontFace;
        auto &blend = graphics_desc.BlendState.RenderTarget[0];
        blend.SrcBlend = blend.SrcBlendAlpha = D3D12_BLEND_ONE;
        blend.DestBlend = blend.DestBlendAlpha = D3D12_BLEND_ZERO;
        blend.BlendOp = blend.BlendOpAlpha = D3D12_BLEND_OP_ADD;
        blend.LogicOp = D3D12_LOGIC_OP_NOOP; blend.RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL;
        check(c.device->CreateGraphicsPipelineState(&graphics_desc, IID_PPV_ARGS(&graphics)), "Create perf graphics PSO");
    }
    D3D12_INDIRECT_ARGUMENT_DESC args[3] = {};
    args[0].Type = D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT; args[0].Constant.Num32BitValuesToSet = 1;
    args[1].Type = D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT_BUFFER_VIEW; args[1].ConstantBufferView.RootParameterIndex = 1;
    args[2].Type = D3D12_INDIRECT_ARGUMENT_TYPE_DRAW;
    D3D12_COMMAND_SIGNATURE_DESC sig = {argument_stride, 3, args, 0};
    ComPtr<ID3D12CommandSignature> signature;
    check(c.device->CreateCommandSignature(&sig, graphics_root.Get(), IID_PPV_ARGS(&signature)), "Create perf signature");
    auto packet = c.buffer(packet_bytes, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    auto input = c.buffer(256, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    D3D12_RESOURCE_DESC rt_desc = {}; rt_desc.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
    rt_desc.Width = width; rt_desc.Height = 1; rt_desc.DepthOrArraySize = rt_desc.MipLevels = 1;
    rt_desc.Format = DXGI_FORMAT_R32_UINT; rt_desc.SampleDesc.Count = 1;
    rt_desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    D3D12_HEAP_PROPERTIES heap = {}; heap.Type = D3D12_HEAP_TYPE_DEFAULT;
    heap.CreationNodeMask = heap.VisibleNodeMask = 1;
    ComPtr<ID3D12Resource> target;
    check(c.device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &rt_desc,
        D3D12_RESOURCE_STATE_RENDER_TARGET, nullptr, IID_PPV_ARGS(&target)), "Create perf target");
    D3D12_DESCRIPTOR_HEAP_DESC rtv_desc = {}; rtv_desc.Type = D3D12_DESCRIPTOR_HEAP_TYPE_RTV;
    rtv_desc.NumDescriptors = 1;
    ComPtr<ID3D12DescriptorHeap> rtv_heap;
    check(c.device->CreateDescriptorHeap(&rtv_desc, IID_PPV_ARGS(&rtv_heap)), "Create perf RTV heap");
    const auto rtv = rtv_heap->GetCPUDescriptorHandleForHeapStart();
    c.device->CreateRenderTargetView(target.Get(), nullptr, rtv);
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT footprint = {};
    UINT rows = 0; UINT64 row_bytes = 0, total_bytes = 0;
    c.device->GetCopyableFootprints(&rt_desc, 0, 1, 0, &footprint, &rows, &row_bytes, &total_bytes);
    require(rows == 1 && row_bytes == width * sizeof(UINT) && footprint.Offset == 0 &&
        footprint.Footprint.RowPitch >= row_bytes && total_bytes >= row_bytes, "invalid perf footprint");
    const UINT64 timestamp_offset = footprint.Footprint.RowPitch;
    const UINT64 readback_bytes = timestamp_offset + 2 * sizeof(UINT64);
    auto readback = c.buffer(readback_bytes, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    D3D12_QUERY_HEAP_DESC query_desc = {}; query_desc.Type = D3D12_QUERY_HEAP_TYPE_TIMESTAMP; query_desc.Count = 2;
    ComPtr<ID3D12QueryHeap> timestamps;
    check(c.device->CreateQueryHeap(&query_desc, IID_PPV_ARGS(&timestamps)), "Create perf timestamp heap");
    // The runner supplies a unique owned run directory. Preserve every raw
    // pixel before grading; a failed sample must not disappear from evidence.
    require(GetFileAttributesW(witness_path) == INVALID_FILE_ATTRIBUTES, "perf witness already exists");
    std::ofstream witness(witness_path, std::ios::binary);
    require(static_cast<bool>(witness), "create perf witness");
    const UINT64 address = packet->GetGPUVirtualAddress();
    const auto reads = D3D12_RESOURCE_STATE_VERTEX_AND_CONSTANT_BUFFER | D3D12_RESOURCE_STATE_INDIRECT_ARGUMENT;
    D3D12_RANGE no_read = {0, 0}, written = {0, 32}, read_range = {0, static_cast<SIZE_T>(readback_bytes)};
    UINT ordinal = 0;
    // Focused direct block then indirect block for each count. Every sample,
    // including first-use/warmup, is archived; no selected minima or FPS claim.
    for (const UINT count : counts) for (UINT arm = 0; arm < 2; ++arm) for (int sample = -1; sample < 5; ++sample) {
        if (ordinal) c.reset();
        const UINT seed = 1009 + static_cast<UINT>(sample + 1) * 17;
        const UINT producer_parameters[] = {seed, count, static_cast<UINT>(address), static_cast<UINT>(address >> 32),
            max_commands, argument_offset, cbv_offset, width};
        void *mapped = nullptr;
        check(input->Map(0, &no_read, &mapped), "Map perf producer parameters");
        std::memcpy(mapped, producer_parameters, sizeof(producer_parameters)); input->Unmap(0, &written);
        c.list->SetComputeRootSignature(producer_root.Get()); c.list->SetPipelineState(producer.Get());
        c.list->SetComputeRootConstantBufferView(0, input->GetGPUVirtualAddress());
        c.list->SetComputeRootUnorderedAccessView(1, address);
        c.list->Dispatch((max_commands + 1 + 63) / 64, 1, 1);
        barrier(c, packet.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, reads);
        const float zero[4] = {};
        c.list->ClearRenderTargetView(rtv, zero, 0, nullptr);
        // Materialize the engine's deferred clear before timestamp0. Merely
        // binding the RTV or writing a timestamp leaves it as a loadOp CLEAR
        // on the first draw, which would contaminate the measured body.
        barrier(c, target.Get(), D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_SOURCE);
        barrier(c, target.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_RENDER_TARGET);
        c.list->OMSetRenderTargets(1, &rtv, FALSE, nullptr);
        D3D12_VIEWPORT viewport = {0, 0, static_cast<float>(width), 1, 0, 1};
        D3D12_RECT scissor = {0, 0, static_cast<LONG>(width), 1};
        c.list->RSSetViewports(1, &viewport); c.list->RSSetScissorRects(1, &scissor);
        c.list->IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        c.list->SetGraphicsRootSignature(graphics_root.Get()); c.list->SetPipelineState(graphics.Get());
        c.list->SetGraphicsRoot32BitConstant(0, 0, 0);
        c.list->SetGraphicsRootConstantBufferView(1, address + cbv_offset);
        c.list->EndQuery(timestamps.Get(), D3D12_QUERY_TYPE_TIMESTAMP, 0);
        const UINT64 record_start = qpc();
        if (arm) c.list->ExecuteIndirect(signature.Get(), max_commands, packet.Get(), argument_offset, packet.Get(), 0);
        else for (UINT i = 0; i < count; ++i) {
            c.list->SetGraphicsRoot32BitConstant(0, i, 0);
            c.list->SetGraphicsRootConstantBufferView(1, address + cbv_offset + static_cast<UINT64>(i) * 256);
            c.list->DrawInstanced(3, 1, 0, 0);
        }
        // Both arms pay for the same legal suffix. Never dereference the NULL
        // CBV which ExecuteIndirect is specified to leave after execution.
        c.list->SetGraphicsRoot32BitConstant(0, max_commands, 0);
        c.list->SetGraphicsRootConstantBufferView(1, address + cbv_offset + static_cast<UINT64>(max_commands) * 256);
        c.list->DrawInstanced(3, 1, 0, 0);
        const UINT64 record_ticks = qpc() - record_start;
        c.list->EndQuery(timestamps.Get(), D3D12_QUERY_TYPE_TIMESTAMP, 1);
        barrier(c, packet.Get(), reads, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        barrier(c, target.Get(), D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_SOURCE);
        D3D12_TEXTURE_COPY_LOCATION src = {}, dst = {};
        src.pResource = target.Get(); src.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
        dst.pResource = readback.Get(); dst.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT; dst.PlacedFootprint = footprint;
        c.list->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
        barrier(c, target.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_RENDER_TARGET);
        c.list->ResolveQueryData(timestamps.Get(), D3D12_QUERY_TYPE_TIMESTAMP, 0, 2, readback.Get(), timestamp_offset);
        const UINT64 close_start = qpc(); check(c.list->Close(), "Close perf list");
        const UINT64 close_ticks = qpc() - close_start;
        SubmissionTiming submission;
        c.execute_closed(&submission);
        check(readback->Map(0, &read_range, &mapped), "Map perf readback");
        std::array<UINT, width> values = {};
        UINT64 gpu_ticks[2] = {};
        std::memcpy(values.data(), static_cast<const char *>(mapped) + footprint.Offset, sizeof(values));
        std::memcpy(gpu_ticks, static_cast<const char *>(mapped) + timestamp_offset, sizeof(gpu_ticks));
        readback->Unmap(0, &no_read);
        witness.write(reinterpret_cast<const char *>(values.data()), sizeof(values)); witness.flush();
        require(static_cast<bool>(witness), "write perf witness");
        const char *phase = sample >= 0 ? "warm" : count == max_commands ? "first-use" : "warmup";
        std::printf("PERF,ordinal=%u,arm=%s,count=%u,sample=%d,phase=%s,seed=%u,record_ticks=%llu,close_ticks=%llu,"
            "submit_signal_ticks=%llu,completion_wait_ticks=%llu,gpu_begin=%llu,gpu_end=%llu,fence=%llu\n",
            ordinal, arm ? "indirect" : "direct", count, sample, phase, seed,
            static_cast<unsigned long long>(record_ticks), static_cast<unsigned long long>(close_ticks),
            static_cast<unsigned long long>(submission.submit_signal_ticks),
            static_cast<unsigned long long>(submission.completion_wait_ticks),
            static_cast<unsigned long long>(gpu_ticks[0]), static_cast<unsigned long long>(gpu_ticks[1]),
            static_cast<unsigned long long>(c.next));
        require(gpu_ticks[1] > gpu_ticks[0], "non-increasing GPU timestamps");
        for (UINT i = 0; i < width; ++i) {
            const UINT expected = i < count || i == max_commands ? 23 * seed + 41 * i : 0;
            if (values[i] != expected) {
                std::fprintf(stderr, "FAIL,perf-readback,ordinal=%u,pixel=%u,value=%u,expected=%u\n",
                    ordinal, i, values[i], expected);
                throw std::runtime_error("perf GPU readback mismatch");
            }
        }
        ++ordinal;
    }
    std::puts("PASS,indirect-measurement,36 samples,36900 words");
}
int wmain(int argc, wchar_t **argv) {
    try {
        const bool ia = argc == 2 && !std::wcscmp(argv[1], L"--ia");
        require(argc == 1 || ia || (argc == 3 && !std::wcscmp(argv[1], L"--measure")),
            "usage: d3d12_indirect_probe.exe [--ia | --measure witness-path]");
        Context context; initialize(context);
        if (argc == 3) measure_roots(context, argv[2]);
        else {
            run_roots(context, ia);
            std::puts(ia ? "PASS,indirect-ia,12 cases,48 words" : "PASS,indirect-roots,12 cases,48 words");
        }
        return 0;
    } catch (const std::exception &error) {
        std::fprintf(stderr, "FAIL,%s\n", error.what());
        return 1;
    }
}
