// Direct Windows DDI contract test. This does not create a native D3D12 device
// or establish GPU conformance. The runner pins the UMD's full path and SHA256.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#ifndef _NTDEF_
typedef LONG NTSTATUS, *PNTSTATUS;
#endif
#pragma warning(push)
#pragma warning(disable: 4201) // WDK anonymous unions.
#include <d3d12umddi.h>
#pragma warning(pop)
#include <climits>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <initializer_list>

static unsigned checks, failures;
static constexpr unsigned char poison = 0xa5;
static constexpr auto fl121 = D3D12DDI_3DPIPELINELEVEL_12_1;
static_assert(sizeof(D3D12DDI_3DPIPELINESUPPORT1_DATA_0081) == 8);
static_assert(offsetof(D3D12DDI_3DPIPELINESUPPORT1_DATA_0081,
        MaximumDriverSupportedFeatureLevel) == 4);
static_assert(sizeof(D3D12DDI_ADAPTERFUNCS_0109) == 8 * sizeof(void *));

static void check(bool passed, const char *what)
{
    ++checks;
    if (!passed) ++failures;
    printf("CHECK,%u,%s,%s\n", checks, passed ? "PASS" : "FAIL", what);
}

static bool untouched(const void *memory, size_t bytes)
{
    auto p = static_cast<const unsigned char *>(memory);
    for (size_t i = 0; i < bytes; ++i) if (p[i] != poison) return false;
    return true;
}

// Keep SEH isolated from functions with C++ unwinding. A protected input-member
// write is reported as a failed assertion instead of terminating the fixture.
static HRESULT guarded_caps(PFND3D12DDI_GETCAPS fn, D3D12DDI_HADAPTER adapter,
        const D3D12DDIARG_GETCAPS *args, DWORD *exception)
{
    *exception = 0;
    __try { return fn(adapter, args); }
    __except(EXCEPTION_EXECUTE_HANDLER) { *exception = GetExceptionCode(); return E_FAIL; }
}

static void feature_levels(const D3D12DDI_ADAPTERFUNCS_0109 &fn, D3D12DDI_HADAPTER adapter)
{
    struct { int limit, expected; } cases[] = {
        {D3D12DDI_3DPIPELINELEVEL_1_0_GENERIC, D3D12DDI_3DPIPELINELEVEL_1_0_GENERIC},
        {D3D12DDI_3DPIPELINELEVEL_1_0_CORE, D3D12DDI_3DPIPELINELEVEL_1_0_CORE},
        {9, D3D12DDI_3DPIPELINELEVEL_1_0_CORE}, // gap in the WDK enumeration
        {D3D12DDI_3DPIPELINELEVEL_11_0, D3D12DDI_3DPIPELINELEVEL_11_0},
        {D3D12DDI_3DPIPELINELEVEL_11_1, D3D12DDI_3DPIPELINELEVEL_11_1},
        {D3D12DDI_3DPIPELINELEVEL_12_0, D3D12DDI_3DPIPELINELEVEL_12_0},
        {fl121, fl121}, {D3D12DDI_3DPIPELINELEVEL_12_2, fl121},
        {INT_MAX, fl121}, {0, -1}, {-1, -1}, {INT_MIN, -1},
    };
    // Include deliberately unaligned storage and a newer runtime's unknown tail.
    for (const auto &c : cases) for (unsigned offset : {0u, 1u}) {
        unsigned char bytes[48]; memset(bytes, poison, sizeof(bytes));
        auto data = bytes + 8 + offset;
        memcpy(data, &c.limit, sizeof(c.limit));
        D3D12DDIARG_GETCAPS args = {D3D12DDICAPS_TYPE_0081_3DPIPELINESUPPORT1,
                nullptr, data, 24};
        auto hr = fn.pfnGetCaps(adapter, &args);
        int input, output;
        memcpy(&input, data, sizeof(input)); memcpy(&output, data + 4, sizeof(output));
        printf("LEVEL,limit=%d,offset=%u,hr=%08lx,answer=%d\n", c.limit, offset, hr, output);
        check(hr == (c.expected < 0 ? E_INVALIDARG : S_OK), "extended query HRESULT");
        check(input == c.limit, "runtime maximum preserved");
        check(c.expected < 0 ? untouched(data + 4, 4) : output == c.expected,
                "highest supported enumerant, or no output on refusal");
        check(untouched(bytes, 8 + offset) && untouched(data + 8, sizeof(bytes) - 16 - offset),
                "extended query preserves prefix, unknown tail and guards");
    }
    for (UINT size = 0; size < 8; ++size) {
        unsigned char bytes[24]; memset(bytes, poison, sizeof(bytes));
        D3D12DDIARG_GETCAPS args = {D3D12DDICAPS_TYPE_0081_3DPIPELINESUPPORT1,
                nullptr, bytes, size};
        check(fn.pfnGetCaps(adapter, &args) == E_INVALIDARG, "short extended query refused");
        check(untouched(bytes, sizeof(bytes)), "short query writes nothing");
    }
    for (UINT size : {0u, 1u, 2u, 3u, 4u, 16u}) {
        unsigned char bytes[32]; memset(bytes, poison, sizeof(bytes));
        D3D12DDIARG_GETCAPS args = {D3D12DDICAPS_TYPE_3DPIPELINESUPPORT,
                nullptr, bytes + 1, size};
        auto hr = fn.pfnGetCaps(adapter, &args);
        int level; memcpy(&level, bytes + 1, sizeof(level));
        check(hr == (size < 4 ? E_INVALIDARG : S_OK), "legacy query HRESULT");
        check(size < 4 ? untouched(bytes, sizeof(bytes)) : level == fl121,
                "legacy query ceiling and short-buffer refusal");
        check(bytes[0] == poison && untouched(bytes + 1 + size, sizeof(bytes) - 1 - size),
                "legacy query preserves guards");
    }
    SYSTEM_INFO info; GetSystemInfo(&info);
    auto pages = static_cast<unsigned char *>(VirtualAlloc(nullptr, 2 * info.dwPageSize,
            MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE));
    check(pages != nullptr, "allocate input/output protection fixture");
    if (pages) {
        auto data = pages + info.dwPageSize - 4;
        int limit = D3D12DDI_3DPIPELINELEVEL_12_2;
        memcpy(data, &limit, 4); memset(data + 4, poison, 4);
        DWORD protection, exception;
        bool protected_ok = !!VirtualProtect(pages, info.dwPageSize, PAGE_READONLY, &protection);
        check(protected_ok, "protect input member while output remains writable");
        if (protected_ok) {
            D3D12DDIARG_GETCAPS args = {D3D12DDICAPS_TYPE_0081_3DPIPELINESUPPORT1,
                    nullptr, data, 8};
            auto hr = guarded_caps(fn.pfnGetCaps, adapter, &args, &exception);
            int output; memcpy(&output, data + 4, 4);
            printf("INPUT_DIRECTION,exception=%08lx,hr=%08lx\n", exception, hr);
            check(!exception && hr == S_OK && output == fl121, "only output member is written");
        }
        VirtualFree(pages, 0, MEM_RELEASE);
    }
    check(fn.pfnGetCaps(adapter, nullptr) == E_INVALIDARG, "null caps arguments refused");
    D3D12DDIARG_GETCAPS null_data = {D3D12DDICAPS_TYPE_3DPIPELINESUPPORT, nullptr, nullptr, 4};
    check(fn.pfnGetCaps(adapter, &null_data) == E_INVALIDARG, "null caps output refused");
}

