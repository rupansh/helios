// Native Microsoft D3D12 / Helios root-signature acceptance. Run through
// d3d12-root-signature-probe.ps1 in the interactive session.
#include <windows.h>
#include <d3d12.h>
#include <d3d12sdklayers.h>
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
    void execute_closed(SubmissionTiming *timing = nullptr) {
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

static void transition(Context &c, ID3D12Resource *r, D3D12_RESOURCE_STATES before,
        D3D12_RESOURCE_STATES after) {
    D3D12_RESOURCE_BARRIER b = {}; b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    b.Transition.pResource = r; b.Transition.Subresource = D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
    b.Transition.StateBefore = before; b.Transition.StateAfter = after;
    c.list->ResourceBarrier(1, &b);
}

static void run_roots(Context &c) {
    const auto shader = read(L"root-signature-cs.dxil");
    auto source = c.buffer(256, D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    auto output = c.buffer(256, D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS);
    auto readback = c.buffer(256, D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    D3D12_DESCRIPTOR_HEAP_DESC hd = {}; hd.Type = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
    hd.NumDescriptors = 1; hd.Flags = D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE;
    ComPtr<ID3D12DescriptorHeap> heap;
    check(c.device->CreateDescriptorHeap(&hd, IID_PPV_ARGS(&heap)), "descriptor heap");
    D3D12_CONSTANT_BUFFER_VIEW_DESC cbv = {source->GetGPUVirtualAddress(), 256};
    c.device->CreateConstantBufferView(&cbv, heap->GetCPUDescriptorHandleForHeapStart());
    UINT cases = 0;
    for (UINT kind = 0; kind < 4; ++kind) {
        D3D12_DESCRIPTOR_RANGE1 range = {};
        range.RangeType = D3D12_DESCRIPTOR_RANGE_TYPE_CBV; range.NumDescriptors = 1;
        range.Flags = kind == 2 ? D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_VOLATILE |
            D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE :
            D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_STATIC_KEEPING_BUFFER_BOUNDS_CHECKS |
            D3D12_DESCRIPTOR_RANGE_FLAG_DATA_STATIC;
        std::array<D3D12_ROOT_PARAMETER1, 3> parameters = {};
        parameters[0].ShaderVisibility = D3D12_SHADER_VISIBILITY_ALL;
        parameters[1].ParameterType = D3D12_ROOT_PARAMETER_TYPE_UAV;
        parameters[1].Descriptor.Flags = D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE;
        parameters[2].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
        parameters[2].Constants.ShaderRegister = 1;
        if (kind == 0) {
            parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS;
            parameters[0].Constants.Num32BitValues = 62;
        } else if (kind == 1) {
            parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_CBV;
            parameters[0].Descriptor.Flags = D3D12_ROOT_DESCRIPTOR_FLAG_DATA_STATIC_WHILE_SET_AT_EXECUTE;
            parameters[2].Constants.Num32BitValues = 60;
        } else {
            parameters[0].ParameterType = D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE;
            parameters[0].DescriptorTable.NumDescriptorRanges = 1;
            parameters[0].DescriptorTable.pDescriptorRanges = &range;
            parameters[2].Constants.Num32BitValues = 61;
        }
        D3D12_VERSIONED_ROOT_SIGNATURE_DESC desc = {}; desc.Version = D3D_ROOT_SIGNATURE_VERSION_1_1;
        desc.Desc_1_1.NumParameters = kind == 0 ? 2 : 3;
        desc.Desc_1_1.pParameters = parameters.data();
        ComPtr<ID3DBlob> blob, errors;
        check(D3D12SerializeVersionedRootSignature(&desc, &blob, &errors), "serialize root 1.1");
        ComPtr<ID3D12RootSignature> root;
        check(c.device->CreateRootSignature(0, blob->GetBufferPointer(), blob->GetBufferSize(),
            IID_PPV_ARGS(&root)), "create 64 DWORD root");
        D3D12_COMPUTE_PIPELINE_STATE_DESC pd = {}; pd.pRootSignature = root.Get();
        pd.CS = {shader.data(), shader.size()};
        ComPtr<ID3D12PipelineState> pso;
        check(c.device->CreateComputePipelineState(&pd, IID_PPV_ARGS(&pso)), "root pipeline");
        for (UINT phase = 0; phase < 3; ++phase) {
            std::array<UINT, 64> values = {};
            for (UINT i = 0; i < values.size(); ++i) values[i] = 101 * (kind + 1) + 13 * phase + 7 * i;
            void *mapped = nullptr; D3D12_RANGE empty = {};
            check(source->Map(0, &empty, &mapped), "source Map");
            std::memcpy(mapped, values.data(), 256); source->Unmap(0, nullptr);
            if (phase == 1) c.list->ClearState(pso.Get());
            else c.list->SetPipelineState(pso.Get());
            c.list->SetComputeRootSignature(root.Get());
            ID3D12DescriptorHeap *heaps[] = {heap.Get()}; c.list->SetDescriptorHeaps(1, heaps);
            c.list->SetComputeRootUnorderedAccessView(1, output->GetGPUVirtualAddress());
            if (kind == 0) {
                c.list->SetComputeRoot32BitConstants(0, 62, values.data(), 0);
                // Independently exercise a partial update at the final DWORD.
                c.list->SetComputeRoot32BitConstant(0, values[61], 61);
            } else {
                c.list->SetComputeRoot32BitConstants(2, parameters[2].Constants.Num32BitValues, values.data(), 0);
                if (kind == 1) c.list->SetComputeRootConstantBufferView(0, source->GetGPUVirtualAddress());
                else c.list->SetComputeRootDescriptorTable(0, heap->GetGPUDescriptorHandleForHeapStart());
            }
            c.list->Dispatch(1, 1, 1);
            transition(c, output.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COPY_SOURCE);
            c.list->CopyBufferRegion(readback.Get(), 0, output.Get(), 0, 16);
            transition(c, output.Get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            check(c.list->Close(), "root Close");
            // Closed-list replay with unchanged resources is legal even for DATA_STATIC.
            c.execute_closed(); c.execute_closed();
            D3D12_RANGE read = {0, 16}; check(readback->Map(0, &read, &mapped), "readback Map");
            const UINT expected[] = {values[0], values[31], values[61], 0x37e25a91};
            for (UINT i = 0; i < 4; ++i) {
                const UINT actual = static_cast<const UINT *>(mapped)[i];
                std::printf("READ,%u,%u,%u,%u,%u\n", kind, phase, i, actual, expected[i]);
                require(actual == expected[i], "root GPU readback mismatch");
            }
            readback->Unmap(0, &empty);
            std::printf("PASS,root-case,kind=%u,phase=%u,cost=64,closed-executions=2\n", kind, phase);
            ++cases;
            c.reset();
        }
    }
    require(cases == 12, "missing root cases");
    std::puts("PASS,root-signature,12 cases,48 words");
}

int wmain(int argc, wchar_t **argv) {
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    try {
        DWORD session = 0;
        require(ProcessIdToSessionId(GetCurrentProcessId(), &session) && session != 0, "interactive session required");
        std::printf("SESSION,%lu,PID,%lu\n", session, GetCurrentProcessId());
        if (argc == 2 && !std::wcscmp(argv[1], L"--gbv")) {
            ComPtr<ID3D12Debug1> debug;
            check(D3D12GetDebugInterface(IID_PPV_ARGS(&debug)), "GPU validation debug interface");
            debug->EnableDebugLayer(); debug->SetEnableGPUBasedValidation(TRUE);
            std::puts("GPU_VALIDATION,enabled");
        } else require(argc == 1, "invalid arguments");
        Context c; initialize(c); run_roots(c);
        return 0;
    } catch (const std::exception &e) {
        std::fprintf(stderr, "FAIL,root-signature,%s\n", e.what()); return 1;
    }
}
