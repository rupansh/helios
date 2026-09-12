// Validate D3D12 DDI table publication without creating an adapter or device.
// Exact and short buffers must be filled without touching their canaries;
// unknown tables, oversized buffers and partial pointer slots must be refused
// without writes. Exercise the remaining Blt fallback and feature negotiation
// through WDK function types so x86 stdcall cleanup is part of the check.
//
// Build from an x86 or x64 developer prompt with WDK headers available:
//   cl /nologo /EHsc /W4 tools\d3d12_fill_table_probe.cpp /Fe:filltable.exe
// Run with an explicit same-bitness driver:
//   filltable.exe path-to-helios_umd12.dll
// Without a path, the x64 probe finds the newest ProgramData hotplug driver.

#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <winternl.h> // NTSTATUS is required by d3d12umddi.h's d3dkmddi.h include.
#include <d3d12umddi.h>

#include <cstddef>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

typedef long HRESULT_T;
typedef HRESULT_T(__cdecl* PFN_FILL)(int table_type, void* table, size_t table_size, unsigned index);
typedef size_t(__cdecl* PFN_SIZE)(int table_type);

const unsigned char kPoison = 0xA5;
const size_t kSlot = sizeof(void*);

int g_steps = 0;
int g_failures = 0;

void check(bool ok, const char* what) {
    ++g_steps;
    printf("%02d %-4s %s\n", g_steps, ok ? "OK" : "FAIL", what);
    if (!ok) {
        ++g_failures;
    }
}

// Every byte in [from, to) still holds the poison pattern.
bool untouched(const std::vector<unsigned char>& buf, size_t from, size_t to) {
    for (size_t i = from; i < to; ++i) {
        if (buf[i] != kPoison) {
            printf("     ... byte %zu is 0x%02X, expected 0x%02X\n", i, buf[i], kPoison);
            return false;
        }
    }
    return true;
}

// Every pointer-sized slot in [0, slots) is non-NULL.
bool all_slots_filled(const std::vector<unsigned char>& buf, size_t slots) {
    for (size_t i = 0; i < slots; ++i) {
        void* p = nullptr;
        std::memcpy(&p, buf.data() + i * kSlot, kSlot);
        if (p == nullptr) {
            printf("     ... slot %zu is NULL\n", i);
            return false;
        }
    }
    return true;
}

std::string newest_programdata_umd12() {
    const std::string dir = "C:\\ProgramData\\HeliosUmd\\";
    WIN32_FIND_DATAA fd = {};
    HANDLE h = FindFirstFileA((dir + "helios_umd12_*.dll").c_str(), &fd);
    if (h == INVALID_HANDLE_VALUE) {
        return std::string();
    }
    std::string best;
    FILETIME best_time = {};
    do {
        if (best.empty() || CompareFileTime(&fd.ftLastWriteTime, &best_time) > 0) {
            best = dir + fd.cFileName;
            best_time = fd.ftLastWriteTime;
        }
    } while (FindNextFileA(h, &fd));
    FindClose(h);
    return best;
}

struct Table {
    int type;
    const char* name;
    size_t expect_slots;  // from DECISIONS.md section 4.1, cross-checked
};

}  // namespace

