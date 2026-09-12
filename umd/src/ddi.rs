//! d3d10umddi DDI types, generated from the WDK `d3d10umddi.h` by bindgen
//! (see `build.rs::generate_d3d10umddi_bindings`).
//!
//! These are the exact ABI structs the OS D3D11 runtime lowers app/DWM calls
//! into: the adapter-funcs tables, `D3D10DDIARG_CREATEDEVICE`, the 152-entry
//! `D3D11DDI_DEVICEFUNCS` device-funcs table, the DXGI base DDI, and the
//! kernel/runtime callback tables. We fill the device-funcs table (backed by the
//! DXVK device from the cxx bridge) to make `D3D11CreateDevice(Helios)` succeed.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]
#![allow(clippy::all)]

include!(concat!(env!("OUT_DIR"), "/d3d10umddi.rs"));

/// Target-specific negotiation layout tripwires, separate from bindgen's own
/// generated layout checks. The raw diagnostic dump in adapter.rs uses these
/// types and bounds itself at ppfnRetrieveSubObject for pre-11.1 callers.
mod abi_offsets {
    use super::*;
    use core::mem::{offset_of, size_of};

    #[cfg(target_pointer_width = "64")]
    const _: () = {
        assert!(size_of::<D3D10DDIARG_CREATEDEVICE>() == 88);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, Interface) == 8);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, pKTCallbacks) == 16);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, __bindgen_anon_1) == 24);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, hDrvDevice) == 32);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, DXGIBaseDDI) == 40);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, hRTCoreLayer) == 56);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, __bindgen_anon_2) == 64);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, Flags) == 72);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, ppfnRetrieveSubObject) == 80);
        assert!(size_of::<DXGI_DDI_BASE_ARGS>() == 16);
        assert!(size_of::<D3D10DDIARG_OPENADAPTER>() == 40);
        assert!(size_of::<D3D10_2DDIARG_GETCAPS>() == 32);
    };

    #[cfg(target_pointer_width = "32")]
    const _: () = {
        assert!(size_of::<D3D10DDIARG_CREATEDEVICE>() == 48);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, Interface) == 4);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, pKTCallbacks) == 12);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, __bindgen_anon_1) == 16);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, hDrvDevice) == 20);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, DXGIBaseDDI) == 24);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, hRTCoreLayer) == 32);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, __bindgen_anon_2) == 36);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, Flags) == 40);
        assert!(offset_of!(D3D10DDIARG_CREATEDEVICE, ppfnRetrieveSubObject) == 44);
        assert!(size_of::<DXGI_DDI_BASE_ARGS>() == 8);
        assert!(size_of::<D3D10DDIARG_OPENADAPTER>() == 24);
        assert!(size_of::<D3D10_2DDIARG_GETCAPS>() == 16);
    };
}