static void negotiation(const D3D12DDI_ADAPTERFUNCS_0109 &fn, D3D12DDI_HADAPTER adapter)
{
    UINT32 count = 0;
    check(fn.pfnGetSupportedVersions(adapter, &count, nullptr) == S_OK && count == 1,
            "version count query advertises one interface");
    check(fn.pfnGetSupportedVersions(adapter, nullptr, nullptr) == E_INVALIDARG,
            "null version count refused");
    for (UINT32 capacity : {0u, 1u, 3u}) {
        UINT64 versions[4]; memset(versions, poison, sizeof(versions)); count = capacity;
        auto hr = fn.pfnGetSupportedVersions(adapter, &count, versions);
        check(hr == (capacity ? S_OK : E_OUTOFMEMORY) && count == 1, "version fill count and HRESULT");
        check(capacity ? versions[0] == D3D12DDI_SUPPORTED_0110 : untouched(versions, sizeof(versions)),
                "exact DDI token, or no version write on short buffer");
        check(untouched(versions + 1, sizeof(versions) - sizeof(*versions)), "version tail preserved");
    }
    D3D12DDIARG_CALCPRIVATEDEVICESIZE size = {D3D12DDI_INTERFACE_VERSION_R8,
            D3D12DDI_BUILD_VERSION_0110 << 16, D3D12DDI_CREATE_DEVICE_FLAG_NONE};
    const auto ordinary = fn.pfnCalcPrivateDeviceSize(adapter, &size);
    check(ordinary != 0, "advertised DDI has device storage");
    size.Flags = D3D12DDI_CREATE_DEVICE_FLAG_DEBUGGABLE;
    check(fn.pfnCalcPrivateDeviceSize(adapter, &size) >= ordinary, "debug device storage covers ordinary prefix");
    for (unsigned bad : {0u, unsigned(D3D12DDI_BUILD_VERSION_0109 << 16), UINT_MAX}) {
        size.Version = bad;
        check(fn.pfnCalcPrivateDeviceSize(adapter, &size) == 0, "unadvertised version has no device storage");
    }
    size.Version = D3D12DDI_BUILD_VERSION_0110 << 16; ++size.Interface;
    check(fn.pfnCalcPrivateDeviceSize(adapter, &size) == 0, "unadvertised interface has no device storage");
    check(fn.pfnCalcPrivateDeviceSize(adapter, nullptr) == 0, "null size arguments refused");
    unsigned char requests[128]; memset(requests, poison, sizeof(requests)); count = 3;
    check(fn.pfnGetOptionalDDITables(adapter, &count,
            reinterpret_cast<D3D12DDI_TABLE_REQUEST *>(requests)) == S_OK && !count,
            "no optional DDI tables");
    check(untouched(requests, sizeof(requests)), "zero optional entries writes no array");
    check(fn.pfnGetOptionalDDITables(adapter, nullptr, nullptr) == E_INVALIDARG,
            "null optional-table count refused");
}

