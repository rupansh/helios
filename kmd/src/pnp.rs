//! PnP / power lifecycle: `evt_device_add` and the prepare/release/D0 callbacks.
//!
//! `evt_device_add` (registered on the WDF driver) builds the WDFDEVICE: it sets
//! the PnP/power callbacks, attaches our typed device context, creates the device
//! interface (`GUID_DEVINTERFACE_HELIOS`), the default parallel IO queue (whose
//! `EvtIoDeviceControl` is the IOCTL dispatch), and the WDF interrupt.
//!
//! `evt_device_prepare_hardware` brings the virtio-gpu transport online — it
//! queries the PCI bus's `BUS_INTERFACE_STANDARD` for config-space access and
//! runs `VirtioGpu::init`. `evt_device_release_hardware` tears it back down.

use alloc::boxed::Box;

use wdk_sys::{
    call_unsafe_wdf_function_binding, BUS_INTERFACE_STANDARD, GUID, NTSTATUS, PINTERFACE,
    PWDFDEVICE_INIT, USHORT, WDFCMRESLIST, WDFDEVICE, WDFDRIVER, WDFOBJECT, WDFQUEUE,
    WDF_NO_OBJECT_ATTRIBUTES, WDF_POWER_DEVICE_STATE, STATUS_SUCCESS, STATUS_UNSUCCESSFUL,
};
use wdk_sys::NT_SUCCESS;

use crate::adapter::{
    adapter_of, device_context_mut, device_context_type_info, free_adapter, AdapterContext,
};
use crate::ioctl::evt_io_device_control;
use crate::kmsg;
use crate::virtio::{KmdfConfigAccess, VirtioGpu};
use crate::wdf::{self, GUID_BUS_INTERFACE_STANDARD, GUID_DEVINTERFACE_HELIOS};

/// `EvtDriverDeviceAdd` — construct the WDFDEVICE and its facets.
pub unsafe extern "C" fn evt_device_add(
    _driver: WDFDRIVER,
    mut device_init: PWDFDEVICE_INIT,
) -> NTSTATUS {
    kmsg(c"Helios: evt_device_add\n");

    // (a) PnP/power callbacks. virtio bring-up happens in prepare/release; the
    //     D0 callbacks are trivial (no per-power-transition work yet).
    let mut pnp = wdf::pnp_power_callbacks(
        Some(evt_device_prepare_hardware),
        Some(evt_device_release_hardware),
        Some(evt_device_d0_entry),
        Some(evt_device_d0_exit),
    );
    // SAFETY: `device_init` is the OS-provided init object (valid for this
    // callback); `&mut pnp` is a valid PWDF_PNPPOWER_EVENT_CALLBACKS.
    call_unsafe_wdf_function_binding!(WdfDeviceInitSetPnpPowerEventCallbacks, device_init, &mut pnp);

    // Set the device I/O type (the one device-init call the working mvisor
    // reference makes that we omitted). SAFETY: `device_init` valid.
    call_unsafe_wdf_function_binding!(
        WdfDeviceInitSetIoType,
        device_init,
        wdk_sys::_WDF_DEVICE_IO_TYPE::WdfDeviceIoDirect as wdk_sys::WDF_DEVICE_IO_TYPE
    );

    // NOTE: a device SDDL (WdfDeviceInitAssignSDDLString) for non-elevated
    // user-mode ICD access is a Phase 5 TODO — the first attempt produced an
    // invalid SECURITY_DESCRIPTOR (Code 31). It must be added with a SDDL
    // validated offline (e.g. ConvertStringSecurityDescriptorToSecurityDescriptor)
    // before going back on the device.

    // (b) Device attributes: register our typed context + a cleanup callback that
    //     frees the heap AdapterContext when the device is destroyed.
    let mut attrs = wdf::object_attributes_for_context(device_context_type_info());
    attrs.EvtCleanupCallback = Some(evt_device_cleanup);

    // (c) Create the device. WdfDeviceCreate takes *mut PWDFDEVICE_INIT and nulls
    //     our local on success — never touch `device_init` afterward.
    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: all three pointers are valid; on success `device` is set.
    let status =
        call_unsafe_wdf_function_binding!(WdfDeviceCreate, &mut device_init, &mut attrs, &mut device);
    if !NT_SUCCESS(status) {
        kmsg(c"Helios: WdfDeviceCreate failed\n");
        return status;
    }

    // (d) Allocate the AdapterContext and stash it in the inline device context.
    //     Freed in evt_device_cleanup. WDF zeroed the inline context, so `adapter`
    //     starts null; set it now.
    let adapter = Box::into_raw(Box::new(AdapterContext::new()));
    // SAFETY: `device` is our WDFDEVICE created with the matching context type.
    // device_add is serialized w.r.t. IOCTL dispatch (the interface is not yet
    // openable), so the exclusive &mut is sound.
    device_context_mut(device.cast()).adapter = adapter;

    // (e) Device interface — the channel the ICD opens via SetupDi/CreateFile.
    // SAFETY: `device` valid; GUID has a stable static address; no ref string.
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreateDeviceInterface,
        device,
        &GUID_DEVINTERFACE_HELIOS as *const GUID,
        core::ptr::null()
    );
    if !NT_SUCCESS(status) {
        kmsg(c"Helios: WdfDeviceCreateDeviceInterface failed\n");
        return status;
    }

    // (f) Default parallel IO queue → EvtIoDeviceControl (the IOCTL spine). The
    //     default queue auto-receives IRP_MJ_DEVICE_CONTROL — no explicit routing
    //     needed. (The earlier "default queue never dispatches" symptom was NOT
    //     the queue *kind*; it was `Settings.Parallel.NumberOfPresentedRequests`
    //     left at 0 in the queue-config builder — see wdf::io_queue_config.)
    let mut qcfg = wdf::io_queue_config(Some(evt_io_device_control));
    let mut queue: WDFQUEUE = core::ptr::null_mut();
    // SAFETY: `device`/`&mut qcfg`/`&mut queue` are valid.
    let status = call_unsafe_wdf_function_binding!(
        WdfIoQueueCreate,
        device,
        &mut qcfg,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut queue
    );
    if !NT_SUCCESS(status) {
        kmsg(c"Helios: WdfIoQueueCreate failed\n");
        return status;
    }
    let _ = queue; // default queue auto-receives IOCTLs; handle not needed

    // (g) NO WDF interrupt object in Phase 1–3. The transport runs in pure
    //     polling mode with device interrupts suppressed (gpu.rs init →
    //     set_dev_notify(false)), so we neither need nor want a WDFINTERRUPT yet.
    //     Creating one here made the device fail to start: KMDF connects+enables
    //     the interrupt during D0 power-up (after EvtDeviceD0Entry), and that
    //     step failed with STATUS_DEVICE_POWER_FAILURE on this device's interrupt
    //     assignment. Phase 4 (async fences) revisits this — it will create the
    //     interrupt only when an interrupt resource is actually present and
    //     re-enable device notifications. See interrupt.rs.

    kmsg(c"Helios: device added OK\n");
    STATUS_SUCCESS
}

