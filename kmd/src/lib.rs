//! Helios vGPU kernel-mode driver (KMD).
//!
//! A WDDM 2.x render-only display miniport driver for the virtio-gpu device
//! (VEN_1AF4 & DEV_1050). `DriverEntry` registers our DDI table with Dxgkrnl via
//! `DxgkInitialize`; from there Dxgkrnl drives the device lifecycle through the
//! callbacks below.
//!
//! Implementation status: Phase 1.5 (adapter enumeration + a loadable DDI
//! table). The adapter-lifecycle and capability DDIs are real; the render and
//! GPU-VA DDIs are registered as `STATUS_NOT_IMPLEMENTED` stubs because
//! `DxgkInitialize` validates the full mandatory render-miniport DDI set at
//! registration time — a sparse table is rejected with
//! `STATUS_FAILED_DRIVER_ENTRY` (device Code 37).

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
mod ddi;
mod device;
// TEMPORARY: post-start bring-up tracer to locate the AddAdapter failure.
mod diag;
mod dxgk;
mod error;
// Scaffolding: the transport's types/parsers are wired into the StartDevice path
// over Phase-2 milestones M1–M4; allow dead_code until M4 consumes them all.
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

/// Driver entry point (named "DriverEntry" so the INF/loader finds it).
///
/// # Safety
/// Called by the OS with valid `driver_object` / `registry_path` per the WDM
/// contract.
#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PUNICODE_STRING,
) -> NTSTATUS {
    kmsg(c"Helios: DriverEntry\n");

    let mut init = build_ddi_table();
    // SAFETY: pointers are valid for the call; `init` outlives the call on this
    // stack frame, and DxgkInitialize copies what it needs.
    unsafe { DxgkInitialize(driver_object, registry_path, &mut init) }
}

