//! helios_umd — minimal **stub** WDDM D3D user-mode driver.
//!
//! dxgkrnl requires a *render* adapter to register a version-matched user-mode
//! driver (`UserModeDriverName`) and resolves + version-checks it during
//! AddAdapter; without one the adapter fails with Code 43 (`OBJECT_NAME_NOT_FOUND`
//! → `REVISION_MISMATCH` with a mismatched UMD). This DLL satisfies that contract
//! with the minimum: it exports `OpenAdapter10`/`OpenAdapter10_2`/`OpenAdapter12`,
//! reports the WDDM 2.0 D3D DDI version, and returns trivial function tables so
//! the adapter starts.
//!
//! It is **not** a functional D3D UMD: `CreateDevice` returns `E_NOTIMPL`. That's
//! fine — Helios renders via DXVK/VKD3D → the Vulkan ICD (the Mesa venus port),
//! which bypasses the native D3D runtime entirely. The table entries below are
//! only ever called at native-D3D device creation, which we never do.
//!
//! ABI: the D3D UMD entry points are `APIENTRY` (`__stdcall`), which on x64 is the
//! single platform convention — `extern "system"` matches it exactly. Structs are
//! `#[repr(C)]` hand-declarations of the (small) WDK `d3d10umddi.h`/`d3d12umddi.h`
//! layouts.

#![allow(non_snake_case, non_camel_case_types)]

use core::ffi::c_void;

const S_OK: i32 = 0;
const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;

// D3D UMD DDI versions reported by GetSupportedVersions, newest-first. These are
// the `D3DWDDM*_DDI_SUPPORTED` values from d3d10umddi.h (26100), extracted with a
// `cl` probe. We advertise the full range (2.0 → 3.2) so the runtime can pick
// whichever it wants for this adapter — reporting only WDDM 2.0 caused
// STATUS_REVISION_MISMATCH because 24H2 expects a newer (OS-native) revision.
const SUPPORTED_VERSIONS: [u64; 10] = [
    0x000B_002D_0001_0000, // WDDM 3.2
    0x000B_002C_0000_0000, // WDDM 3.1
    0x000B_002B_0000_0000, // WDDM 3.0
    0x000B_0027_0004_0000, // WDDM 2.6
    0x000B_0026_0000_0000, // WDDM 2.5
    0x000B_0025_0001_0000, // WDDM 2.4
    0x000B_0024_0001_0000, // WDDM 2.3
    0x000B_0023_0005_0000, // WDDM 2.2
    0x000B_0022_0002_0000, // WDDM 2.1
    0x000B_0020_0009_0000, // WDDM 2.0
];

/// Fill the standard `GetSupportedVersions` out-params: a NULL `vers` is the
/// count query; otherwise copy up to `*count` versions and report how many.
unsafe fn report_versions(count: *mut u32, vers: *mut u64) -> i32 {
    if count.is_null() {
        return E_INVALIDARG;
    }
    let n = SUPPORTED_VERSIONS.len() as u32;
    // SAFETY: caller passes a valid count; vers (if non-NULL) holds >= *count u64s.
    unsafe {
        if vers.is_null() {
            *count = n;
        } else {
            let cap = (*count).min(n) as usize;
            for i in 0..cap {
                *vers.add(i) = SUPPORTED_VERSIONS[i];
            }
            *count = cap as u32;
        }
    }
    S_OK
}

// A driver adapter handle the runtime stores opaquely. Must be non-NULL.
const STUB_HADAPTER: *mut c_void = 1 as *mut c_void;

// ───────────────────────── D3D10 / D3D11 UMD ─────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct D3D10DDI_HADAPTER {
    p: *mut c_void,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct D3D10DDI_HRTADAPTER {
    p: *mut c_void,
}

/// `D3D10_2DDI_ADAPTERFUNCS` (5 entries) — the table OpenAdapter10_2 returns.
#[repr(C)]
struct D3D10_2DDI_ADAPTERFUNCS {
    pfnCalcPrivateDeviceSize: unsafe extern "system" fn(D3D10DDI_HADAPTER, *const c_void) -> usize,
    pfnCreateDevice: unsafe extern "system" fn(D3D10DDI_HADAPTER, *mut c_void) -> i32,
    pfnCloseAdapter: unsafe extern "system" fn(D3D10DDI_HADAPTER) -> i32,
    pfnGetSupportedVersions: unsafe extern "system" fn(D3D10DDI_HADAPTER, *mut u32, *mut u64) -> i32,
    pfnGetCaps: unsafe extern "system" fn(D3D10DDI_HADAPTER, *const c_void) -> i32,
}

#[repr(C)]
struct D3D10DDIARG_OPENADAPTER {
    hRTAdapter: D3D10DDI_HRTADAPTER,
    hAdapter: D3D10DDI_HADAPTER,
    Interface: u32,
    Version: u32,
    pAdapterCallbacks: *const c_void,
    // Union of pAdapterFuncs / pAdapterFuncs_2 — same pointer slot. OpenAdapter10_2
    // reads it as the 5-entry _2 table; OpenAdapter10 reads the first 3 entries.
    pAdapterFuncs: *mut D3D10_2DDI_ADAPTERFUNCS,
}

unsafe extern "system" fn d10_calc_size(_a: D3D10DDI_HADAPTER, _x: *const c_void) -> usize {
    core::mem::size_of::<usize>()
}
unsafe extern "system" fn d10_create_device(_a: D3D10DDI_HADAPTER, _x: *mut c_void) -> i32 {
    E_NOTIMPL
}
unsafe extern "system" fn d10_close_adapter(_a: D3D10DDI_HADAPTER) -> i32 {
    S_OK
}
unsafe extern "system" fn d10_get_versions(
    _a: D3D10DDI_HADAPTER,
    count: *mut u32,
    vers: *mut u64,
) -> i32 {
    unsafe { report_versions(count, vers) }
}
unsafe extern "system" fn d10_get_caps(_a: D3D10DDI_HADAPTER, _x: *const c_void) -> i32 {
    S_OK
}