/// `EvtDevicePrepareHardware` — bring the virtio-gpu transport online.
///
/// Resources are assigned (BARs mappable) and the device is powered, but
/// interrupts are not yet connected — virtio feature negotiation needs neither.
/// We get config-space access via the PCI bus's `BUS_INTERFACE_STANDARD` and run
/// `VirtioGpu::init` (which maps the BARs on demand through `WdkHal`).
pub unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resources_raw: WDFCMRESLIST,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    kmsg(c"Helios: prepare_hardware\n");

    // Query the PCI bus driver for the standard bus interface (config access).
    // SAFETY: zeroed BUS_INTERFACE_STANDARD is the documented input shape; the
    // bus driver fills Size/Version/Context + the GetBusData/SetBusData pointers.
    let mut bus: BUS_INTERFACE_STANDARD = core::mem::zeroed();
    let bus_ptr = (&mut bus as *mut BUS_INTERFACE_STANDARD) as PINTERFACE;
    let status = call_unsafe_wdf_function_binding!(
        WdfFdoQueryForInterface,
        device,
        &GUID_BUS_INTERFACE_STANDARD as *const GUID,
        bus_ptr,
        core::mem::size_of::<BUS_INTERFACE_STANDARD>() as USHORT,
        1u16, // BUS_INTERFACE_STANDARD version
        core::ptr::null_mut()
    );
    if !NT_SUCCESS(status) {
        kmsg(c"Helios: QueryForInterface(BUS_INTERFACE_STANDARD) failed\n");
        return status;
    }

    // Config access is needed only during PciTransport::new (cap/BAR discovery);
    // after init the transport uses MMIO. So we can release the bus-interface
    // reference as soon as init returns.
    let access = KmdfConfigAccess::new(&bus);
    let result = VirtioGpu::init(&access);
    if let Some(deref) = bus.InterfaceDereference {
        // SAFETY: balances the reference WdfFdoQueryForInterface took; `Context`
        // is the value the bus driver gave us.
        deref(bus.Context);
    }

    let adapter = match adapter_of(device.cast()) {
        Some(a) => a,
        // Should never happen: evt_device_add set the adapter before any IRP.
        None => return STATUS_UNSUCCESSFUL,
    };
    match result {
        Ok(gpu) => {
            adapter.set_virtio(Some(gpu));
            kmsg(c"Helios: virtio-gpu transport up\n");
            STATUS_SUCCESS
        }
        Err(e) => {
            kmsg(c"Helios: virtio-gpu init failed\n");
            let st: NTSTATUS = e.into();
            st
        }
    }
}

/// `EvtDeviceReleaseHardware` — tear down the transport (drops `VirtioGpu`, which
/// resets the device and frees DMA at PASSIVE_LEVEL).
pub unsafe extern "C" fn evt_device_release_hardware(
    device: WDFDEVICE,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    kmsg(c"Helios: release_hardware\n");
    if let Some(adapter) = adapter_of(device.cast()) {
        adapter.set_virtio(None);
    }
    STATUS_SUCCESS
}

/// `EvtDeviceD0Entry` — no per-D0 work (transport lives across the prepare/release
/// span). Present because ARCH §1 registers it.
pub unsafe extern "C" fn evt_device_d0_entry(
    _device: WDFDEVICE,
    _previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `EvtDeviceD0Exit` — see [`evt_device_d0_entry`].
pub unsafe extern "C" fn evt_device_d0_exit(
    _device: WDFDEVICE,
    _target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    STATUS_SUCCESS
}

/// `EvtCleanupCallback` on the device — free the heap `AdapterContext`. Runs at
/// PASSIVE_LEVEL on device destruction, after ReleaseHardware has cleared virtio.
pub unsafe extern "C" fn evt_device_cleanup(object: WDFOBJECT) {
    free_adapter(object);
}