/// Build the `DRIVER_INITIALIZATION_DATA` DDI table.
fn build_ddi_table() -> DRIVER_INITIALIZATION_DATA {
    // SAFETY: an all-zero DRIVER_INITIALIZATION_DATA is valid and means "no DDI
    // registered"; we then fill in the callbacks we support.
    let mut data: DRIVER_INITIALIZATION_DATA = unsafe { core::mem::zeroed() };

    // Declare the OS-native WDDM version: DXGKDDI_INTERFACE_VERSION is the
    // build-time default (WDDM3_2 / 0x11007 on the 26100 WDK), matching the struct
    // layout bindgen compiles. Declaring WDDM 2.0 instead made 24H2 reject the
    // user-mode driver at AddAdapter with STATUS_REVISION_MISMATCH (a 2.0 adapter
    // is too old for the OS's UMD), so we stay native. dxgkrnl then queries some
    // WDDM 2.6/2.9 cap types (DXGKQAITYPE_WDDMDEVICECAPS=29, PHYSICAL_MEMORY_CAPS=34)
    // which we answer STATUS_NOT_SUPPORTED — tolerated during bring-up.
    data.Version = DXGKDDI_INTERFACE_VERSION;

    // ── PnP / power lifecycle (Phase 1, real) ──────────────────────────────
    data.DxgkDdiAddDevice = Some(ddi::dxgkddi_add_device);
    data.DxgkDdiStartDevice = Some(ddi::dxgkddi_start_device);
    data.DxgkDdiStopDevice = Some(ddi::dxgkddi_stop_device);
    data.DxgkDdiRemoveDevice = Some(ddi::dxgkddi_remove_device);
    data.DxgkDdiDispatchIoRequest = Some(ddi::dxgkddi_dispatch_io_request);
    data.DxgkDdiSetPowerState = Some(ddi::dxgkddi_set_power_state);

    // ── Base driver/adapter lifecycle DDIs ──────────────────────────────────
    // Non-version-gated base-block DDIs that the MSDN DxgkInitialize sample
    // always provides. dxgkrnl's DpiInitializeEx rejects the init data (with
    // STATUS_REVISION_MISMATCH) when these are NULL, even though the version
    // field itself is accepted — this is the second gate past the DDI-presence
    // check that surfaced once the render/GPU-VA DDIs were registered.
    data.DxgkDdiUnload = Some(ddi::dxgkddi_unload);
    data.DxgkDdiQueryInterface = Some(ddi::dxgkddi_query_interface);
    data.DxgkDdiControlEtwLogging = Some(ddi::dxgkddi_control_etw_logging);
    data.DxgkDdiResetDevice = Some(ddi::dxgkddi_reset_device);
    data.DxgkDdiNotifyAcpiEvent = Some(ddi::dxgkddi_notify_acpi_event);

    // ── Child/adapter queries (Phase 1, real — render-only: no children) ────
    data.DxgkDdiQueryChildRelations = Some(ddi::dxgkddi_query_child_relations);
    data.DxgkDdiQueryChildStatus = Some(ddi::dxgkddi_query_child_status);
    data.DxgkDdiQueryDeviceDescriptor = Some(ddi::dxgkddi_query_device_descriptor);
    data.DxgkDdiQueryAdapterInfo = Some(ddi::dxgkddi_query_adapter_info);

    // ── Interrupt path (registered now; ISR wired in Phase 2/3) ─────────────
    data.DxgkDdiInterruptRoutine = Some(ddi::dxgkddi_interrupt_routine);
    data.DxgkDdiDpcRoutine = Some(ddi::dxgkddi_dpc_routine);

    // ── Device / context / process (Phase 1 device alloc; rest stubbed) ─────
    data.DxgkDdiCreateDevice = Some(device::dxgkddi_create_device);
    data.DxgkDdiDestroyDevice = Some(device::dxgkddi_destroy_device);
    data.DxgkDdiCreateContext = Some(device::dxgkddi_create_context);
    data.DxgkDdiDestroyContext = Some(device::dxgkddi_destroy_context);
    data.DxgkDdiCreateProcess = Some(device::dxgkddi_create_process);
    data.DxgkDdiDestroyProcess = Some(device::dxgkddi_destroy_process);

    // ── Memory management (Phase 3 stubs) ───────────────────────────────────
    data.DxgkDdiCreateAllocation = Some(ddi::dxgkddi_create_allocation);
    data.DxgkDdiDestroyAllocation = Some(ddi::dxgkddi_destroy_allocation);
    data.DxgkDdiBuildPagingBuffer = Some(ddi::dxgkddi_build_paging_buffer);

    // ── Command submission (Phase 3 stubs) ──────────────────────────────────
    // SubmitCommand is the physical/paging submit path: Dxgkrnl queues paging
    // buffers (from BuildPagingBuffer, hDevice=NULL) through it, so a driver that
    // advertises paging must register it. SubmitCommandVirtual is the GPU-VA
    // render path. We register both.
    data.DxgkDdiSubmitCommand = Some(ddi::dxgkddi_submit_command);
    data.DxgkDdiSubmitCommandVirtual = Some(ddi::dxgkddi_submit_command_virtual);
    data.DxgkDdiPreemptCommand = Some(ddi::dxgkddi_preempt_command);
    data.DxgkDdiResetFromTimeout = Some(ddi::dxgkddi_reset_from_timeout);
    data.DxgkDdiRestartFromTimeout = Some(ddi::dxgkddi_restart_from_timeout);

    // ── Out-of-band ICD → KMD channel (Phase 4 stub) ────────────────────────
    data.DxgkDdiEscape = Some(ddi::dxgkddi_escape);

    // ── Mandatory render-path & GPU-VA DDIs (Phase 1.5 — the Code-37 fix) ────
    // DxgkInitialize validates that a WDDM 2.0 *render* miniport exposes the
    // full render + GPU-virtual-addressing DDI surface. With these NULL it
    // rejects the table (STATUS_FAILED_DRIVER_ENTRY → device Code 37), even
    // though they aren't exercised until rendering begins. They're registered
    // as STATUS_NOT_IMPLEMENTED stubs here; real bodies land in Phases 3–4.
    data.DxgkDdiRender = Some(ddi::dxgkddi_render);
    data.DxgkDdiRenderKm = Some(ddi::dxgkddi_render_km);
    data.DxgkDdiPatch = Some(ddi::dxgkddi_patch);
    data.DxgkDdiOpenAllocation = Some(ddi::dxgkddi_open_allocation);
    data.DxgkDdiCloseAllocation = Some(ddi::dxgkddi_close_allocation);
    data.DxgkDdiDescribeAllocation = Some(ddi::dxgkddi_describe_allocation);
    data.DxgkDdiGetStandardAllocationDriverData =
        Some(ddi::dxgkddi_get_standard_allocation_driver_data);
    data.DxgkDdiGetNodeMetadata = Some(ddi::dxgkddi_get_node_metadata);
    data.DxgkDdiSetRootPageTable = Some(ddi::dxgkddi_set_root_page_table);
    data.DxgkDdiGetRootPageTableSize = Some(ddi::dxgkddi_get_root_page_table_size);
    data.DxgkDdiCollectDbgInfo = Some(ddi::dxgkddi_collect_dbg_info);
    data.DxgkDdiControlInterrupt = Some(ddi::dxgkddi_control_interrupt);
    data.DxgkDdiQueryCurrentFence = Some(ddi::dxgkddi_query_current_fence);

    // Display/VidPn DDIs are intentionally left NULL — render-only adapter.
    data
}
