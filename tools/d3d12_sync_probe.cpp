// Native D3D12/runtime fence acceptance. Run in the interactive guest session.
// Build: cl /EHsc /std:c++17 d3d12_sync_probe.cpp d3d12.lib dxgi.lib
// Each case requires both fence ordering and an exact GPU readback pattern.
#include <windows.h>
#include <d3d12.h>
#include <dxgi1_6.h>
#include <wrl/client.h>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

using Microsoft::WRL::ComPtr;
static constexpr UINT words = 65536;
static constexpr UINT bytes = words * sizeof(uint32_t);
static constexpr uint32_t unwritten = 0xa5a5a5a5u;

static void check(HRESULT hr, const char *what) {
    if (FAILED(hr)) { std::fprintf(stderr, "FAIL %s hr=%08lx\n", what, hr); std::exit(1); }
}
struct Event {
    HANDLE handle = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    Event() { if (!handle) std::exit(2); }
    ~Event() { CloseHandle(handle); }
    void wait() { if (WaitForSingleObject(handle, 10000) != WAIT_OBJECT_0) {
        std::fprintf(stderr, "FAIL completion missing; timeout is not completion\n"); std::exit(1);
    } }
};

static ComPtr<ID3D12Device> device() {
    ComPtr<IDXGIFactory4> factory;
    check(CreateDXGIFactory2(0, IID_PPV_ARGS(&factory)), "factory");
    ComPtr<IDXGIAdapter1> adapter;
    for (UINT i = 0; factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) {
        DXGI_ADAPTER_DESC1 desc{};
        check(adapter->GetDesc1(&desc), "adapter description");
        if (desc.VendorId == 0x1af4 && desc.DeviceId == 0x1050) {
            ComPtr<ID3D12Device> result;
            check(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&result)), "Helios device");
            std::printf("adapter LUID=%08lx:%08lx\n", desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart);
            return result;
        }
        adapter.Reset();
    }
    std::fprintf(stderr, "FAIL exact Helios PCI adapter absent\n"); std::exit(1);
}
static ComPtr<ID3D12Resource> buffer(ID3D12Device *dev, D3D12_HEAP_TYPE heap, D3D12_RESOURCE_STATES state) {
    D3D12_HEAP_PROPERTIES props{}; props.Type = heap;
    props.CreationNodeMask = props.VisibleNodeMask = 1;
    D3D12_RESOURCE_DESC desc{};
    desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER; desc.Width = bytes;
    desc.Height = desc.DepthOrArraySize = desc.MipLevels = 1;
    desc.SampleDesc.Count = 1; desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    ComPtr<ID3D12Resource> result;
    check(dev->CreateCommittedResource(&props, D3D12_HEAP_FLAG_NONE, &desc, state, nullptr,
        IID_PPV_ARGS(&result)), "buffer");
    return result;
}
struct Queue {
    ComPtr<ID3D12CommandQueue> queue;
    ComPtr<ID3D12CommandAllocator> allocator;
    ComPtr<ID3D12GraphicsCommandList> list;
    Queue(ID3D12Device *dev, D3D12_COMMAND_LIST_TYPE type) {
        D3D12_COMMAND_QUEUE_DESC desc{}; desc.Type = type;
        check(dev->CreateCommandQueue(&desc, IID_PPV_ARGS(&queue)), "queue");
        check(dev->CreateCommandAllocator(type, IID_PPV_ARGS(&allocator)), "allocator");
        check(dev->CreateCommandList(0, type, allocator.Get(), nullptr, IID_PPV_ARGS(&list)), "list");
    }
    void submit() {
        check(list->Close(), "close list");
        ID3D12CommandList *lists[] = {list.Get()};
        queue->ExecuteCommandLists(1, lists);
    }
};
static ComPtr<ID3D12Fence> fence(ID3D12Device *dev, UINT64 value = 0,
    D3D12_FENCE_FLAGS flags = D3D12_FENCE_FLAG_NONE) {
    ComPtr<ID3D12Fence> result;
    check(dev->CreateFence(value, flags, IID_PPV_ARGS(&result)), "fence");
    return result;
}
static uint32_t pattern(UINT epoch, UINT index) { return 0x9e3779b9u * (epoch + 1) ^ index; }

