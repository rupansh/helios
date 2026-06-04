//! Helios vGPU kernel-mode driver (KMD).
//!
//! A **System-class KMDF function driver** for the virtio-gpu PCI device
//! (VEN_1AF4 & DEV_1050). It is NOT a WDDM/display miniport: there is no
//! dxgkrnl, no DDI table, no GPU-VA contract. `DriverEntry` registers
//! `evt_device_add` via `WdfDriverCreate`; from there WDF drives the device
//! lifecycle (pnp.rs). User mode (the Mesa-venus Vulkan ICD) reaches the driver
//! via `DeviceIoControl` on `GUID_DEVINTERFACE_HELIOS`, carrying the six
//! `helios_protocol` ops as IOCTLs (ioctl.rs). See ARCH.md (canonical).
//!
//! Bring-up status: **Phase 1** — System-class KMDF skeleton + the virtio
//! transport re-homed onto the KMDF device + the IOCTL control verbs
//! (CTX_CREATE/DESTROY/SUBMIT_VENUS/WAIT_FENCE). Blob verbs and the async-fence
//! DPC land in Phases 3–4.

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
mod error;
mod fence;
// The WDF interrupt object (ISR/DPC + WdfInterruptCreate) is not used in
// Phase 1–3 (the transport polls; device interrupts are suppressed). Kept for
// Phase 4 (async fences); allow(dead_code) until then.
#[allow(dead_code)]
mod interrupt;
mod ioctl;
mod mapping;
mod pnp;
mod wdf;
// The transport carries some not-yet-consumed scaffolding (blob/async-fence
// helpers, extra VirtioError variants) until Phases 3–4 wire them in.
#[allow(dead_code)]
mod virtio;

use wdk_sys::{
    call_unsafe_wdf_function_binding, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, WDFDRIVER,
    WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES,
};

/// Emit a line to the kernel debugger / DebugView.
pub(crate) fn kmsg(msg: &core::ffi::CStr) {
    // SAFETY: DbgPrint takes a NUL-terminated C format string; `msg` is
    // NUL-terminated and carries no `%` specifiers, so no varargs are consumed.
    unsafe {
        wdk_sys::ntddk::DbgPrint(msg.as_ptr().cast());
    }
}

/// Driver entry point (named "DriverEntry" so the WDF loader finds it).
///
/// # Safety
/// Called by the OS with valid `driver_object` / `registry_path` per the WDM
/// contract; KMDF takes over once `WdfDriverCreate` registers `evt_device_add`.
#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    kmsg(c"Helios: DriverEntry\n");

    let mut config = wdf::driver_config(Some(pnp::evt_device_add));
    config.EvtDriverUnload = Some(evt_driver_unload);

    // SAFETY: `driver_object`/`registry_path` are the OS-provided pointers;
    // `&mut config` is valid; we pass a null (optional) driver-handle out.
    call_unsafe_wdf_function_binding!(
        WdfDriverCreate,
        driver_object,
        registry_path,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut config,
        WDF_NO_HANDLE.cast::<WDFDRIVER>()
    )
}

/// `EvtDriverUnload` — release the process-wide BAR MMIO mappings `WdkHal`
/// cached across device start/stop cycles (nothing references them once all
/// devices are removed, which precedes driver unload).
unsafe extern "C" fn evt_driver_unload(_driver: WDFDRIVER) {
    kmsg(c"Helios: DriverUnload\n");
    crate::virtio::hal::WdkHal::unmap_all();
}