int main(int argc, char** argv) {
    if (sizeof(void*) == 4 && argc < 2) {
        printf("FAIL: x86 probe requires an explicit x86 helios_umd12 DLL path\n");
        return 2;
    }
    std::string dll = argc > 1 ? argv[1] : newest_programdata_umd12();
    if (dll.empty()) {
        printf("FAIL: no helios_umd12 DLL given and none found in C:\\ProgramData\\HeliosUmd\\\n");
        return 2;
    }
    printf("module: %s\n\n", dll.c_str());

    HMODULE m = LoadLibraryA(dll.c_str());
    if (!m) {
        printf("FAIL: LoadLibrary failed, GetLastError=%lu\n", GetLastError());
        return 2;
    }
    PFN_FILL fill = (PFN_FILL)GetProcAddress(m, "helios_umd12_probe_fill_ddi_table_v1");
    PFN_SIZE table_size = (PFN_SIZE)GetProcAddress(m, "helios_umd12_probe_ddi_table_size_v1");
    if (!fill || !table_size) {
        printf("FAIL: exports missing (fill=%p size=%p)\n", (void*)fill, (void*)table_size);
        return 2;
    }

    // DECISIONS.md section 4.1's canonical counts. The probe asks the DLL for
    // the BYTE size (so an SDK pin move cannot make this probe agree with a
    // previous header) and checks the derived slot count against these.
    const Table tables[] = {
        {0, "DEVICE_FUNCS_CORE_0109", 124},
        {1, "COMMAND_LIST_FUNCS_3D_0108", 75},
        {2, "COMMAND_QUEUE_FUNCS_CORE_0001", 7},
        {27, "EXTENDED_FEATURES_FUNCS_0096", 4},
    };

    for (const Table& t : tables) {
        printf("\n-- %s --\n", t.name);
        const size_t bytes = table_size(t.type);
        char msg[256];

        _snprintf_s(msg, sizeof(msg), _TRUNCATE, "%s: header size %zu B = %zu slots (want %zu)",
                    t.name, bytes, bytes / kSlot, t.expect_slots);
        check(bytes != 0 && bytes % kSlot == 0 && bytes / kSlot == t.expect_slots, msg);
        if (bytes == 0) {
            continue;
        }

        // --- 1. exact size: every slot filled, nothing past it -------------
        {
            const size_t guard = 128;
            std::vector<unsigned char> buf(bytes + guard, kPoison);
            HRESULT_T hr = fill(t.type, buf.data(), bytes, 0);
            _snprintf_s(msg, sizeof(msg), _TRUNCATE, "%s: fill(exact) hr=0x%08lX",
                        t.name, (unsigned long)hr);
            check(hr == 0, msg);
            _snprintf_s(msg, sizeof(msg), _TRUNCATE, "%s: exact -> all %zu slots non-NULL",
                        t.name, bytes / kSlot);
            check(all_slots_filled(buf, bytes / kSlot), msg);
            _snprintf_s(msg, sizeof(msg), _TRUNCATE,
                        "%s: exact -> %zu-byte guard band untouched", t.name, guard);
            check(untouched(buf, bytes, bytes + guard), msg);

            if (t.type == D3D12DDI_TABLE_TYPE_COMMAND_LIST_3D && hr == 0 &&
                bytes == sizeof(D3D12DDI_COMMAND_LIST_FUNCS_3D_0108)) {
                D3D12DDI_COMMAND_LIST_FUNCS_3D_0108 typed = {};
                std::memcpy(&typed, buf.data(), sizeof(typed));
                check(typed.pfnBlt != nullptr, "Blt fallback has a callable WDK signature");
                if (typed.pfnBlt) {
                    // Blt remains a counted fallback. Do not call arbitrary slots
                    // with dummy arguments: most now own real device state.
                    D3D12DDI_HCOMMANDLIST list = {};
                    D3D12DDIARG_BLT args = {};
#if defined(_M_IX86)
                    unsigned stack_before = 0, stack_after = 0;
                    __asm mov stack_before, esp
#endif
                    typed.pfnBlt(list, &args);
#if defined(_M_IX86)
                    __asm mov stack_after, esp
                    check(stack_before == stack_after,
                          "x86 Blt fallback performs stdcall stack cleanup");
#endif
                    check(untouched(buf, bytes, bytes + guard),
                          "typed Blt fallback returned with the canary intact");
                }
            }
            if (t.type == D3D12DDI_TABLE_TYPE_0096_EXTENDED_FEATURES && hr == 0 &&
                bytes == sizeof(D3D12DDI_EXTENDED_FEATURES_FUNCS_0096)) {
                D3D12DDI_EXTENDED_FEATURES_FUNCS_0096 typed = {};
                std::memcpy(&typed, buf.data(), sizeof(typed));
                const bool callable = typed.pfnGetSupportedExtendedFeatures &&
                    typed.pfnGetSupportedExtendedFeatureVersions &&
                    typed.pfnEnableExtendedFeature && typed.pfnSetExtendedFeatureCallbacks;
                check(callable, "all extended-feature callbacks are callable");
                if (!callable) {
                    continue;
                }
                D3D12DDI_HDEVICE device = {};
                UINT32 count = 123;
                const auto feature = D3D12DDI_FEATURE_0054_DOWNLEVEL_SUPPORT;
                HRESULT feature_hr = typed.pfnGetSupportedExtendedFeatures(
                    device, feature, &count, nullptr);
                check(feature_hr == S_OK && count == 0,
                      "typed feature enumeration writes an empty supported set");
                feature_hr = typed.pfnGetSupportedExtendedFeatures(
                    device, feature, nullptr, nullptr);
                check(FAILED(feature_hr), "null feature-count pointer is refused");
                count = 123;
                feature_hr = typed.pfnGetSupportedExtendedFeatureVersions(
                    device, feature, &count, nullptr);
                check(FAILED(feature_hr) && count == 0,
                      "unsupported feature has no versions and is refused");
                feature_hr = typed.pfnEnableExtendedFeature(device, feature, 1);
                check(FAILED(feature_hr), "unsupported extended feature cannot be enabled");
                feature_hr = typed.pfnSetExtendedFeatureCallbacks(
                    device, D3D12DDI_TABLE_TYPE_0054_DOWNLEVEL_SUPPORT_CALLBACKS,
                    nullptr, 0);
                check(FAILED(feature_hr), "absent extended-feature callbacks are refused");
            }
        }

        // --- 2. R702: the runtime's count is SHORTER than our struct -------
        if (bytes > kSlot) {
            const size_t asked = bytes - kSlot;
            const size_t guard = 128;
            std::vector<unsigned char> buf(bytes + guard, kPoison);
            HRESULT_T hr = fill(t.type, buf.data(), asked, 0);
            _snprintf_s(msg, sizeof(msg), _TRUNCATE, "%s: fill(short=%zu) hr=0x%08lX",
                        t.name, asked, (unsigned long)hr);
            check(hr == 0, msg);
            _snprintf_s(msg, sizeof(msg), _TRUNCATE,
                        "%s: short -> all %zu asked slots non-NULL", t.name, asked / kSlot);
            check(all_slots_filled(buf, asked / kSlot), msg);
            // THE R702 CHECK. Not one byte past the count the caller gave.
            _snprintf_s(msg, sizeof(msg), _TRUNCATE,
                        "%s: short -> bytes %zu..%zu UNTOUCHED (R702)", t.name, asked,
                        bytes + guard);
            check(untouched(buf, asked, bytes + guard), msg);
        }

        // --- 3. the runtime's count is LONGER than our struct --------------
        {
            const size_t asked = bytes + 64;
            const size_t guard = 128;
            std::vector<unsigned char> buf(asked + guard, kPoison);
            HRESULT_T hr = fill(t.type, buf.data(), asked, 0);
            _snprintf_s(msg, sizeof(msg), _TRUNCATE, "%s: fill(long=%zu) hr=0x%08lX",
                        t.name, asked, (unsigned long)hr);
            check(FAILED(hr), msg);
            _snprintf_s(msg, sizeof(msg), _TRUNCATE,
                        "%s: oversized table and %zu-byte guard entirely untouched", t.name, guard);
            check(untouched(buf, 0, buf.size()), msg);
        }
    }

    // --- 5. an unserved table type writes NOTHING --------------------------
    printf("\n-- refusals --\n");
    {
        const size_t bytes = 992;
        std::vector<unsigned char> buf(bytes, kPoison);
        // 3 is D3D12DDI_TABLE_TYPE_DXGI: a real enumerator, deliberately not
        // served (D12-G5 measured that this runtime never requests it).
        HRESULT_T hr = fill(3, buf.data(), bytes, 0);
        check(hr != 0, "unserved table type 3 (DXGI) is refused");
        check(untouched(buf, 0, bytes), "unserved table type 3 wrote NOTHING");
    }
    {
        const size_t bytes = 992;
        std::vector<unsigned char> buf(bytes, kPoison);
        HRESULT_T hr = fill(9999, buf.data(), bytes, 0);
        check(hr != 0, "unknown table type 9999 is refused");
        check(untouched(buf, 0, bytes), "unknown table type 9999 wrote NOTHING");
    }
    {
        HRESULT_T hr = fill(0, nullptr, 992, 0);
        check(hr != 0, "null table pointer is refused");
    }
    {
        std::vector<unsigned char> buf(64, kPoison);
        HRESULT_T hr = fill(0, buf.data(), 0, 0);
        check(hr != 0, "zero table size is refused");
        check(untouched(buf, 0, buf.size()), "zero table size wrote NOTHING");
    }

    {
        std::vector<unsigned char> buf(64, kPoison);
        HRESULT_T hr = fill(0, buf.data(), kSlot + 1, 0);
        check(FAILED(hr), "partial function-pointer slot is refused");
        check(untouched(buf, 0, buf.size()), "partial-slot request wrote NOTHING");
    }

    printf("\n%d steps, %d failures\n", g_steps, g_failures);
    // Deliberately NOT FreeLibrary: the DLL's process-lifetime log handle is
    // released in DllMain(DLL_PROCESS_DETACH), and letting process exit do it
    // keeps the log file open for the lines the checks above produced.
    return g_failures == 0 ? 0 : 1;
}
