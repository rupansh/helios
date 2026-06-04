//! `EvtIoDeviceControl` — the IOCTL channel the Venus ICD drives over
//! `DeviceIoControl` (ARCH.md §3). The IOCTL control code is the verb; WDF
//! validates the in/out buffer lengths the I/O manager reports, and we re-check
//! every guest-supplied size against those lengths before reading.
//!
//! These handler bodies are the former `ddi/escape.rs` Venus verbs ported 1:1
//! onto IOCTL buffers: CTX_CREATE / CTX_DESTROY / SUBMIT_VENUS / WAIT_FENCE are
//! live; ALLOC_BLOB / MAP_BLOB are documented Phase 3/4 stubs (as the escape
//! path left them).
//!
//! TRUST BOUNDARY: the buffers are guest-supplied. We pass each op's struct size
//! as `WdfRequestRetrieve*Buffer`'s `MinimumRequiredLength` (WDF rejects a short
//! buffer) and read with `pod_read_unaligned` (no alignment assumption).

use core::mem::size_of;
use core::slice;

use bytemuck::{bytes_of, pod_read_unaligned};
use helios_protocol::{
    HeliosEscapeAllocBlob, HeliosEscapeCtxCreate, HeliosEscapeCtxDestroy, HeliosEscapeSubmitVenus,
    HeliosEscapeWaitFence, IOCTL_HELIOS_ALLOC_BLOB, IOCTL_HELIOS_CTX_CREATE,
    IOCTL_HELIOS_CTX_DESTROY, IOCTL_HELIOS_MAP_BLOB, IOCTL_HELIOS_SUBMIT_VENUS,
    IOCTL_HELIOS_WAIT_FENCE,
};
use wdk_sys::{
    call_unsafe_wdf_function_binding, NTSTATUS, PVOID, ULONG, ULONG_PTR, WDFOBJECT, WDFQUEUE,
    WDFREQUEST, NT_SUCCESS, STATUS_DEVICE_DOES_NOT_EXIST, STATUS_INSUFFICIENT_RESOURCES,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_PARAMETER, STATUS_NOT_IMPLEMENTED, STATUS_SUCCESS,
};

use crate::adapter::{adapter_of, AdapterContext};
use crate::virtio::hal::DmaBuffer;

/// `EvtIoDeviceControl` for the default parallel queue. Dispatches on the IOCTL
/// control code and completes the request with the handler's status + byte count.
pub unsafe extern "C" fn evt_io_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // The device behind the queue → its AdapterContext.
    // SAFETY: `queue` is our queue; WdfIoQueueGetDevice returns our WDFDEVICE.
    let device = call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue);
    let adapter = match adapter_of(device.cast::<core::ffi::c_void>() as WDFOBJECT) {
        Some(a) => a,
        None => {
            complete(request, STATUS_DEVICE_DOES_NOT_EXIST, 0);
            return;
        }
    };

    let (status, info): (NTSTATUS, usize) = match io_control_code {
        IOCTL_HELIOS_CTX_CREATE => handle_ctx_create(adapter, request),
        IOCTL_HELIOS_CTX_DESTROY => handle_ctx_destroy(adapter, request),
        IOCTL_HELIOS_SUBMIT_VENUS => handle_submit_venus(adapter, request),
        IOCTL_HELIOS_WAIT_FENCE => handle_wait_fence(adapter, request),
        IOCTL_HELIOS_ALLOC_BLOB => handle_alloc_blob(adapter, request),
        // MAP_BLOB lands in Phase 4c. STUB: documented not-implemented.
        IOCTL_HELIOS_MAP_BLOB => (STATUS_NOT_IMPLEMENTED, 0),
        // Unknown control codes are rejected (CLAUDE.md invariant).
        _ => (STATUS_INVALID_DEVICE_REQUEST, 0),
    };
    complete(request, status, info);
}

/// `WdfRequestCompleteWithInformation`.
unsafe fn complete(request: WDFREQUEST, status: NTSTATUS, info: usize) {
    call_unsafe_wdf_function_binding!(
        WdfRequestCompleteWithInformation,
        request,
        status,
        info as ULONG_PTR
    );
}

/// Retrieve the request's buffered input buffer (≥ `min` bytes, WDF-enforced).
unsafe fn input_buffer(request: WDFREQUEST, min: usize) -> Result<(*mut u8, usize), NTSTATUS> {
    let mut ptr: PVOID = core::ptr::null_mut();
    let mut len: usize = 0;
    let st = call_unsafe_wdf_function_binding!(
        WdfRequestRetrieveInputBuffer,
        request,
        min,
        &mut ptr,
        &mut len
    );
    if NT_SUCCESS(st) {
        Ok((ptr.cast::<u8>(), len))
    } else {
        Err(st)
    }
}

/// Retrieve the request's output buffer (≥ `min` bytes). For METHOD_BUFFERED this
/// is the shared system buffer; for the SUBMIT_VENUS METHOD_IN_DIRECT request it
/// is the read-locked bulk buffer WDF maps to a system VA for us.
unsafe fn output_buffer(request: WDFREQUEST, min: usize) -> Result<(*mut u8, usize), NTSTATUS> {
    let mut ptr: PVOID = core::ptr::null_mut();
    let mut len: usize = 0;
    let st = call_unsafe_wdf_function_binding!(
        WdfRequestRetrieveOutputBuffer,
        request,
        min,
        &mut ptr,
        &mut len
    );
    if NT_SUCCESS(st) {
        Ok((ptr.cast::<u8>(), len))
    } else {
        Err(st)
    }
}

