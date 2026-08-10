// d3d12_shared_fence_probe.cpp — is an `ID3D12Fence` shared handle a WDDM sync
// object the Helios ICD can open, and if not, which component made it?
//
// WHY THIS EXISTS. `VK_LAYER_HELIOS_present` builds a WSI device, imports four
// D3D12 committed textures, and then dies on the Ready/Release fence:
//
//     [helios-wsi] REFUSE swapchain_refused_semaphore_import (-3)
//     sync_open_nt failed nt2_status=0xc000000d legacy_status=0xc000000d
//                  handle=00000000000003a8 dev=0x40000600
//
// `FINDINGS.md` F7 addendum 3 proves one half: the ICD created no WDDM sync
// object in that process, so the handle from `ID3D12Fence::CreateSharedHandle`
// is not one of ours. It does NOT prove why the open fails, and four different
// causes produce the identical `STATUS_INVALID_PARAMETER`:
//
//   (a) the handle is not a synchronization object at all;
//   (b) it is one, but of a sharing class `...FromNtHandle2` rejects;
//   (c) the `hDevice` is on the wrong adapter, or the wrong kind of device;
//   (d) the granted access is insufficient.
//
// The standing hypothesis — `ID3D12Fence` is a *runtime* object over a dxgkrnl
// monitored fence, so vkd3d's `d3d12_shared_fence` path is app-local-vkd3d-only
// — is one reading of (a)/(b). ⛔ It is to be MEASURED, not assumed, and the
// measurement must not be able to blame Helios for something D3D12 does
// everywhere. Hence the controls.
//
// WHAT IT MEASURES, per adapter:
//
//   arm 1  a real `ID3D12Fence(D3D12_FENCE_FLAG_SHARED)` -> `CreateSharedHandle`
//          * the NT object TYPE NAME of the handle (this is the "which
//            component created it" question, answered by the kernel);
//          * `ID3D12Device::OpenSharedHandle` round-trip, which proves the
//            handle is valid and functional whatever the KMT layer thinks;
//          * `D3DKMTOpenSyncObjectFromNtHandle2` with Flags=0 and with
//            NtSecuritySharing=1, then the legacy `...FromNtHandle`;
//          * `D3DKMTQueryResourceInfoFromNtHandle`, which says "it is a
//            resource, not a sync object" if that is what is going on.
//
//   arm 2  CONTROL — a sync object this process demonstrably owns:
//          `D3DKMTCreateSynchronizationObject2(MONITORED, Shared,
//          NtSecuritySharing)` -> `D3DKMTShareObjects`, run through the exact
//          same open calls, from the same device and from a second device on
//          the same adapter. If arm 2 opens and arm 1 does not, the difference
//          is the OBJECT, not the call, the process, or the adapter.
//
// and arm 1 runs on EVERY adapter present, so a failure that also happens on
// the Microsoft/WARP adapter is generic D3D12 architecture, not a Helios
// defect. That distinction is the whole point: it decides whether the fix
// belongs in `icd/mesa`, in the layer's design, or nowhere.
//
// It opens no window and needs no desktop, so it is safe from session 0.
//
// ⭐ MEASURED ANSWER (2026-08-10, 22.22.263.0) — `docs/retirement/FINDINGS.md` F8.
// Arm 1 fails identically on ALL THREE adapters, including both Microsoft Basic
// Render Driver ones, while arm 2 succeeds on all three: an `ID3D12Fence` shared
// handle is not openable through any public D3DKMT entry point, and that is not
// a Helios defect. The last control is the useful one — `ID3D12Device::Open-
// SharedHandle` DOES accept a monitored fence we created and shared, so the
// sharing direction has to be reversed: the ICD creates and exports, the D3D12
// side opens. Re-run this after K7 + the `SURFACE` flip if the native-fence row
// starts to matter; today no adapter here advertises native fences, so that row
// measures nothing.
//
// Build (on the VM, from an x64 developer prompt):
//   cl /nologo /EHsc /W4 /std:c++17 Z:\tools\d3d12_shared_fence_probe.cpp
//      /I"Z:\icd\win-build\wdk-include"
//      /Fe:C:\Users\Rupansh\helios-probe\d3d12_shared_fence_probe.exe
//      /link d3d12.lib dxgi.lib dxguid.lib gdi32.lib
//
// Exit codes: 0 = the probe ran (read the table), 1 = could not even set up.