static void run_case(UINT epoch, bool producer_first, bool cpu_gate, bool shared_gate) {
    std::printf("BEGIN epoch=%u producer_first=%u cpu=%u cross_process=%u\n",
        epoch, producer_first, cpu_gate, shared_gate);
    std::fflush(stdout);
    auto dev = device();
    auto upload = buffer(dev.Get(), D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ);
    auto src = buffer(dev.Get(), D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COPY_DEST);
    auto readback = buffer(dev.Get(), D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    auto producer_readback = buffer(dev.Get(), D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST);
    ID3D12Resource *observed[] = {producer_readback.Get(), readback.Get()};
    const char *observed_names[] = {"producer", "consumer"};
    void *data = nullptr;
    D3D12_RANGE empty{0, 0};
    check(upload->Map(0, &empty, &data), "map upload");
    for (UINT i = 0; i < words; ++i) static_cast<uint32_t *>(data)[i] = pattern(epoch, i);
    upload->Unmap(0, nullptr);
    for (auto *resource : observed) {
        check(resource->Map(0, &empty, &data), "initialize readback sentinel");
        for (UINT i = 0; i < words; ++i) static_cast<uint32_t *>(data)[i] = unwritten;
        resource->Unmap(0, nullptr);
    }
    Queue producer(dev.Get(), D3D12_COMMAND_LIST_TYPE_DIRECT);
    Queue consumer(dev.Get(), D3D12_COMMAND_LIST_TYPE_COMPUTE);
    auto ready = fence(dev.Get(), 0);
    auto done = fence(dev.Get(), 0);
    auto gate = fence(dev.Get(), 7, shared_gate ? D3D12_FENCE_FLAG_SHARED : D3D12_FENCE_FLAG_NONE);
    const UINT64 value = 10 + epoch;
    // Independent producer witness: a consumer wait can hide an early producer
    // write, so observe both queues before releasing the CPU/shared gate.
    producer.list->CopyBufferRegion(producer_readback.Get(), 0, upload.Get(), 0, bytes);
    producer.list->CopyBufferRegion(src.Get(), 0, upload.Get(), 0, bytes);
    D3D12_RESOURCE_BARRIER barrier{};
    barrier.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
    barrier.Transition.pResource = src.Get();
    barrier.Transition.Subresource = D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
    barrier.Transition.StateBefore = D3D12_RESOURCE_STATE_COPY_DEST;
    barrier.Transition.StateAfter = D3D12_RESOURCE_STATE_COPY_SOURCE;
    producer.list->ResourceBarrier(1, &barrier);
    consumer.list->CopyBufferRegion(readback.Get(), 0, src.Get(), 0, bytes);

    // Exercise a CPU initial value, then a rewind before queuing a future wait.
    check(producer.queue->Wait(gate.Get(), 7), "initial CPU value wait");
    if (cpu_gate || shared_gate) {
        check(gate->Signal(0), "CPU fence rewind");
        check(producer.queue->Wait(gate.Get(), value), "future CPU/shared wait");
    }
    Event finished;
    check(done->SetEventOnCompletion(value, finished.handle), "completion event");
    auto produce = [&]() { producer.submit(); check(producer.queue->Signal(ready.Get(), value), "producer signal"); };
    if (producer_first) produce();
    check(consumer.queue->Wait(ready.Get(), value), "cross queue wait");
    consumer.submit();
    check(consumer.queue->Signal(done.Get(), value), "consumer signal");

    if (!producer_first || cpu_gate || shared_gate) {
        // Deliberate negative interval: this is testing absence of completion,
        // not treating a timeout as successful GPU completion.
        if (WaitForSingleObject(finished.handle, 100) != WAIT_TIMEOUT) {
            std::fprintf(stderr, "FAIL epoch %u completed through an unsignaled wait\n", epoch); std::exit(1);
        }
        // An unchanged fence alone is insufficient: the engine could execute
        // early while Windows withholds only its completion notification.
        // The unsignaled queue wait must protect the GPU write itself. This CPU
        // read is ordered before that write by the wait being tested.
        D3D12_RANGE pending_range{0, bytes};
        for (UINT which = 0; which < 2; ++which) {
            check(observed[which]->Map(0, &pending_range, &data), "read pending sentinel");
            for (UINT i = 0; i < words; ++i) if (static_cast<uint32_t *>(data)[i] != unwritten) {
                std::fprintf(stderr, "FAIL epoch %u %s GPU wrote through unsignaled wait: word %u actual=%08x\n",
                    epoch, observed_names[which], i, static_cast<uint32_t *>(data)[i]); std::exit(1);
            }
            observed[which]->Unmap(0, &empty);
        }
    }
    if (!producer_first) produce();
    if (shared_gate) {
        SECURITY_ATTRIBUTES attrs{sizeof(attrs), nullptr, TRUE};
        HANDLE shared = nullptr;
        check(dev->CreateSharedHandle(gate.Get(), &attrs, GENERIC_ALL, nullptr, &shared), "shared fence handle");
        wchar_t executable[MAX_PATH];
        if (!GetModuleFileNameW(nullptr, executable, MAX_PATH)) std::exit(2);
        std::wstring command = L"\"" + std::wstring(executable) + L"\" --signal "
            + std::to_wstring(reinterpret_cast<uintptr_t>(shared)) + L" " + std::to_wstring(value);
        STARTUPINFOW startup{}; startup.cb = sizeof(startup);
        PROCESS_INFORMATION process{};
        if (!CreateProcessW(executable, command.data(), nullptr, nullptr, TRUE, 0, nullptr, nullptr, &startup, &process)) std::exit(2);
        if (WaitForSingleObject(process.hProcess, 10000) != WAIT_OBJECT_0) {
            TerminateProcess(process.hProcess, 1); std::exit(1);
        }
        DWORD code = 1; GetExitCodeProcess(process.hProcess, &code);
        CloseHandle(process.hThread); CloseHandle(process.hProcess); CloseHandle(shared);
        if (code != 0) { std::fprintf(stderr, "FAIL shared fence child\n"); std::exit(1); }
    } else if (cpu_gate) {
        check(gate->Signal(value), "CPU release");
    }
    finished.wait();
    D3D12_RANGE range{0, bytes};
    for (UINT which = 0; which < 2; ++which) {
        check(observed[which]->Map(0, &range, &data), "readback");
        for (UINT i = 0; i < words; ++i) if (static_cast<uint32_t *>(data)[i] != pattern(epoch, i)) {
            std::fprintf(stderr, "FAIL epoch %u %s word %u actual=%08x expected=%08x\n", epoch,
                observed_names[which], i, static_cast<uint32_t *>(data)[i], pattern(epoch, i)); std::exit(1);
        }
        observed[which]->Unmap(0, &empty);
    }
    check(dev->GetDeviceRemovedReason(), "device health");
    std::printf("PASS epoch=%u producer_first=%u cpu=%u cross_process=%u words=%u\n",
        epoch, producer_first, cpu_gate, shared_gate, words);
}

int wmain(int argc, wchar_t **argv) {
    if (argc == 4 && std::wstring(argv[1]) == L"--signal") {
        auto dev = device();
        HANDLE shared = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(_wcstoui64(argv[2], nullptr, 10)));
        ComPtr<ID3D12Fence> target;
        check(dev->OpenSharedHandle(shared, IID_PPV_ARGS(&target)), "child open exact fence handle");
        check(target->Signal(_wcstoui64(argv[3], nullptr, 10)), "child CPU signal");
        CloseHandle(shared);
        return 0;
    }
    if (argc == 3 && std::wstring(argv[1]) == L"--case") {
        wchar_t *end = nullptr;
        const auto selected = std::wcstoul(argv[2], &end, 10);
        if (!end || *end || selected < 1 || selected > 4) return 2;
        const auto epoch = static_cast<UINT>(selected);
        run_case(epoch, epoch != 2, epoch == 3, epoch == 4);
        std::printf("PASS selected native runtime synchronization case %u\n", epoch);
        return 0;
    }
    if (argc != 1) return 2;
    run_case(1, true, false, false);
    run_case(2, false, false, false);
    run_case(3, true, true, false);
    run_case(4, true, false, true);
    std::puts("PASS all native runtime synchronization cases");
    return 0;
}
