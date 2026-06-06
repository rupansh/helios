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

use alloc::vec::Vec;
use bytemuck::{bytes_of, pod_read_unaligned};
use helios_protocol::{
    HeliosEscapeAllocBlob, HeliosEscapeCtxCreate, HeliosEscapeCtxDestroy, HeliosEscapeMapBlob,
    HeliosEscapePresentBlob, HeliosEscapeSubmitVenus, HeliosEscapeWaitFence, IOCTL_HELIOS_ALLOC_BLOB,
    IOCTL_HELIOS_CTX_CREATE, IOCTL_HELIOS_CTX_DESTROY, IOCTL_HELIOS_MAP_BLOB,
    IOCTL_HELIOS_PRESENT_BLOB, IOCTL_HELIOS_SUBMIT_VENUS, IOCTL_HELIOS_WAIT_FENCE,
    VIRTIO_GPU_MAP_CACHE_CACHED, VIRTIO_GPU_MAP_CACHE_UNCACHED, VIRTIO_GPU_MAP_CACHE_WC,
};
use wdk_sys::ntddk::{
    IoAllocateMdl, IoFreeMdl, KeDelayExecutionThread, MmMapLockedPagesSpecifyCache,
    MmUnmapLockedPages,
};
use wdk_sys::{
    call_unsafe_wdf_function_binding, LARGE_INTEGER, MDL, NTSTATUS, PMDL, PVOID, ULONG, ULONG_PTR,
    WDFFILEOBJECT, WDFOBJECT, WDFQUEUE, WDFREQUEST, NT_SUCCESS, STATUS_DEVICE_DOES_NOT_EXIST,
    STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_PARAMETER,
    STATUS_IO_TIMEOUT, STATUS_SUCCESS, _MEMORY_CACHING_TYPE,
};

use crate::adapter::{adapter_of, AdapterContext};
use crate::virtio::gpu::{InFlight, MAX_INFLIGHT, SUBMIT_META_BYTES};
use crate::virtio::hal::DmaBuffer;
use crate::wdf::{KERNEL_MODE, USER_MODE};

// ── MDL / user-mapping constants (Phase 4c) ─────────────────────────────────
/// log2(page size). The host-visible window is mapped page-granular.
const PAGE_SHIFT: u32 = 12;
/// `MDL_PAGES_LOCKED` — set on a manually-built MDL describing device (BAR) pages,
/// which are not pageable, so `MmMapLockedPagesSpecifyCache` treats them as locked.
const MDL_PAGES_LOCKED: i16 = 0x0002;
/// `MDL_IO_SPACE` — the described frames are PCI-BAR (host-visible window) pages
/// that live in high MMIO ABOVE guest RAM and have NO entry in the guest PFN
/// database. Without this flag `MmMapLockedPagesSpecifyCache` would index
/// `MmPfnDatabase[pfn]` for its per-page lock bookkeeping → a wild out-of-range
/// access (bugcheck/corruption). The flag tells MM to bypass the PFN database and
/// build the user PTEs straight from our PFN array.
const MDL_IO_SPACE: i16 = 0x0800;
/// `NormalPagePriority` (`MM_PAGE_PRIORITY`) for `MmMapLockedPagesSpecifyCache`.
const NORMAL_PAGE_PRIORITY: u32 = 16;
/// `MdlMappingNoExecute` — map the user view non-executable (data-only blob).
const MDL_MAPPING_NO_EXECUTE: u32 = 0x4000_0000;

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
        IOCTL_HELIOS_MAP_BLOB => handle_map_blob(adapter, request),
        IOCTL_HELIOS_PRESENT_BLOB => handle_present_blob(adapter, request),
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
    // Per-call free list for any in-flight async submits reaped while quiescing
    // the control queue; dropped here at PASSIVE after with_virtio releases the lock.
    let mut retired: Vec<InFlight> = Vec::with_capacity(MAX_INFLIGHT);
    match adapter.with_virtio(|v| v.ctx_create(req.capset_id, &mut retired)) {
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
    let mut retired: Vec<InFlight> = Vec::with_capacity(MAX_INFLIGHT);
    match adapter.with_virtio(|v| v.ctx_destroy(req.ctx_id, &mut retired)) {
        Ok(Ok(())) => (STATUS_SUCCESS, 0),
        Ok(Err(ve)) => (ve.into(), 0),
        Err(de) => (de.into(), 0),
    }
}