// ⚠ No WIN32_LEAN_AND_MEAN here. With it, `d3dkmthk.h` fails to parse (NTSTATUS
// undeclared, ~100 C2059s starting in the PFND3DKMT_* typedef block). The
// working `d3dkmt_sync_probe.cpp` includes a full `windows.h`; match it.
#include <windows.h>
// For OBJECT_ATTRIBUTES: `d3dkmthk.h` only forward-declares `_OBJECT_ATTRIBUTES`
// in its D3DKMTShareObjects prototype, so the definition has to come from here.
#include <winternl.h>
#include <d3dkmthk.h>
#include <dxgi1_6.h>
#include <d3d12.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

// ---- ntdll: the object-type query ---------------------------------------
// NtQueryObject/ObjectTypeInformation on a handle this process owns needs no
// privilege beyond the process itself. The type NAME is the kernel's own
// answer to "what made this", and it cannot be argued with.

constexpr ULONG kObjectTypeInformation = 2;
constexpr ULONG kObjectBasicInformation = 0;

typedef struct _PROBE_UNICODE_STRING {
    USHORT Length;
    USHORT MaximumLength;
    PWSTR  Buffer;
} PROBE_UNICODE_STRING;

typedef struct _PROBE_OBJECT_BASIC_INFORMATION {
    ULONG  Attributes;
    ACCESS_MASK GrantedAccess;
    ULONG  HandleCount;
    ULONG  PointerCount;
    ULONG  Reserved[10];
} PROBE_OBJECT_BASIC_INFORMATION;

using PfnNtQueryObject = LONG(NTAPI*)(HANDLE, ULONG, PVOID, ULONG, PULONG);
PfnNtQueryObject g_queryObject = nullptr;

bool load_ntdll() {
    HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
    if (!ntdll)
        return false;
    g_queryObject = reinterpret_cast<PfnNtQueryObject>(GetProcAddress(ntdll, "NtQueryObject"));
    return g_queryObject != nullptr;
}

std::wstring handle_type_name(HANDLE h) {
    if (!g_queryObject)
        return L"(NtQueryObject unavailable)";
    alignas(16) unsigned char buf[2048] = {};
    ULONG needed = 0;
    const LONG st = g_queryObject(h, kObjectTypeInformation, buf, sizeof(buf), &needed);
    if (st < 0) {
        wchar_t tmp[64];
        swprintf(tmp, 64, L"(query failed 0x%08lX)", static_cast<unsigned long>(st));
        return tmp;
    }
    // PUBLIC_OBJECT_TYPE_INFORMATION begins with the TypeName UNICODE_STRING.
    const auto* us = reinterpret_cast<const PROBE_UNICODE_STRING*>(buf);
    if (!us->Buffer || us->Length == 0)
        return L"(unnamed type)";
    return std::wstring(us->Buffer, us->Length / sizeof(wchar_t));
}

ACCESS_MASK handle_granted_access(HANDLE h) {
    if (!g_queryObject)
        return 0;
    PROBE_OBJECT_BASIC_INFORMATION bi = {};
    ULONG needed = 0;
    if (g_queryObject(h, kObjectBasicInformation, &bi, sizeof(bi), &needed) < 0)
        return 0;
    return bi.GrantedAccess;
}

void describe_handle(const char* what, HANDLE h) {
    const std::wstring type = handle_type_name(h);
    printf("      %-22s handle=%p  type=%ls  granted=0x%08lX\n", what, h, type.c_str(),
           static_cast<unsigned long>(handle_granted_access(h)));
}

// ---- D3DKMT surface ------------------------------------------------------
// Resolved dynamically so a missing export names itself rather than failing to
// load the whole probe.

