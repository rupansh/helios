//! `DxgkDdiAddDevice` — allocate the adapter context for a discovered device.
//!
//! Reference: https://learn.microsoft.com/windows-hardware/drivers/ddi/dispmprt/nc-dispmprt-dxgkddi_add_device

use core::ffi::c_void;

use crate::adapter::AdapterContext;
use crate::dxgk::*;

pub unsafe extern "C" fn dxgkddi_add_device(
    physical_device_object: PDEVICE_OBJECT,
    miniport_device_context: *mut *mut c_void,
) -> NTSTATUS {
    crate::kmsg(c"Helios: AddDevice\n");
    crate::diag::record(0x0A00_0001);

    if miniport_device_context.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    // Match the display-miniport contract and the reference viogpu3d driver:
    // leave a deterministic NULL out-pointer on every failure path after this.
    unsafe { *miniport_device_context = core::ptr::null_mut() };

    // One call: allocate at the final heap address AND initialize the embedded
    // kernel dispatcher objects there. `create` never hands back an
    // `AdapterContext` by value, so a caller cannot skip the in-place init and
    // cannot move the context afterwards. The context is leaked to a raw
    // pointer; Dxgkrnl returns it to us on every DDI and we reclaim it in
    // DxgkDdiRemoveDevice.
    if physical_device_object.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let raw = match AdapterContext::create(physical_device_object) {
        Ok(raw) => raw,
        Err(crate::virtio::TransportOwnerCreateError::DomainExhausted) => {
            crate::kmsg(c"Helios: AddDevice transport domain exhausted\n");
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        Err(crate::virtio::TransportOwnerCreateError::StorageExhausted) => {
            crate::kmsg(c"Helios: AddDevice control-owner storage exhausted\n");
            return STATUS_INSUFFICIENT_RESOURCES;
        }
    };
    // SAFETY: miniport_device_context is a valid out-pointer per the DDI contract.
    unsafe { *miniport_device_context = raw.as_ptr() as *mut c_void };

    crate::diag::record(0x0A00_0002);
    STATUS_SUCCESS
}