static void foreign_handles(const D3D12DDI_ADAPTERFUNCS_0109 &fn)
{
    for (uintptr_t address : {uintptr_t(0), uintptr_t(1), UINTPTR_MAX}) {
        D3D12DDI_HADAPTER bad = {reinterpret_cast<void *>(address)};
        UINT32 count = 3; UINT64 versions[3]; memset(versions, poison, sizeof(versions));
        check(fn.pfnGetSupportedVersions(bad, &count, versions) == E_INVALIDARG,
                "foreign handle cannot enumerate versions");
        check(count == 3 && untouched(versions, sizeof(versions)), "foreign version query writes nothing");
        unsigned char bytes[256]; memset(bytes, poison, sizeof(bytes));
        D3D12DDIARG_GETCAPS args = {D3D12DDICAPS_TYPE_3DPIPELINESUPPORT, nullptr, bytes, 4};
        check(fn.pfnGetCaps(bad, &args) == E_INVALIDARG && untouched(bytes, sizeof(bytes)),
                "foreign handle cannot read caps or alter outputs");
        count = 3;
        check(fn.pfnGetOptionalDDITables(bad, &count, nullptr) == E_INVALIDARG && count == 3,
                "foreign optional-table query writes nothing");
        check(fn.pfnFillDDITable(bad, D3D12DDI_TABLE_TYPE_COMMAND_QUEUE_3D, bytes,
                7 * sizeof(void *), 0, {}) == E_INVALIDARG && untouched(bytes, sizeof(bytes)),
                "foreign handle cannot fill a DDI table");
        D3D12DDIARG_CALCPRIVATEDEVICESIZE size = {D3D12DDI_INTERFACE_VERSION_R8,
                D3D12DDI_BUILD_VERSION_0110 << 16, D3D12DDI_CREATE_DEVICE_FLAG_NONE};
        check(!fn.pfnCalcPrivateDeviceSize(bad, &size), "foreign handle cannot size a device");
        check(fn.pfnCreateDevice(bad, nullptr) == E_INVALIDARG, "foreign device creation refused");
        check(fn.pfnCloseAdapter(bad) == E_INVALIDARG, "foreign adapter close refused");
    }
}

int wmain(int argc, wchar_t **argv)
{
    DWORD session = 0; ProcessIdToSessionId(GetCurrentProcessId(), &session);
    if (argc != 2 || !session) { fprintf(stderr, "Exact UMD path and interactive session required.\n"); return 2; }
    printf("SCOPE,direct DDI contract only,pid=%lu,session=%lu\n", GetCurrentProcessId(), session);
    auto module = LoadLibraryExW(argv[1], nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!module) { fprintf(stderr, "LoadLibrary error %lu\n", GetLastError()); return 2; }
    wchar_t path[32768]; GetModuleFileNameW(module, path, 32768);
    printf("MODULE,%ls\n", path);
    auto open = reinterpret_cast<PFND3D12DDI_OPENADAPTER>(GetProcAddress(module, "OpenAdapter12"));
    if (!open) return 2;
    struct { D3D12DDI_ADAPTERFUNCS_0109 fn; unsigned char guard[64]; } storage;
    memset(&storage, poison, sizeof(storage));
    D3D12DDIARG_OPENADAPTER adapter = {};
    adapter.pAdapterFuncs = reinterpret_cast<D3D12DDI_ADAPTERFUNCS *>(&storage.fn);
    auto hr = open(&adapter);
    check(hr == S_OK && adapter.hAdapter.pDrvPrivate, "OpenAdapter12 supplies a valid token");
    check(untouched(storage.guard, sizeof(storage.guard)), "OpenAdapter12 preserves table guard");
    if (FAILED(hr)) return 1;
    const auto &fn = storage.fn;
    // Deliberately query caps before version negotiation, as the real runtime does.
    feature_levels(fn, adapter.hAdapter);
    negotiation(fn, adapter.hAdapter);
    foreign_handles(fn);
    UINT32 count = 0;
    check(fn.pfnGetSupportedVersions(adapter.hAdapter, &count, nullptr) == S_OK && count == 1,
            "foreign-handle refusals leave valid adapter usable");
    check(fn.pfnCloseAdapter(adapter.hAdapter) == S_OK, "valid adapter close succeeds");
    printf("SUMMARY,checks=%u,failures=%u\n", checks, failures);
    FreeLibrary(module);
    return failures ? 1 : 0;
}