using PfnOpenAdapterFromLuid = NTSTATUS(APIENTRY*)(D3DKMT_OPENADAPTERFROMLUID*);
using PfnCreateDevice = NTSTATUS(APIENTRY*)(D3DKMT_CREATEDEVICE*);
using PfnDestroyDevice = NTSTATUS(APIENTRY*)(const D3DKMT_DESTROYDEVICE*);
using PfnCloseAdapter = NTSTATUS(APIENTRY*)(const D3DKMT_CLOSEADAPTER*);
using PfnOpenSync2 = NTSTATUS(APIENTRY*)(D3DKMT_OPENSYNCOBJECTFROMNTHANDLE2*);
using PfnOpenSync1 = NTSTATUS(APIENTRY*)(D3DKMT_OPENSYNCOBJECTFROMNTHANDLE*);
using PfnCreateSync2 = NTSTATUS(APIENTRY*)(D3DKMT_CREATESYNCHRONIZATIONOBJECT2*);
using PfnDestroySync = NTSTATUS(APIENTRY*)(const D3DKMT_DESTROYSYNCHRONIZATIONOBJECT*);
using PfnShareObjects = NTSTATUS(APIENTRY*)(UINT, const D3DKMT_HANDLE*, POBJECT_ATTRIBUTES, DWORD, HANDLE*);
using PfnQueryResInfoNt = NTSTATUS(APIENTRY*)(D3DKMT_QUERYRESOURCEINFOFROMNTHANDLE*);
using PfnOpenNativeFenceNt = NTSTATUS(APIENTRY*)(D3DKMT_OPENNATIVEFENCEFROMNTHANDLE*);

struct Kmt {
    PfnOpenAdapterFromLuid open_adapter_from_luid = nullptr;
    PfnCreateDevice        create_device = nullptr;
    PfnDestroyDevice       destroy_device = nullptr;
    PfnCloseAdapter        close_adapter = nullptr;
    PfnOpenSync2           open_sync2 = nullptr;
    PfnOpenSync1           open_sync1 = nullptr;
    PfnCreateSync2         create_sync2 = nullptr;
    PfnDestroySync         destroy_sync = nullptr;
    PfnShareObjects        share_objects = nullptr;
    PfnQueryResInfoNt      query_res_info_nt = nullptr;
    PfnOpenNativeFenceNt   open_native_fence_nt = nullptr;
    bool complete = false;
};

Kmt load_kmt() {
    Kmt k;
    HMODULE g = LoadLibraryW(L"gdi32.dll");
    if (!g) {
        printf("  gdi32.dll not loadable\n");
        return k;
    }
#define GET(field, name)                                                        \
    k.field = reinterpret_cast<decltype(k.field)>(GetProcAddress(g, name));     \
    if (!k.field)                                                               \
        printf("  MISSING export: %s\n", name);
    GET(open_adapter_from_luid, "D3DKMTOpenAdapterFromLuid")
    GET(create_device, "D3DKMTCreateDevice")
    GET(destroy_device, "D3DKMTDestroyDevice")
    GET(close_adapter, "D3DKMTCloseAdapter")
    GET(open_sync2, "D3DKMTOpenSyncObjectFromNtHandle2")
    GET(open_sync1, "D3DKMTOpenSyncObjectFromNtHandle")
    GET(create_sync2, "D3DKMTCreateSynchronizationObject2")
    GET(destroy_sync, "D3DKMTDestroySynchronizationObject")
    GET(share_objects, "D3DKMTShareObjects")
    GET(query_res_info_nt, "D3DKMTQueryResourceInfoFromNtHandle")
    GET(open_native_fence_nt, "D3DKMTOpenNativeFenceFromNtHandle")
#undef GET
    k.complete = k.open_adapter_from_luid && k.create_device && k.destroy_device &&
                 k.close_adapter && k.open_sync2 && k.open_sync1 && k.create_sync2 &&
                 k.destroy_sync && k.share_objects;
    return k;
}

const char* nt_name(NTSTATUS st) {
    switch (static_cast<unsigned long>(st)) {
    case 0x00000000: return "STATUS_SUCCESS";
    case 0xC000000D: return "STATUS_INVALID_PARAMETER";
    case 0xC0000008: return "STATUS_INVALID_HANDLE";
    case 0xC0000022: return "STATUS_ACCESS_DENIED";
    case 0xC000009A: return "STATUS_INSUFFICIENT_RESOURCES";
    case 0xC00000BB: return "STATUS_NOT_SUPPORTED";
    case 0xC0000024: return "STATUS_OBJECT_TYPE_MISMATCH";
    case 0xC0000023: return "STATUS_BUFFER_TOO_SMALL";
    case 0xC0000225: return "STATUS_NOT_FOUND";
    default: return "?";
    }
}

// ---- one open attempt, printed the same way every time -------------------