/// `IOCTL_HELIOS_PRESENT_BLOB` → throwaway Phase-7 go/no-go gate (DISPLAY.md §8).
/// Bind a venus blob `resource_id` to scanout 0 (`SET_SCANOUT_BLOB`) and flush it
/// (`RESOURCE_FLUSH`) so the host displays it zero-copy under `-spice gl=on`. A
/// non-success return (the host rejecting the scanout because the resource has no
/// exported `dmabuf_fd`, `RESP_ERR_UNSPEC`) is the gate's *failure* signal — i.e.
/// the venus image wasn't created exportable. Input-only. Removed once the DOD's
/// real `HELIOS_PRESENT_BLOB` escape supersedes it.
unsafe fn handle_present_blob(adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let sz = size_of::<HeliosEscapePresentBlob>();
    let (in_ptr, _) = match input_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    // SAFETY: WDF guaranteed ≥ sz bytes at in_ptr. Read unaligned.
    let req: HeliosEscapePresentBlob = pod_read_unaligned(slice::from_raw_parts(in_ptr, sz));
    if req.resource_id == 0 || req.width == 0 || req.height == 0 {
        return (STATUS_INVALID_PARAMETER, 0);
    }
    let mut retired: Vec<InFlight> = Vec::with_capacity(MAX_INFLIGHT);
    // Both control commands run under the one virtio spinlock (DISPATCH, alloc-free).
    match adapter.with_virtio(|v| {
        v.set_scanout_blob(
            req.resource_id,
            req.width,
            req.height,
            req.format,
            req.stride,
            req.offset,
            &mut retired,
        )
        .and_then(|()| v.resource_flush(req.resource_id, req.width, req.height, &mut retired))
    }) {
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
    let mut retired: Vec<InFlight> = Vec::with_capacity(MAX_INFLIGHT);
    match adapter.with_virtio(|v| {
        v.alloc_blob(
            req.ctx_id,
            req.blob_mem,
            req.blob_flags,
            req.blob_id,
            req.size,
            &mut retired,
        )
    }) {
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

    // Allocate the two device-visible contiguous buffers the in-flight async
    // submission OWNS until the host completes it (PASSIVE_LEVEL, before the
    // lock): `meta` carries the SUBMIT_3D header + the response slot, `venus`
    // carries the command stream. The KMD must keep both alive past this IOCTL's
    // return because submit is non-blocking now — so they move into the in-flight
    // pool inside `submit_venus` and are freed only when the matching `pop_used`
    // reaps them (here, via the `retired` list, or in a later drain).
    let meta = match DmaBuffer::new(SUBMIT_META_BYTES) {
        Some(d) => d,
        None => return (STATUS_INSUFFICIENT_RESOURCES, 0),
    };
    let mut venus = match DmaBuffer::new(payload) {
        Some(d) => d,
        None => return (STATUS_INSUFFICIENT_RESOURCES, 0),
    };
    // SAFETY: WDF guaranteed ≥ payload readable bytes at venus_ptr.
    venus
        .as_mut_slice()
        .copy_from_slice(slice::from_raw_parts(venus_ptr, payload));

    let (ctx_id, fence_id, ring_idx) = (req.ctx_id, req.fence_id, req.ring_idx);
    // Pre-reserved free list for completions reaped during this submit (drain
    // before add + backpressure draining) AND for `meta`/`venus` on a submit
    // error path; freed below at PASSIVE.
    let mut retired: Vec<InFlight> = Vec::with_capacity(MAX_INFLIGHT);
    // `meta`/`venus` are MOVED into submit_venus (it parks them in the in-flight
    // pool on success, or hands them back via `retired` on error — it never drops
    // a DmaBuffer itself, since that would free contiguous memory at the
    // DISPATCH-level lock). `retired` is captured by &mut (the `move` only moves
    // the meta/venus bindings + the reference, not the Vec itself).
    let retired_ref = &mut retired;
    let status = match adapter.with_virtio(move |v| {
        v.submit_venus(ctx_id, fence_id, ring_idx, meta, venus, payload, retired_ref)
    }) {
        Ok(Ok(())) => STATUS_SUCCESS,
        Ok(Err(ve)) => ve.into(),
        Err(de) => de.into(),
    };
    // `retired` drops here at PASSIVE_LEVEL after the lock released, freeing every
    // reaped/handed-back DmaBuffer. If the transport was down (with_virtio Err),
    // `meta`/`venus` were moved into the closure and dropped when the closure was
    // dropped — but with_virtio drops the closure AFTER releasing the spinlock, so
    // that drop is also at PASSIVE. (See adapter.rs::with_virtio.)
    (status, 0)
}

/// Poll interval between used-ring re-checks in `handle_wait_fence`, in 100ns
/// units (negative = relative). 100µs keeps latency low without busy-spinning.
const WAIT_FENCE_POLL_INTERVAL_100NS: i64 = -1_000;
/// The same interval in nanoseconds, for deriving the timeout iteration count.
const WAIT_FENCE_POLL_INTERVAL_NS: u64 = 100_000;

/// `IOCTL_HELIOS_WAIT_FENCE` → block until `fence_id` completes or `timeout_ns`
/// elapses (Phase 4e). Submission is now asynchronous, so the host fence may not
/// be retired yet when this runs.
///
/// Poll-first model (no interrupt yet — see the interrupt blocker in the Phase-4e
/// handover): there is no DPC to signal a KEVENT, so this drives the used ring
/// itself. Each iteration reaps completions under the virtio lock
/// ([`VirtioGpu::fence_complete`]) and, if the target is not yet complete, sleeps
/// a short interval at PASSIVE_LEVEL before re-checking. `fence_complete` is
/// out-of-order-safe (a fence is done once it is submitted and no longer
/// in-flight), so this is correct even under per-`ring_idx` fence routing.
///
/// `timeout_ns == 0` → poll once (non-blocking). A huge value (venus passes
/// `UINT64_MAX`) is effectively infinite. On timeout this completes with
/// `STATUS_IO_TIMEOUT` — an *error* status (`!NT_SUCCESS`), so `DeviceIoControl`
/// returns FALSE and the ICD can tell timeout from completion (`STATUS_TIMEOUT`,
/// 0x102, is a success code and would be indistinguishable from completion).
unsafe fn handle_wait_fence(adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let sz = size_of::<HeliosEscapeWaitFence>();
    let (in_ptr, _) = match input_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    // SAFETY: ≥ sz bytes at in_ptr.
    let req: HeliosEscapeWaitFence = pod_read_unaligned(slice::from_raw_parts(in_ptr, sz));

    // Reaped completions land here each iteration and are freed (dropped) at
    // PASSIVE before the next sleep; the capacity is reused via `clear`.
    let mut retired: Vec<InFlight> = Vec::with_capacity(MAX_INFLIGHT);

    // Bound the poll loop by the timeout, counted in poll-interval steps. Each
    // KeDelayExecutionThread may sleep slightly LONGER than requested, so this
    // over-waits rather than ever timing out early (a false timeout would be the
    // dangerous direction). `timeout_ns == 0` → 0 sleeps → poll once. A huge
    // value (venus passes UINT64_MAX) yields an astronomically large count, i.e.
    // effectively infinite. (KeQueryInterruptTime is a header inline, not an
    // exported symbol, so it is unavailable to bindgen — hence step counting.)
    let max_sleeps = req.timeout_ns / WAIT_FENCE_POLL_INTERVAL_NS;
    let mut sleeps: u64 = 0;

    loop {
        let done = match adapter.with_virtio(|v| v.fence_complete(req.fence_id, &mut retired)) {
            Ok(Ok(d)) => d,
            Ok(Err(ve)) => {
                retired.clear();
                return (ve.into(), 0);
            }
            Err(de) => {
                retired.clear();
                return (de.into(), 0);
            }
        };
        // Free reaped buffers at PASSIVE (lock released); keep the capacity.
        retired.clear();
        if done {
            return (STATUS_SUCCESS, 0);
        }
        if sleeps >= max_sleeps {
            return (STATUS_IO_TIMEOUT, 0);
        }
        sleeps += 1;
        // Sleep a short interval at PASSIVE before re-polling the used ring.
        let mut interval: LARGE_INTEGER = core::mem::zeroed();
        interval.QuadPart = WAIT_FENCE_POLL_INTERVAL_100NS;
        // SAFETY: PASSIVE_LEVEL; non-alertable kernel-mode relative delay.
        let _ = KeDelayExecutionThread(KERNEL_MODE, 0, &mut interval);
    }
}

/// `IOCTL_HELIOS_MAP_BLOB` (METHOD_BUFFERED) → map a host-visible blob's pages into
/// the calling process and return the user VA (ARCH §6). The page mapping is the
/// side effect; the in/out buffer only carries the resource id in and the user VA
/// out.
///
/// IRQL split (the Phase 4c crux): the virtio `RESOURCE_MAP_BLOB` round-trip runs
/// under the AdapterContext virtio spinlock at DISPATCH_LEVEL ([`map_blob_prepare`]),
/// but `IoAllocateMdl` + `MmMapLockedPagesSpecifyCache(UserMode)` require
/// PASSIVE_LEVEL and map into the CALLING process — so they run *outside* the lock,
/// here in the user thread's context (WDF dispatches this default-parallel,
/// passive-level queue inline in the requesting thread). The mapping is recorded in
/// the AdapterContext mapping table and torn down in [`evt_file_cleanup`].
unsafe fn handle_map_blob(adapter: &AdapterContext, request: WDFREQUEST) -> (NTSTATUS, usize) {
    let sz = size_of::<HeliosEscapeMapBlob>();
    let (in_ptr, _) = match input_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    let (out_ptr, _) = match output_buffer(request, sz) {
        Ok(b) => b,
        Err(s) => return (s, 0),
    };
    // SAFETY: WDF guaranteed ≥ sz bytes at in_ptr/out_ptr. Read unaligned.
    let req: HeliosEscapeMapBlob = pod_read_unaligned(slice::from_raw_parts(in_ptr, sz));

    // The owning file object — each mapping is tagged with it so EvtFileCleanup
    // unmaps exactly this handle's mappings (and only in the process that created
    // them). File-object support is enabled (WdfDeviceInitSetFileObjectConfig), so
    // this is non-null for a normal IOCTL; reject defensively if it is not.
    // SAFETY: `request` is our IOCTL request handle.
    let file_object = call_unsafe_wdf_function_binding!(WdfRequestGetFileObject, request);
    if file_object.is_null() {
        return (STATUS_INVALID_DEVICE_REQUEST, 0);
    }

    // Reject a second map of an already-mapped resource: it would claim a second
    // window offset + leave a duplicate host mapping (the host typically rejects
    // re-mapping anyway). The ICD maps each blob once.
    if adapter.mappings.contains(req.resource_id) {
        return (STATUS_INVALID_DEVICE_REQUEST, 0);
    }

    // Phase 1 — under the virtio spinlock (DISPATCH): RESOURCE_MAP_BLOB at a fresh
    // window offset; returns the guest-physical range + host caching. Quiescing
    // the control queue inside map_blob_prepare may reap in-flight submits into
    // `retired`, which drops (frees) at PASSIVE at the end of this handler.
    let mut retired: Vec<InFlight> = Vec::with_capacity(MAX_INFLIGHT);
    let prep = match adapter.with_virtio(|v| v.map_blob_prepare(req.resource_id, &mut retired)) {
        Ok(Ok(p)) => p,
        Ok(Err(ve)) => return (ve.into(), 0),
        Err(de) => return (de.into(), 0),
    };
    // `IoAllocateMdl` length is a ULONG (u32); guard the (page-rounded) size. A
    // failure here leaks the just-claimed window offset (bump allocator, no reclaim)
    // and the host-side mapping — acceptable for bring-up (the ctx is torn down on
    // CTX_DESTROY); the ICD never maps a >4 GiB blob.
    if prep.size == 0 || prep.size > ULONG::MAX as u64 {
        return (STATUS_INVALID_PARAMETER, 0);
    }

    // Phase 2 — at PASSIVE_LEVEL, in the caller's process, holding NO lock: build
    // an MDL over the host-visible BAR pages and map it into user space.
    let cache = map_cache_to_mm(prep.map_cache);
    let (user_va, mdl) = match map_io_pages_to_user(prep.gpa, prep.size, cache) {
        Some(x) => x,
        None => return (STATUS_INSUFFICIENT_RESOURCES, 0),
    };

    // Phase 3 — record (tagged with the owning file object) for handle-close
    // teardown. If the table is full, undo the user mapping immediately (still in
    // the owning process at PASSIVE).
    if !adapter
        .mappings
        .insert(file_object as usize, req.resource_id, user_va, mdl as usize)
    {
        unmap_io_pages_from_user(user_va, mdl);
        return (STATUS_INSUFFICIENT_RESOURCES, 0);
    }

    let mut out = req;
    out.out_user_va = user_va;
    // SAFETY: ≥ sz writable bytes at out_ptr.
    slice::from_raw_parts_mut(out_ptr, sz).copy_from_slice(bytes_of(&out));
    (STATUS_SUCCESS, sz)
}

/// Translate a virtio-gpu `map_info` caching nibble to a Windows cache type.
fn map_cache_to_mm(map_cache: u32) -> _MEMORY_CACHING_TYPE::Type {
    match map_cache {
        VIRTIO_GPU_MAP_CACHE_CACHED => _MEMORY_CACHING_TYPE::MmCached,
        VIRTIO_GPU_MAP_CACHE_WC => _MEMORY_CACHING_TYPE::MmWriteCombined,
        VIRTIO_GPU_MAP_CACHE_UNCACHED => _MEMORY_CACHING_TYPE::MmNonCached,
        // NONE / unknown: the host expressed no preference. Cached matches the
        // common host-coherent venus heap; only end-to-end venus traffic (the ICD)
        // exercises non-CACHED values, at which point the host always sets one.
        _ => _MEMORY_CACHING_TYPE::MmCached,
    }
}

/// Build an MDL over the page-aligned guest-physical range `[gpa, gpa + size)` (a
/// span of the host-visible BAR — device I/O pages, NOT pageable RAM) and map it
/// into the CURRENT process's user address space. Returns `(user_va, mdl)`.
///
/// # Safety
/// Must be called at PASSIVE_LEVEL in the target process's context, holding no
/// spinlock. `gpa`/`size` must be page-aligned and name a valid host-injected
/// window range (from `RESOURCE_MAP_BLOB`). The PFNs are device pages not subject
/// to paging, so `MDL_PAGES_LOCKED` is the correct flag (no `MmProbeAndLockPages`).
///
/// ⚠️ HARDENING TODO (known, deferred): `MmMapLockedPagesSpecifyCache(UserMode)`
/// RAISES a structured exception on failure (VA exhaustion / quota / a WDK that
/// rejects an I/O-space user mapping) — it does NOT return NULL. This `no_std`
/// crate has no SEH, so such a failure unwinds unhandled → bugcheck. The proper fix
/// is a tiny C `__try/__except` shim wrapping this call (returns NULL on raise) so
/// the `is_null` path below becomes live. Until then the exposure is bounded by:
/// (a) the per-map size cap (`MAX_BLOB_MAP_BYTES`, gpu.rs) shrinking the failure
/// window, and (b) the sole caller being the trusted single-process ICD mapping a
/// modest blob into a fresh address space. Add the SEH shim before exposing
/// MAP_BLOB to any untrusted/multi-client caller. The `is_null` branch is kept as
/// defense-in-depth (and is live if a future kernel build returns NULL instead).
unsafe fn map_io_pages_to_user(
    gpa: u64,
    size: u64,
    cache: _MEMORY_CACHING_TYPE::Type,
) -> Option<(u64, PMDL)> {
    // SAFETY: VirtualAddress = NULL is valid for a manually-populated MDL; Length
    // is page-aligned so the PFN-array span is exactly `size >> PAGE_SHIFT`.
    let mdl = IoAllocateMdl(
        core::ptr::null_mut(),
        size as ULONG,
        0, // SecondaryBuffer = FALSE
        0, // ChargeQuota = FALSE
        core::ptr::null_mut(),
    );
    if mdl.is_null() {
        return None;
    }
    // The (device BAR) pages are inherently locked/non-pageable, and live in I/O
    // space (no PFN-database entry) — both flags are required, see their docs.
    (*mdl).MdlFlags |= MDL_PAGES_LOCKED | MDL_IO_SPACE;
    // The PFN array immediately follows the MDL header.
    let pfns = (mdl as *mut u8).add(size_of::<MDL>()) as *mut u64;
    let pages = (size >> PAGE_SHIFT) as usize;
    let pfn0 = gpa >> PAGE_SHIFT;
    for i in 0..pages {
        // SAFETY: `pfns[0..pages]` is the freshly-allocated PFN array sized for
        // `pages` entries by IoAllocateMdl.
        *pfns.add(i) = pfn0 + i as u64;
    }
    let priority = NORMAL_PAGE_PRIORITY | MDL_MAPPING_NO_EXECUTE;
    // SAFETY: `mdl` is a valid, populated, locked MDL; maps into the current
    // (user) process. BugCheckOnFailure = FALSE (ignored for UserMode — see the
    // exception note above).
    let va = MmMapLockedPagesSpecifyCache(
        mdl,
        USER_MODE,
        cache,
        core::ptr::null_mut(),
        0,
        priority,
    );
    if va.is_null() {
        // Unreached for UserMode (it raises rather than returning NULL), but freed
        // here for completeness if a future kernel build returns NULL.
        IoFreeMdl(mdl);
        return None;
    }
    Some((va as u64, mdl))
}

/// Unmap a user-space blob mapping made by [`map_io_pages_to_user`] and free its
/// MDL.
///
/// # Safety
/// Must run at PASSIVE_LEVEL in the SAME process the mapping was created in (the
/// process that opened the device handle); `user_va`/`mdl` must be a pair returned
/// by `map_io_pages_to_user` and not yet unmapped.
unsafe fn unmap_io_pages_from_user(user_va: u64, mdl: PMDL) {
    MmUnmapLockedPages(user_va as PVOID, mdl);
    IoFreeMdl(mdl);
}

/// `EvtFileCleanup` — runs when a user handle to the device interface is released,
/// at PASSIVE_LEVEL in the closing process's context. Unmaps and frees the
/// host-visible blob mappings created through THIS file object. The unmap MUST
/// happen here (in-process, before teardown) — `MmMapLockedPagesSpecifyCache(UserMode)`
/// mapped into this process, and leaving a mapping live at process exit bugchecks
/// `0x76 PROCESS_HAS_LOCKED_PAGES`.
///
/// This fires PER FILE OBJECT (once for each closed handle, not only the last), and
/// a user VA is valid only in the process that created it — so we drain ONLY this
/// file object's mappings (`take_one_for`), never another open handle's (which
/// would unmap a foreign process's VA → corruption / 0x76).
pub unsafe extern "C" fn evt_file_cleanup(file_object: WDFFILEOBJECT) {
    // SAFETY: `file_object` is our file object; WdfFileObjectGetDevice returns our
    // WDFDEVICE.
    let device = call_unsafe_wdf_function_binding!(WdfFileObjectGetDevice, file_object);
    let adapter = match adapter_of(device.cast::<core::ffi::c_void>() as WDFOBJECT) {
        Some(a) => a,
        None => return,
    };
    // Pop one of THIS handle's mappings at a time under the table's lock, then unmap
    // OUTSIDE the lock (MmUnmapLockedPages requires PASSIVE; the lock raises to
    // DISPATCH). One-at-a-time avoids collecting all entries on the stack (the 0x7F
    // lesson). Note: this path does NOT touch the virtio transport, so it is correct
    // even after ReleaseHardware has dropped it.
    let owner = file_object as usize;
    while let Some((user_va, mdl)) = adapter.mappings.take_one_for(owner) {
        unmap_io_pages_from_user(user_va, mdl as PMDL);
    }
}
