//! Per-D3D-device, per-context, and per-process state, plus their DDIs.
//!
//! Phase 1 implements device alloc/free (so the runtime can open a device
//! without crashing). Context and GPU-VA process DDIs are stubbed until the
//! Venus path (Phase 4) and the memory model (Phase 3) land.
//!
//! NOTE: the exact argument struct/handle types below come from the generated
//! `dxgk` bindings and may need a binding-alignment pass at first compile.

use alloc::boxed::Box;
use core::ffi::c_void;

use crate::adapter::AdapterContext;
use crate::dxgk::*;

/// State for one D3D device opened on the adapter.
pub struct DeviceContext {
    /// Back-pointer to the owning adapter (valid for the device's lifetime).
    pub adapter: *mut AdapterContext,
}

/// `DxgkDdiCreateDevice` — allocate per-device state.
pub unsafe extern "C" fn dxgkddi_create_device(
    miniport_device_context: *mut c_void,
    create_device: *mut DXGKARG_CREATEDEVICE,
) -> NTSTATUS {
    if miniport_device_context.is_null() || create_device.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: Dxgkrnl passes our adapter context and a valid args struct.
    let args = unsafe { &mut *create_device };
    let ctx = Box::new(DeviceContext {
        adapter: miniport_device_context as *mut AdapterContext,
    });
    // Hand the device handle back to Dxgkrnl; reclaimed in destroy_device.
    args.hDevice = Box::into_raw(ctx) as *mut c_void;
    STATUS_SUCCESS
}

/// `DxgkDdiDestroyDevice` — free per-device state.
pub unsafe extern "C" fn dxgkddi_destroy_device(h_device: *mut c_void) -> NTSTATUS {
    if !h_device.is_null() {
        // SAFETY: h_device was produced by Box::into_raw in create_device and is
        // destroyed exactly once.
        drop(unsafe { Box::from_raw(h_device as *mut DeviceContext) });
    }
    STATUS_SUCCESS
}

/// `DxgkDdiCreateContext` — GPU execution context.
// STUB: Phase 4 — create the Venus virtio-gpu context here.
pub unsafe extern "C" fn dxgkddi_create_context(
    _h_device: *mut c_void,
    _create_context: *mut DXGKARG_CREATECONTEXT,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiDestroyContext`.
// STUB: Phase 4 — tear down the Venus context.
pub unsafe extern "C" fn dxgkddi_destroy_context(_h_context: *mut c_void) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiCreateProcess` — GPU-VA process object (WDDM 2.0 requirement).
// STUB: Phase 3 — track per-process GPU virtual address space.
pub unsafe extern "C" fn dxgkddi_create_process(
    _miniport_device_context: *mut c_void,
    _args: *mut DXGKARG_CREATEPROCESS,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiDestroyProcess`.
// STUB: Phase 3.
pub unsafe extern "C" fn dxgkddi_destroy_process(
    _miniport_device_context: *mut c_void,
    _h_process: *mut c_void,
) -> NTSTATUS {
    STATUS_SUCCESS
}