void try_open2(const Kmt& k, const char* label, HANDLE nt, D3DKMT_HANDLE hDevice,
               bool nt_security_sharing) {
    D3DKMT_OPENSYNCOBJECTFROMNTHANDLE2 o = {};
    o.hNtHandle = nt;
    o.hDevice = hDevice;
    if (nt_security_sharing)
        o.Flags.NtSecuritySharing = 1;
    const NTSTATUS st = k.open_sync2(&o);
    printf("      open2 %-28s dev=0x%08X flags=%s -> 0x%08lX %s",
           label, hDevice, nt_security_sharing ? "NtSecuritySharing" : "0",
           static_cast<unsigned long>(st), nt_name(st));
    if (st == 0) {
        printf("   local=0x%08X cpu_va=%p gpu_va=0x%llX", o.hSyncObject,
               o.MonitoredFence.FenceValueCPUVirtualAddress,
               static_cast<unsigned long long>(o.MonitoredFence.FenceValueGPUVirtualAddress));
        D3DKMT_DESTROYSYNCHRONIZATIONOBJECT d = {};
        d.hSyncObject = o.hSyncObject;
        if (k.destroy_sync)
            (void)k.destroy_sync(&d);
    }
    printf("\n");
}

void try_open1(const Kmt& k, const char* label, HANDLE nt) {
    D3DKMT_OPENSYNCOBJECTFROMNTHANDLE o = {};
    o.hNtHandle = nt;
    const NTSTATUS st = k.open_sync1(&o);
    printf("      open1 %-28s (no device)              -> 0x%08lX %s",
           label, static_cast<unsigned long>(st), nt_name(st));
    if (st == 0) {
        printf("   local=0x%08X", o.hSyncObject);
        D3DKMT_DESTROYSYNCHRONIZATIONOBJECT d = {};
        d.hSyncObject = o.hSyncObject;
        if (k.destroy_sync)
            (void)k.destroy_sync(&d);
    }
    printf("\n");
}

// WDDM 3.1's native-fence open. This is the row that decides what the result
// MEANS: if `...FromNtHandle2` refuses an ID3D12Fence handle everywhere, the
// remaining question is whether the runtime hands such fences out through the
// native-fence path instead — which is exactly the surface `lane-kmd-core.md`
// K7 adds and which Helios does not advertise today (SURFACE = Wddm2_1GpuMmu,
// NATIVE_FENCE_ADVERTISED off). A "the layer's design is wrong" verdict and a
// "the layer needs K7" verdict are different work, and only this call separates
// them.
void try_open_native_fence(const Kmt& k, const char* label, HANDLE nt, D3DKMT_HANDLE hDevice) {
    if (!k.open_native_fence_nt) {
        printf("      nativefn  (D3DKMTOpenNativeFenceFromNtHandle export missing)\n");
        return;
    }
    D3DKMT_OPENNATIVEFENCEFROMNTHANDLE o = {};
    o.hNtHandle = nt;
    o.hDevice = hDevice;
    const NTSTATUS st = k.open_native_fence_nt(&o);
    printf("      nativefn %-27s dev=0x%08X            -> 0x%08lX %s",
           label, hDevice, static_cast<unsigned long>(st), nt_name(st));
    if (st == 0) {
        printf("   local=0x%08X", o.hSyncObject);
        D3DKMT_DESTROYSYNCHRONIZATIONOBJECT d = {};
        d.hSyncObject = o.hSyncObject;
        if (k.destroy_sync)
            (void)k.destroy_sync(&d);
    }
    printf("\n");
}

void try_query_resource(const Kmt& k, HANDLE nt) {
    if (!k.query_res_info_nt) {
        printf("      queryres  (export missing)\n");
        return;
    }
    D3DKMT_QUERYRESOURCEINFOFROMNTHANDLE q = {};
    q.hNtHandle = nt;
    const NTSTATUS st = k.query_res_info_nt(&q);
    printf("      queryres  D3DKMTQueryResourceInfoFromNtHandle       -> 0x%08lX %s\n",
           static_cast<unsigned long>(st), nt_name(st));
}

// ---- the per-adapter run -------------------------------------------------

struct AdapterRec {
    LUID luid;
    std::wstring desc;
    UINT flags;
    IDXGIAdapter1* adapter;
};

// Returns hAdapter/hDevice for the LUID, or zeros. Prints its own failures.
struct KmtDevice {
    D3DKMT_HANDLE hAdapter = 0;
    D3DKMT_HANDLE hDevice = 0;
};

