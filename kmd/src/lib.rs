//! Helios vGPU kernel-mode driver (KMD).
//!
//! A **WDDM Display-Only Driver (DOD)** for the virtio-gpu PCI device
//! (VEN_1AF4 & DEV_1050). `DriverEntry` registers the DOD DDI table with dxgkrnl
//! via `DxgkInitializeDisplayOnlyDriver`; dxgkrnl then drives the device
//! lifecycle through the callbacks in `dod.rs`. The driver owns the virtio-gpu
//! scanout and presents the Windows desktop to the host (SPICE) over a 2D
//! scanout; the venus ops ride `DxgkDdiEscape` (Phase 7.2). See DISPLAY.md.
//!
//! Phase-7 display pivot (DISPLAY.md §3): this replaces the System-class KMDF
//! function-driver model. The virtio transport (`virtio/`), the `helios_protocol`
//! wire crate, and the Mesa venus ICD are reused unchanged.
//!
//! Bring-up status: **Phase 7.1** — DOD skeleton: loads as a Display adapter and
//! brings up the 2D desktop scanout. The VidPN cofunctional-modality DDIs are
//! first-cut stubs (`dod.rs`); the full mode-commit + venus escape land in
//! 7.1b / 7.2.

#![no_std]
#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

extern crate alloc;

// wdk-panic supplies the kernel `#[panic_handler]` (KeBugCheck); importing the
// crate is sufficient. We never want to panic in release, but if we do, this
// bugchecks rather than corrupting kernel state.
#[cfg(not(test))]
extern crate wdk_panic;

use wdk_alloc::WdkAllocator;

#[cfg(not(test))]
#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

mod adapter;
// TEMPORARY: registry-breadcrumb tracer to locate the post-start (Code 43)
// failing DDI; removed once the DOD loads cleanly.
mod diag;
mod dod;
mod dxgk;
mod error;
mod vidpn;
// The transport carries some not-yet-consumed scaffolding (blob/async-fence
// helpers used by the Phase-7.2 venus escape path) until that lands.
#[allow(dead_code)]
mod virtio;

use dxgk::*;

/// Emit a line to the kernel debugger / DebugView.
pub(crate) fn kmsg(msg: &core::ffi::CStr) {
    // SAFETY: DbgPrint takes a NUL-terminated C format string; `msg` is
    // NUL-terminated and carries no `%` specifiers, so no varargs are consumed.
    unsafe {
        wdk_sys::ntddk::DbgPrint(msg.as_ptr().cast());
    }
}

/// Driver entry point (named "DriverEntry" so the loader finds it).
///
/// # Safety
/// Called by the OS with valid `driver_object` / `registry_path` per the WDM
/// contract; dxgkrnl takes over once `DxgkInitializeDisplayOnlyDriver` registers
/// our DOD DDI table.
#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PUNICODE_STRING,
) -> NTSTATUS {
    kmsg(c"Helios DOD: DriverEntry\n");

    let mut init = build_dod_init_data();
    // SAFETY: pointers are valid for the call; `init` outlives the call on this
    // stack frame, and DxgkInitializeDisplayOnlyDriver copies what it needs.
    unsafe { DxgkInitializeDisplayOnlyDriver(driver_object, registry_path, &mut init) }
}