static FUNCS10: D3D10_2DDI_ADAPTERFUNCS = D3D10_2DDI_ADAPTERFUNCS {
    pfnCalcPrivateDeviceSize: d10_calc_size,
    pfnCreateDevice: d10_create_device,
    pfnCloseAdapter: d10_close_adapter,
    pfnGetSupportedVersions: d10_get_versions,
    pfnGetCaps: d10_get_caps,
};

/// D3D10.1+/D3D11 user-mode driver entry point.
#[no_mangle]
pub unsafe extern "system" fn OpenAdapter10_2(args: *mut D3D10DDIARG_OPENADAPTER) -> i32 {
    if args.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the runtime owns a valid OPENADAPTER struct.
    let a = unsafe { &mut *args };
    a.hAdapter = D3D10DDI_HADAPTER { p: STUB_HADAPTER };
    a.pAdapterFuncs = &FUNCS10 as *const _ as *mut _;
    S_OK
}

/// Legacy D3D10 entry point — routes to the same minimal table.
#[no_mangle]
pub unsafe extern "system" fn OpenAdapter10(args: *mut D3D10DDIARG_OPENADAPTER) -> i32 {
    unsafe { OpenAdapter10_2(args) }
}

// ───────────────────────────── D3D12 UMD ─────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct D3D12DDI_HADAPTER {
    p: *mut c_void,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct D3D12DDI_HRTADAPTER {
    p: *mut c_void,
}

/// `D3D12DDI_ADAPTERFUNCS` (8 entries). Exact arg types past the handle don't
/// matter here (these are never invoked at AddAdapter); all are stubbed.
#[repr(C)]
struct D3D12DDI_ADAPTERFUNCS {
    pfnCalcPrivateDeviceSize: unsafe extern "system" fn(D3D12DDI_HADAPTER, *const c_void) -> usize,
    pfnCreateDevice: unsafe extern "system" fn(D3D12DDI_HADAPTER, *mut c_void) -> i32,
    pfnCloseAdapter: unsafe extern "system" fn(D3D12DDI_HADAPTER) -> i32,
    pfnGetSupportedVersions: unsafe extern "system" fn(D3D12DDI_HADAPTER, *mut u32, *mut u64) -> i32,
    pfnGetCaps: unsafe extern "system" fn(D3D12DDI_HADAPTER, *const c_void) -> i32,
    pfnGetOptionalDDITables: unsafe extern "system" fn(D3D12DDI_HADAPTER, *mut c_void) -> i32,
    pfnFillDDITable: unsafe extern "system" fn(D3D12DDI_HADAPTER, *mut c_void) -> i32,
    pfnDestroyDevice: unsafe extern "system" fn(D3D12DDI_HADAPTER) -> i32,
}

#[repr(C)]
struct D3D12DDIARG_OPENADAPTER {
    hRTAdapter: D3D12DDI_HRTADAPTER,
    hAdapter: D3D12DDI_HADAPTER,
    pAdapterCallbacks: *const c_void,
    pAdapterFuncs: *mut D3D12DDI_ADAPTERFUNCS,
}

unsafe extern "system" fn d12_calc_size(_a: D3D12DDI_HADAPTER, _x: *const c_void) -> usize {
    core::mem::size_of::<usize>()
}
unsafe extern "system" fn d12_create_device(_a: D3D12DDI_HADAPTER, _x: *mut c_void) -> i32 {
    E_NOTIMPL
}
unsafe extern "system" fn d12_close_adapter(_a: D3D12DDI_HADAPTER) -> i32 {
    S_OK
}
unsafe extern "system" fn d12_get_versions(
    _a: D3D12DDI_HADAPTER,
    count: *mut u32,
    vers: *mut u64,
) -> i32 {
    unsafe { report_versions(count, vers) }
}
unsafe extern "system" fn d12_get_caps(_a: D3D12DDI_HADAPTER, _x: *const c_void) -> i32 {
    S_OK
}
unsafe extern "system" fn d12_get_optional(_a: D3D12DDI_HADAPTER, _x: *mut c_void) -> i32 {
    S_OK
}
unsafe extern "system" fn d12_fill(_a: D3D12DDI_HADAPTER, _x: *mut c_void) -> i32 {
    S_OK
}
unsafe extern "system" fn d12_destroy(_a: D3D12DDI_HADAPTER) -> i32 {
    S_OK
}

static FUNCS12: D3D12DDI_ADAPTERFUNCS = D3D12DDI_ADAPTERFUNCS {
    pfnCalcPrivateDeviceSize: d12_calc_size,
    pfnCreateDevice: d12_create_device,
    pfnCloseAdapter: d12_close_adapter,
    pfnGetSupportedVersions: d12_get_versions,
    pfnGetCaps: d12_get_caps,
    pfnGetOptionalDDITables: d12_get_optional,
    pfnFillDDITable: d12_fill,
    pfnDestroyDevice: d12_destroy,
};

/// D3D12 user-mode driver entry point.
#[no_mangle]
pub unsafe extern "system" fn OpenAdapter12(args: *mut D3D12DDIARG_OPENADAPTER) -> i32 {
    if args.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the runtime owns a valid OPENADAPTER struct.
    let a = unsafe { &mut *args };
    a.hAdapter = D3D12DDI_HADAPTER { p: STUB_HADAPTER };
    a.pAdapterFuncs = &FUNCS12 as *const _ as *mut _;
    S_OK
}