KmtDevice kmt_open(const Kmt& k, LUID luid, const char* label) {
    KmtDevice out;
    D3DKMT_OPENADAPTERFROMLUID oa = {};
    oa.AdapterLuid = luid;
    NTSTATUS st = k.open_adapter_from_luid(&oa);
    if (st != 0) {
        printf("      [%s] D3DKMTOpenAdapterFromLuid -> 0x%08lX %s\n", label,
               static_cast<unsigned long>(st), nt_name(st));
        return out;
    }
    out.hAdapter = oa.hAdapter;

    D3DKMT_CREATEDEVICE cd = {};
    cd.hAdapter = oa.hAdapter;
    st = k.create_device(&cd);
    if (st != 0) {
        printf("      [%s] D3DKMTCreateDevice -> 0x%08lX %s\n", label,
               static_cast<unsigned long>(st), nt_name(st));
        D3DKMT_CLOSEADAPTER ca = {};
        ca.hAdapter = out.hAdapter;
        (void)k.close_adapter(&ca);
        out.hAdapter = 0;
        return out;
    }
    out.hDevice = cd.hDevice;
    printf("      [%s] hAdapter=0x%08X hDevice=0x%08X\n", label, out.hAdapter, out.hDevice);
    return out;
}

void kmt_close(const Kmt& k, KmtDevice& d) {
    if (d.hDevice) {
        D3DKMT_DESTROYDEVICE dd = {};
        dd.hDevice = d.hDevice;
        (void)k.destroy_device(&dd);
        d.hDevice = 0;
    }
    if (d.hAdapter) {
        D3DKMT_CLOSEADAPTER ca = {};
        ca.hAdapter = d.hAdapter;
        (void)k.close_adapter(&ca);
        d.hAdapter = 0;
    }
}

