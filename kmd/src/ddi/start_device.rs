//! Adapter PnP / power lifecycle DDIs and the render-only child queries.
//!
//! Phase 1: StartDevice saves the Dxgkrnl interface and reports a render-only
//! adapter (zero video present sources, zero children). The virtio-gpu hardware
//! bring-up (PCI cap scan, BAR mapping, feature negotiation, virtqueue init) is
//! added in Phase 2 (task #4) where the STUB marker is below.

use alloc::boxed::Box;
use core::ffi::c_void;

use crate::adapter::AdapterContext;
use crate::dxgk::*;

/// `DxgkDdiStartDevice` — bring the adapter online.
pub unsafe extern "C" fn dxgkddi_start_device(
    miniport_device_context: *mut c_void,
    _dxgk_start_info: *mut DXGK_START_INFO,
    dxgkrnl_interface: *mut DXGKRNL_INTERFACE,
    number_of_video_present_sources: *mut u32,
    number_of_children: *mut u32,
) -> NTSTATUS {
    crate::kmsg(c"Helios: StartDevice\n");

    if miniport_device_context.is_null()
        || dxgkrnl_interface.is_null()
        || number_of_video_present_sources.is_null()
        || number_of_children.is_null()
    {
        return STATUS_INVALID_PARAMETER;
    }

    // SAFETY: Dxgkrnl passes our adapter context and valid out-pointers.
    let adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };

    // Save the callback interface for the driver's lifetime (Copy struct).
    adapter.dxgkrnl = Some(unsafe { *dxgkrnl_interface });

    // STUB (Phase 2): initialize the virtio-gpu device here —
    //   1. DxgkCbGetDeviceInformation → translated PCI resource list
    //   2. scan VirtIO PCI vendor caps, map BARs
    //   3. reset → ACK → DRIVER → negotiate features → FEATURES_OK → DRIVER_OK
    //   4. init control virtqueue, GET_DISPLAY_INFO smoke test
    // For Phase 1 the adapter simply starts so it appears in Device Manager.

    // Render-only adapter: no scanout sources, no child devices (no monitors).
    // SAFETY: out-pointers validated non-null above.
    unsafe {
        *number_of_video_present_sources = 0;
        *number_of_children = 0;
    }

    STATUS_SUCCESS
}

/// `DxgkDdiStopDevice` — quiesce the adapter (inverse of StartDevice).
pub unsafe extern "C" fn dxgkddi_stop_device(miniport_device_context: *mut c_void) -> NTSTATUS {
    crate::kmsg(c"Helios: StopDevice\n");
    // STUB (Phase 2): reset the device, disable queues, unmap BARs.
    let _ = miniport_device_context;
    STATUS_SUCCESS
}

/// `DxgkDdiRemoveDevice` — free the adapter context allocated in AddDevice.
pub unsafe extern "C" fn dxgkddi_remove_device(miniport_device_context: *mut c_void) -> NTSTATUS {
    crate::kmsg(c"Helios: RemoveDevice\n");
    if !miniport_device_context.is_null() {
        // SAFETY: this pointer came from Box::into_raw in AddDevice; freed once.
        drop(unsafe { Box::from_raw(miniport_device_context as *mut AdapterContext) });
    }
    STATUS_SUCCESS
}

/// `DxgkDdiDispatchIoRequest` — legacy VRP path; unused by a render-only WDDM
/// adapter.
pub unsafe extern "C" fn dxgkddi_dispatch_io_request(
    _miniport_device_context: *mut c_void,
    _vidpn_source_id: u32,
    _video_request_packet: PVIDEO_REQUEST_PACKET,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// `DxgkDdiSetPowerState` — accept power transitions (nothing device-specific to
/// do yet).
pub unsafe extern "C" fn dxgkddi_set_power_state(
    _miniport_device_context: *mut c_void,
    _device_uid: u32,
    _device_power_state: DEVICE_POWER_STATE,
    _action_type: POWER_ACTION::Type,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiQueryChildRelations` — render-only: we expose no child devices.
pub unsafe extern "C" fn dxgkddi_query_child_relations(
    _miniport_device_context: *mut c_void,
    _child_relations: *mut DXGK_CHILD_DESCRIPTOR,
    _child_relations_size: u32,
) -> NTSTATUS {
    // No connectors/monitors → leave the (already-zeroed) array untouched.
    STATUS_SUCCESS
}

/// `DxgkDdiQueryChildStatus` — no children to report status for.
pub unsafe extern "C" fn dxgkddi_query_child_status(
    _miniport_device_context: *mut c_void,
    _child_status: *mut DXGK_CHILD_STATUS,
    _non_destructive_only: BOOLEAN,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `DxgkDdiQueryDeviceDescriptor` — no child descriptors (no EDID/monitor).
pub unsafe extern "C" fn dxgkddi_query_device_descriptor(
    _miniport_device_context: *mut c_void,
    _child_uid: u32,
    _device_descriptor: *mut DXGK_DEVICE_DESCRIPTOR,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

// ── Base driver/adapter lifecycle DDIs ──────────────────────────────────────
// These sit in the base (non-version-gated) block of DRIVER_INITIALIZATION_DATA
// and are all present in the MSDN DxgkInitialize sample. dxgkrnl's init path
// (DpiInitializeEx) rejects the init data when they are NULL — leaving them out
// is what made DxgkInitialize return STATUS_REVISION_MISMATCH even after the
// render/GPU-VA DDIs were registered.

/// `DxgkDdiUnload` — driver-wide unload (no device context). Inverse of
/// DriverEntry; nothing global to tear down yet.
pub unsafe extern "C" fn dxgkddi_unload() {
    crate::kmsg(c"Helios: Unload\n");
}

/// `DxgkDdiQueryInterface` — export a driver-defined interface. We expose none.
pub unsafe extern "C" fn dxgkddi_query_interface(
    _miniport_device_context: IN_CONST_PVOID,
    _query_interface: IN_PQUERY_INTERFACE,
) -> NTSTATUS {
    STATUS_NOT_SUPPORTED
}

/// `DxgkDdiControlEtwLogging` — enable/disable the driver's ETW logging. We emit
/// none, so this is a no-op.
pub unsafe extern "C" fn dxgkddi_control_etw_logging(
    _enable: IN_BOOLEAN,
    _flags: IN_ULONG,
    _level: IN_UCHAR,
) {
}

/// `DxgkDdiResetDevice` — reset the device to a known state (e.g. before a crash
/// dump). No hardware to quiesce until Phase 2; no-op.
pub unsafe extern "C" fn dxgkddi_reset_device(_miniport_device_context: IN_CONST_PVOID) {}

/// `DxgkDdiNotifyAcpiEvent` — handle a platform ACPI event. We service none.
pub unsafe extern "C" fn dxgkddi_notify_acpi_event(
    _miniport_device_context: IN_CONST_PVOID,
    _event_type: IN_DXGK_EVENT_TYPE,
    _event: IN_ULONG,
    _argument: IN_PVOID,
    _acpi_flags: OUT_PULONG,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}