/// Build the `KMDDOD_INITIALIZATION_DATA` DOD DDI table.
///
/// `Version = DXGKDDI_INTERFACE_VERSION` — the OS-native version (the bindgen
/// header default; WDDM 3.2 on the 26100 WDK). This MUST match the version the
/// structs are compiled against: dxgkrnl sizes the buffers it hands our DDIs
/// (e.g. `DXGK_DRIVERCAPS` in QueryAdapterInfo) by the declared version, so
/// declaring an older version (WIN8) against header-default-compiled structs
/// makes the QueryAdapterInfo size check fail `BUFFER_TOO_SMALL` → Code 43. This
/// matches the Microsoft KMDOD sample (`InitialData.Version =
/// DXGKDDI_INTERFACE_VERSION`). Unfilled DDIs stay `None` (zeroed), which
/// dxgkrnl tolerates for a display-only driver (KMDOD leaves many NULL too).
fn build_dod_init_data() -> KMDDOD_INITIALIZATION_DATA {
    // SAFETY: an all-zero KMDDOD_INITIALIZATION_DATA is valid and means "no DDI
    // registered"; we then fill in the ones we support.
    let mut data: KMDDOD_INITIALIZATION_DATA = unsafe { core::mem::zeroed() };
    data.Version = DXGKDDI_INTERFACE_VERSION;

    // Full DDI table. NOTE: trimming this to KMDOD's 28-entry set (leaving the
    // optional DDIs NULL) was found to REGRESS the VidPN commit path — dxgkrnl then
    // rejects our target mode `pfnAddMode` with STATUS_GRAPHICS_INVALID_FREQUENCY
    // (0xC01E030A) in every enum context, so no cofunctional VidPN forms and Present
    // never fires (which only *masked* the post-present Code 43, it did not fix it).
    // With the full table the target mode is accepted and the desktop commits. The
    // optional DDIs that previously returned NOT_IMPLEMENTED now return SUCCESS
    // (accept-and-ignore) so a registered present-window DDI never fails a call.

    // ── Lifecycle / PnP / power ─────────────────────────────────────────────
    data.DxgkDdiAddDevice = Some(dod::add_device);
    data.DxgkDdiStartDevice = Some(dod::start_device);
    data.DxgkDdiStopDevice = Some(dod::stop_device);
    data.DxgkDdiRemoveDevice = Some(dod::remove_device);
    data.DxgkDdiDispatchIoRequest = Some(dod::dispatch_io_request);
    data.DxgkDdiInterruptRoutine = Some(dod::interrupt_routine);
    data.DxgkDdiDpcRoutine = Some(dod::dpc_routine);
    data.DxgkDdiQueryChildRelations = Some(dod::query_child_relations);
    data.DxgkDdiQueryChildStatus = Some(dod::query_child_status);
    data.DxgkDdiQueryDeviceDescriptor = Some(dod::query_device_descriptor);
    data.DxgkDdiSetPowerState = Some(dod::set_power_state);
    data.DxgkDdiUnload = Some(dod::unload);
    data.DxgkDdiQueryInterface = Some(dod::query_interface);
    data.DxgkDdiQueryAdapterInfo = Some(dod::query_adapter_info);
    data.DxgkDdiResetDevice = Some(dod::reset_device); // load-mandatory (Code 37 if NULL)
    data.DxgkDdiNotifyAcpiEvent = Some(dod::notify_acpi_event);
    data.DxgkDdiControlEtwLogging = Some(dod::control_etw_logging);
    data.DxgkDdiSetPalette = Some(dod::set_palette);
    data.DxgkDdiCollectDbgInfo = Some(dod::collect_dbg_info);
    data.DxgkDdiGetScanLine = Some(dod::get_scan_line);
    data.DxgkDdiControlInterrupt = Some(dod::control_interrupt);
    data.DxgkDdiGetChildContainerId = Some(dod::get_child_container_id);
    data.DxgkDdiNotifySurpriseRemoval = Some(dod::notify_surprise_removal);

    // ── Pointer (software cursor — accept/ignore) ───────────────────────────
    data.DxgkDdiSetPointerPosition = Some(dod::set_pointer_position);
    data.DxgkDdiSetPointerShape = Some(dod::set_pointer_shape);

    // ── VidPN (mode negotiation) ────────────────────────────────────────────
    data.DxgkDdiIsSupportedVidPn = Some(dod::is_supported_vidpn);
    data.DxgkDdiRecommendFunctionalVidPn = Some(dod::recommend_functional_vidpn);
    data.DxgkDdiEnumVidPnCofuncModality = Some(dod::enum_vidpn_cofunc_modality);
    data.DxgkDdiSetVidPnSourceVisibility = Some(dod::set_vidpn_source_visibility);
    data.DxgkDdiCommitVidPn = Some(dod::commit_vidpn);
    data.DxgkDdiUpdateActiveVidPnPresentPath = Some(dod::update_active_vidpn_present_path);
    data.DxgkDdiRecommendMonitorModes = Some(dod::recommend_monitor_modes);
    data.DxgkDdiQueryVidPnHWCapability = Some(dod::query_vidpn_hw_capability);

    // ── Present + system display ────────────────────────────────────────────
    data.DxgkDdiPresentDisplayOnly = Some(dod::present_display_only);
    data.DxgkDdiStopDeviceAndReleasePostDisplayOwnership =
        Some(dod::stop_device_and_release_post_display_ownership);
    data.DxgkDdiSystemDisplayEnable = Some(dod::system_display_enable);
    data.DxgkDdiSystemDisplayWrite = Some(dod::system_display_write);

    // The venus carrier — STUB for now (NOT_SUPPORTED); the real escape dispatch
    // (today's ioctl.rs body) lands in Phase 7.2.
    data.DxgkDdiEscape = Some(dod::escape);
    data
}