// arm 2 — a sync object this process demonstrably owns, through the identical
// calls. Without it, an arm-1 failure cannot be distinguished from "these KMT
// calls do not work in this process at all".
void control_arm(const Kmt& k, const KmtDevice& primary, const KmtDevice& secondary,
                 ID3D12Device* d3d12) {
    printf("    arm 2 CONTROL: our own D3DKMT monitored fence\n");
    if (!primary.hDevice) {
        printf("      skipped: no KMT device on this adapter\n");
        return;
    }

    D3DKMT_CREATESYNCHRONIZATIONOBJECT2 cs = {};
    cs.hDevice = primary.hDevice;
    cs.Info.Type = D3DDDI_MONITORED_FENCE;
    cs.Info.Flags.Shared = 1;
    cs.Info.Flags.NtSecuritySharing = 1;
    cs.Info.MonitoredFence.InitialFenceValue = 0;
    NTSTATUS st = k.create_sync2(&cs);
    if (st != 0) {
        printf("      CreateSynchronizationObject2(MONITORED,Shared,NtSec) -> 0x%08lX %s\n",
               static_cast<unsigned long>(st), nt_name(st));
        return;
    }
    printf("      CreateSynchronizationObject2 ok local=0x%08X cpu_va=%p\n", cs.hSyncObject,
           cs.Info.MonitoredFence.FenceValueCPUVirtualAddress);

    HANDLE nt = nullptr;
    D3DKMT_HANDLE obj = cs.hSyncObject;
    OBJECT_ATTRIBUTES attr = {};
    attr.Length = sizeof(attr);
    st = k.share_objects(1, &obj, &attr, GENERIC_ALL, &nt);
    if (st != 0) {
        printf("      D3DKMTShareObjects -> 0x%08lX %s\n", static_cast<unsigned long>(st),
               nt_name(st));
    } else {
        describe_handle("control fence", nt);
        try_open2(k, "control fence, same device", nt, primary.hDevice, false);
        try_open2(k, "control fence, same device", nt, primary.hDevice, true);
        if (secondary.hDevice)
            try_open2(k, "control fence, 2nd device", nt, secondary.hDevice, false);
        try_open1(k, "control fence", nt);
        try_open_native_fence(k, "control fence", nt, primary.hDevice);
        try_query_resource(k, nt);

        // arm 4 — THE REVERSE DIRECTION, and the only row that is actionable.
        // If D3D12 cannot open OUR fence and we cannot open D3D12's, the two
        // sides simply cannot share a fence through any public interface and
        // the layer needs a different synchronisation design. If D3D12 CAN
        // open ours, the fix is a direction change the ICD already has the
        // machinery for: the ICD creates and exports the monitored fence, and
        // the D3D12 side opens it, instead of the layer importing an
        // ID3D12Fence.
        if (d3d12) {
            ID3D12Fence* opened = nullptr;
            const HRESULT hr = d3d12->OpenSharedHandle(nt, IID_PPV_ARGS(&opened));
            printf("      reverse   ID3D12Device::OpenSharedHandle(our KMT fence) -> 0x%08lX %s\n",
                   static_cast<unsigned long>(hr), SUCCEEDED(hr) ? "OK" : "FAILED");
            if (opened) {
                printf("                GetCompletedValue=%llu\n",
                       static_cast<unsigned long long>(opened->GetCompletedValue()));
                opened->Release();
            }
        } else {
            printf("      reverse   (no D3D12 device on this adapter)\n");
        }
        CloseHandle(nt);
    }

    // arm 3 — CONTROL for the composite-handle mechanism. `D3DKMTShareObjects`
    // can pack N objects into one NT handle, and `...FromNtHandle2` opens
    // exactly one. If a two-object handle also returns STATUS_INVALID_PARAMETER
    // while the one-object handle above succeeds, then "the runtime shared more
    // than a bare fence" is a live explanation for arm 1's failure. If it opens
    // fine, that explanation is dead and the difference is the object's class.
    D3DKMT_CREATESYNCHRONIZATIONOBJECT2 cs2 = {};
    cs2.hDevice = primary.hDevice;
    cs2.Info.Type = D3DDDI_MONITORED_FENCE;
    cs2.Info.Flags.Shared = 1;
    cs2.Info.Flags.NtSecuritySharing = 1;
    cs2.Info.MonitoredFence.InitialFenceValue = 0;
    if (k.create_sync2(&cs2) == 0) {
        HANDLE nt2 = nullptr;
        D3DKMT_HANDLE pair[2] = {cs.hSyncObject, cs2.hSyncObject};
        OBJECT_ATTRIBUTES attr2 = {};
        attr2.Length = sizeof(attr2);
        const NTSTATUS sst = k.share_objects(2, pair, &attr2, GENERIC_ALL, &nt2);
        printf("    arm 3 CONTROL: two sync objects in ONE NT handle\n");
        printf("      D3DKMTShareObjects(2 objects) -> 0x%08lX %s\n",
               static_cast<unsigned long>(sst), nt_name(sst));
        if (sst == 0) {
            describe_handle("2-object handle", nt2);
            try_open2(k, "2-object handle", nt2, primary.hDevice, false);
            CloseHandle(nt2);
        }
        D3DKMT_DESTROYSYNCHRONIZATIONOBJECT d2 = {};
        d2.hSyncObject = cs2.hSyncObject;
        (void)k.destroy_sync(&d2);
    }

    D3DKMT_DESTROYSYNCHRONIZATIONOBJECT d = {};
    d.hSyncObject = cs.hSyncObject;
    (void)k.destroy_sync(&d);
}