/// `IOCTL_HELIOS_CTX_CREATE` → create a Venus virtio-gpu context; write the
/// guest-assigned id back into the (METHOD_BUFFERED in/out) buffer.
unsafe fn handle_ctx_create(adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let sz = size_of::<HeliosEscapeCtxCreate>();
    let (in_ptr, _) = match input_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    let (out_ptr, _) = match output_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    // SAFETY: WDF guaranteed ≥ sz bytes at in_ptr/out_ptr. Read unaligned.
    let req: HeliosEscapeCtxCreate = pod_read_unaligned(slice::from_raw_parts(in_ptr, sz));
    match adapter.with_virtio(|v| v.ctx_create(req.capset_id)) {
        Ok(Ok(ctx_id)) => {
            let mut out = req;
            out.out_ctx_id = ctx_id;
            // SAFETY: ≥ sz writable bytes at out_ptr.
            slice::from_raw_parts_mut(out_ptr, sz).copy_from_slice(bytes_of(&out));
            (STATUS_SUCCESS, sz)
        }
        Ok(Err(ve)) => (ve.into(), 0),
        Err(de) => (de.into(), 0),
    }
}

/// `IOCTL_HELIOS_CTX_DESTROY` → tear down a context.
unsafe fn handle_ctx_destroy(adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let sz = size_of::<HeliosEscapeCtxDestroy>();
    let (in_ptr, _) = match input_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    // SAFETY: ≥ sz bytes at in_ptr.
    let req: HeliosEscapeCtxDestroy = pod_read_unaligned(slice::from_raw_parts(in_ptr, sz));
    match adapter.with_virtio(|v| v.ctx_destroy(req.ctx_id)) {
        Ok(Ok(())) => (STATUS_SUCCESS, 0),
        Ok(Err(ve)) => (ve.into(), 0),
        Err(de) => (de.into(), 0),
    }
}

/// `IOCTL_HELIOS_ALLOC_BLOB` → create a virtio-gpu blob resource; write the
/// guest-assigned `out_resource_id` back into the (METHOD_BUFFERED in/out) buffer.
unsafe fn handle_alloc_blob(adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let sz = size_of::<HeliosEscapeAllocBlob>();
    let (in_ptr, _) = match input_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    let (out_ptr, _) = match output_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    // SAFETY: WDF guaranteed ≥ sz bytes at in_ptr/out_ptr. Read unaligned.
    let req: HeliosEscapeAllocBlob = pod_read_unaligned(slice::from_raw_parts(in_ptr, sz));
    match adapter
        .with_virtio(|v| v.alloc_blob(req.ctx_id, req.blob_mem, req.blob_flags, req.size))
    {
        Ok(Ok(resource_id)) => {
            let mut out = req;
            out.out_resource_id = resource_id;
            // SAFETY: ≥ sz writable bytes at out_ptr.
            slice::from_raw_parts_mut(out_ptr, sz).copy_from_slice(bytes_of(&out));
            (STATUS_SUCCESS, sz)
        }
        Ok(Err(ve)) => (ve.into(), 0),
        Err(de) => (de.into(), 0),
    }
}

/// `IOCTL_HELIOS_SUBMIT_VENUS` (METHOD_IN_DIRECT) → forward an opaque Venus
/// command stream to the host. The fixed header rides the buffered input buffer;
/// the variable Venus blob rides the read-locked output buffer (WDF maps it to a
/// system VA). We stage the blob into a contiguous DMA buffer at PASSIVE_LEVEL
/// (before taking the queue lock) and submit it fenced.
unsafe fn handle_submit_venus(adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let hsz = size_of::<HeliosEscapeSubmitVenus>();
    let (hdr_ptr, _) = match input_buffer(request, hsz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    // SAFETY: ≥ hsz bytes at hdr_ptr.
    let req: HeliosEscapeSubmitVenus = pod_read_unaligned(slice::from_raw_parts(hdr_ptr, hsz));

    let payload = req.buffer_size as usize;
    if payload == 0 {
        return (STATUS_INVALID_PARAMETER, 0);
    }
    // The Venus blob is the IN_DIRECT-locked output buffer; require ≥ payload.
    let (venus_ptr, _venus_len) = match output_buffer(request, payload) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };

    // Copy the stream into device-visible contiguous memory (PASSIVE_LEVEL).
    let mut dma = match DmaBuffer::new(payload) {
        Some(d) => d,
        None => return (STATUS_INSUFFICIENT_RESOURCES, 0),
    };
    // SAFETY: WDF guaranteed ≥ payload readable bytes at venus_ptr.
    dma.as_mut_slice()
        .copy_from_slice(slice::from_raw_parts(venus_ptr, payload));

    let (ctx_id, fence_id) = (req.ctx_id, req.fence_id);
    let status = match adapter.with_virtio(|v| v.submit_venus(ctx_id, fence_id, dma.as_slice())) {
        Ok(Ok(())) => STATUS_SUCCESS,
        Ok(Err(ve)) => ve.into(),
        Err(de) => de.into(),
    };
    // `dma` drops here, at PASSIVE_LEVEL, after the lock has been released.
    (status, 0)
}

/// `IOCTL_HELIOS_WAIT_FENCE` → interim synchronous fence model.
///
/// `submit_venus` blocks on the used ring until the device acknowledges the
/// fenced command, so any fence the ICD asks to wait on has already completed by
/// the time SUBMIT_VENUS returned. We validate the request shape and report
/// success. PHASE 4 (async submission): read the request and call
/// `adapter.fences.wait_and_remove(req.fence_id, req.timeout_ns)`.
unsafe fn handle_wait_fence(_adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let sz = size_of::<HeliosEscapeWaitFence>();
    match input_buffer(request, sz) {
        Ok(_) => (STATUS_SUCCESS, 0),
        Err(s) => (s, 0),
    }
}