void run_adapter(const Kmt& k, const AdapterRec& rec, const std::vector<AdapterRec>& all) {
    printf("\n=== adapter %ls  LUID=%08lX:%08lX  flags=0x%X ===\n", rec.desc.c_str(),
           static_cast<unsigned long>(rec.luid.HighPart),
           static_cast<unsigned long>(rec.luid.LowPart), rec.flags);

    KmtDevice primary = kmt_open(k, rec.luid, "primary");
    KmtDevice secondary = kmt_open(k, rec.luid, "secondary");

    printf("    arm 1: ID3D12Fence(D3D12_FENCE_FLAG_SHARED)::CreateSharedHandle\n");
    ID3D12Device* dev = nullptr;
    HRESULT hr = D3D12CreateDevice(rec.adapter, D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&dev));
    if (FAILED(hr)) {
        printf("      D3D12CreateDevice -> 0x%08lX (adapter not D3D12-capable; arm 1 skipped)\n",
               static_cast<unsigned long>(hr));
    } else {
        ID3D12Fence* fence = nullptr;
        hr = dev->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(&fence));
        if (FAILED(hr)) {
            printf("      CreateFence(SHARED) -> 0x%08lX\n", static_cast<unsigned long>(hr));
        } else {
            HANDLE nt = nullptr;
            hr = dev->CreateSharedHandle(fence, nullptr, GENERIC_ALL, nullptr, &nt);
            if (FAILED(hr)) {
                printf("      CreateSharedHandle -> 0x%08lX\n", static_cast<unsigned long>(hr));
            } else {
                describe_handle("d3d12 fence", nt);

                // Round-trip through the runtime. If this succeeds the handle
                // is valid and functional, and any KMT rejection below is a
                // statement about the KMT interface, not about the handle.
                ID3D12Fence* reopened = nullptr;
                const HRESULT rt = dev->OpenSharedHandle(nt, IID_PPV_ARGS(&reopened));
                printf("      OpenSharedHandle round-trip (ID3D12Fence)      -> 0x%08lX %s\n",
                       static_cast<unsigned long>(rt), SUCCEEDED(rt) ? "OK" : "FAILED");
                if (reopened)
                    reopened->Release();

                try_open2(k, "d3d12 fence, this adapter", nt, primary.hDevice, false);
                try_open2(k, "d3d12 fence, this adapter", nt, primary.hDevice, true);
                // (c): is it the device that is wrong? Try every OTHER adapter's
                // device. A different status here localises the failure to the
                // adapter binding rather than to the object.
                for (const auto& other : all) {
                    if (other.luid.LowPart == rec.luid.LowPart &&
                        other.luid.HighPart == rec.luid.HighPart)
                        continue;
                    KmtDevice od = kmt_open(k, other.luid, "cross");
                    if (od.hDevice)
                        try_open2(k, "d3d12 fence, OTHER adapter", nt, od.hDevice, false);
                    kmt_close(k, od);
                }
                try_open1(k, "d3d12 fence", nt);
                try_open_native_fence(k, "d3d12 fence", nt, primary.hDevice);
                try_query_resource(k, nt);
                CloseHandle(nt);
            }
            fence->Release();
        }
    }

    // `dev` stays alive: arm 4 inside the control needs it to try the reverse
    // direction on this same adapter.
    control_arm(k, primary, secondary, dev);
    if (dev)
        dev->Release();

    kmt_close(k, secondary);
    kmt_close(k, primary);
}

} // namespace

int main() {
    printf("d3d12_shared_fence_probe — is an ID3D12Fence shared handle a WDDM sync object?\n");
    printf("pid=%lu\n", GetCurrentProcessId());

    if (!load_ntdll()) {
        printf("FATAL: cannot resolve NtQueryObject\n");
        return 1;
    }
    const Kmt k = load_kmt();
    if (!k.complete) {
        printf("FATAL: D3DKMT surface incomplete\n");
        return 1;
    }

    IDXGIFactory1* factory = nullptr;
    if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&factory)))) {
        printf("FATAL: CreateDXGIFactory1 failed\n");
        return 1;
    }

    std::vector<AdapterRec> adapters;
    for (UINT i = 0;; ++i) {
        IDXGIAdapter1* a = nullptr;
        if (factory->EnumAdapters1(i, &a) == DXGI_ERROR_NOT_FOUND)
            break;
        DXGI_ADAPTER_DESC1 d = {};
        a->GetDesc1(&d);
        adapters.push_back({d.AdapterLuid, d.Description, d.Flags, a});
    }
    printf("adapters: %zu\n", adapters.size());

    for (const auto& rec : adapters)
        run_adapter(k, rec, adapters);

    for (auto& rec : adapters)
        rec.adapter->Release();
    factory->Release();

    printf("\nHOW TO READ THIS\n");
    printf("  * arm 2 succeeds, arm 1 fails, on the SAME adapter and device:\n");
    printf("      the object is the difference. An ID3D12Fence shared handle is not\n");
    printf("      a KMT-openable sync object for us -> the layer's fence design is wrong,\n");
    printf("      not the ICD.\n");
    printf("  * arm 1 fails on the Microsoft/WARP adapter too:\n");
    printf("      generic D3D12 architecture, nothing Helios-specific. Do not change\n");
    printf("      icd/mesa.\n");
    printf("  * arm 1 fails ONLY on Helios:\n");
    printf("      ours. The type name says which component to look at.\n");
    printf("  * both arms fail:\n");
    printf("      the instrument is broken. Fix the probe before concluding anything.\n");
    return 0;
}
