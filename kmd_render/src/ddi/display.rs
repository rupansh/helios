//! Display/VidPn DDIs for the active Helios render+display adapter.
//!
//! Windows identifies the primary through `SetVidPnSourceAddress`; the landed
//! D4 plane contract validates and queues that exact allocation directly.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

use helios_protocol::{HELIOS_WDDM_ALLOC_KIND_DEVICE_MEMORY, HELIOS_WDDM_ALLOC_KIND_STANDARD};

use crate::adapter::AdapterContext;
use crate::ddi::create_allocation::{present_alloc_info, PresentAllocationStorage};
use crate::ddi::present_packet::{
    MpoPresentRefusal, PatchCapacity, PresentAllocations, PresentMpoPayload, PresentPayload,
    PresentSubmissionPrivate, PRESENT_DMA_PACKET_BYTES, STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER,
};
use crate::device::ContextHandleRef;
use crate::dxgk::*;
use crate::virtio::venus::{OptimalPresentImageDesc, PresentBufferDesc, PresentDestinationDesc};
use crate::virtio::VirtioError;
use helios_kmd_logic::ScanoutFormat;
use wdk_sys::ntddk::KeGetCurrentIrql;

pub static PRESENT_COUNT: AtomicU32 = AtomicU32::new(0);
/// Drives the throttle for this DDI's IDENTITY dumps (`diag::sample_tick`).
/// Failure values — PBRet, PBCpy, PBSyWt, PBSyCp and PBFlip's error arms — are
/// never sampled; they stay unconditional.
///
/// ⚠ `PBCpy`/`PBFlip` **`0xEA`** is the newest of those and the one to expect
/// first after an allocation-association regression: it means
/// `present_alloc_info` answered `None`. It is counted by
/// `create_allocation::PRESENT_NO_ALLOC_INFO`, because a last-value breadcrumb
/// cannot distinguish "once" from "every frame". Do not read this as `0xE1`,
/// which means dxgkrnl handed us a handle we could not resolve. The two shared one
/// value until round 3 of the Phase-2 review separated them.
static PRESENT_TRACE_TICK: AtomicU32 = AtomicU32::new(0);
/// Drives the independent success-result mirror. `PBRet` used to perform a
/// synchronous registry write for every successful Present, directly on the
/// render thread. Non-success statuses remain immediate; successful results
/// need only the same first/every-600th live refresh as the identity block.
static PRESENT_RESULT_TRACE_TICK: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_SRC_COUNT: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_DST_COUNT: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_DMA_SIZE: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_PATCH_SIZE: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_SRC_OPEN_LOW: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_DST_OPEN_LOW: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_FLAGS: AtomicU32 = AtomicU32::new(0);
pub static PRESENT_LAST_STATUS: AtomicU32 = AtomicU32::new(0);
static D5_DISABLED_OWNER_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_MPO_INFO_POINTER_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_PLANE_LIST_COUNT_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_PLANE_LIST_POINTER_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_SOURCE_OR_LAYER_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_DISABLED_OR_MALFORMED_PLANE_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_RESERVED_BITS_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_OPEN_ALLOCATION_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_ALLOCATION_PROFILE_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_OUTPUT_CAPACITY_REFUSALS: AtomicU32 = AtomicU32::new(0);
static D5_PACKET_CONSTRUCTION_REFUSALS: AtomicU32 = AtomicU32::new(0);

fn refuse_mpo_present(refusal: MpoPresentRefusal) -> NTSTATUS {
    let (counter, name, status) = match refusal {
        MpoPresentRefusal::DisabledOwnerBoundary => (
            &D5_DISABLED_OWNER_REFUSALS,
            b"D5Disabled".as_slice(),
            STATUS_NOT_SUPPORTED,
        ),
        MpoPresentRefusal::MpoInfoPointer => (
            &D5_MPO_INFO_POINTER_REFUSALS,
            b"D5InfoPtr".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
        MpoPresentRefusal::PlaneListCount => (
            &D5_PLANE_LIST_COUNT_REFUSALS,
            b"D5PlaneCnt".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
        MpoPresentRefusal::PlaneListPointer => (
            &D5_PLANE_LIST_POINTER_REFUSALS,
            b"D5PlanePtr".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
        MpoPresentRefusal::SourceOrLayer => (
            &D5_SOURCE_OR_LAYER_REFUSALS,
            b"D5SrcLayer".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
        MpoPresentRefusal::DisabledOrMalformedPlane => (
            &D5_DISABLED_OR_MALFORMED_PLANE_REFUSALS,
            b"D5PlaneBad".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
        MpoPresentRefusal::ReservedBits => (
            &D5_RESERVED_BITS_REFUSALS,
            b"D5Reserved".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
        MpoPresentRefusal::OpenAllocation => (
            &D5_OPEN_ALLOCATION_REFUSALS,
            b"D5OpenBad".as_slice(),
            STATUS_INVALID_HANDLE,
        ),
        MpoPresentRefusal::AllocationIdentityOrProfile => (
            &D5_ALLOCATION_PROFILE_REFUSALS,
            b"D5Profile".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
        MpoPresentRefusal::OutputCapacity => (
            &D5_OUTPUT_CAPACITY_REFUSALS,
            b"D5Capacity".as_slice(),
            STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER,
        ),
        MpoPresentRefusal::PacketConstruction => (
            &D5_PACKET_CONSTRUCTION_REFUSALS,
            b"D5Packet".as_slice(),
            STATUS_INVALID_PARAMETER,
        ),
    };
    let count = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    crate::diag::record_named_bytes(name, count);
    PRESENT_LAST_STATUS.store(status as u32, Ordering::Relaxed);
    status
}

unsafe fn present_mpo_d5(args: &mut DXGKARG_PRESENT, payload: PresentMpoPayload) -> NTSTATUS {
    // This is the real D5 authority boundary. Outside the exact WDDM 3.2/D2
    // package, the raw MPO pointer carried by `payload` is not dereferenced and
    // no allocation is resolved.
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return refuse_mpo_present(MpoPresentRefusal::DisabledOwnerBoundary);
    }

    // SAFETY: the selected union arm came from this live Present;
    // `prepare_mpo_present` checks every pointer before reference formation
    // and performs no writes.
    let plan = match unsafe { payload.prepare_mpo_present(args) } {
        Ok(plan) => plan,
        Err(refusal) => return refuse_mpo_present(refusal),
    };
    // SAFETY: `plan` proves every output capacity for this same argument and
    // leaves no fallible work after the first output byte is changed.
    unsafe { plan.emit_mpo_present(args) };
    PRESENT_LAST_STATUS.store(STATUS_SUCCESS as u32, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// Non-cloneable capability proving that the classic SetVidPn callback is
/// currently executing above DISPATCH_LEVEL in the classic WDK callback.
///
/// Its constructor is private to this module and is called only by the classic
/// DDI. The sole consumer is the fixed D4 queue operation; no general transport
/// borrow, allocator, waiter, spinlock guard, or cleanup API accepts it. DIRQL
/// does not itself prove ownership of dxgkrnl's interrupt lock; the queue takes
/// its own bounded, nonblocking exclusion gate.
pub(crate) struct SetVidPnDirql<'a> {
    adapter: *const AdapterContext,
    _scope: PhantomData<&'a AdapterContext>,
    _not_send: PhantomData<*mut ()>,
}

impl<'a> SetVidPnDirql<'a> {
    const DISPATCH_LEVEL_IRQL: u8 = 2;

    fn mint(adapter: &'a AdapterContext) -> Option<Self> {
        // SAFETY: KeGetCurrentIrql is callable at every IRQL.
        (unsafe { KeGetCurrentIrql() } > Self::DISPATCH_LEVEL_IRQL).then_some(Self {
            adapter,
            _scope: PhantomData,
            _not_send: PhantomData,
        })
    }

    pub(crate) fn authorizes(&self, adapter: &AdapterContext) -> bool {
        core::ptr::eq(self.adapter, adapter)
            // SAFETY: KeGetCurrentIrql is callable at every IRQL. This second
            // check catches a proof illicitly retained past the callback even
            // though the lifetime and !Send marker already prevent safe code
            // from doing so.
            && unsafe { KeGetCurrentIrql() } > Self::DISPATCH_LEVEL_IRQL
    }
}

/// Pin `kmd_logic`'s hand-written virtio values to the wire constants.
///
/// `kmd_logic` is deliberately dependency-free (it has to build under a host
/// libtest harness), so it spells the three `VIRTIO_GPU_FORMAT_*` values as
/// literals. This is where they are checked against the single source of truth;
/// a drift is a build failure, not a black scan-out.
const _: () = {
    assert!(ScanoutFormat::Bgra8.virtio() == helios_protocol::VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM);
    assert!(ScanoutFormat::Bgrx8.virtio() == helios_protocol::VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM);
    assert!(ScanoutFormat::Rgba8.virtio() == helios_protocol::VIRTIO_GPU_FORMAT_R8G8B8A8_UNORM);
};

pub fn diag_dump_present_atomics() {
    crate::diag::record(0x1320_0000 | (PRESENT_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1321_0000 | (PRESENT_LAST_SRC_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1322_0000 | (PRESENT_LAST_DST_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1323_0000 | (PRESENT_LAST_DMA_SIZE.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1324_0000 | (PRESENT_LAST_PATCH_SIZE.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1325_0000 | (PRESENT_LAST_SRC_OPEN_LOW.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1326_0000 | (PRESENT_LAST_DST_OPEN_LOW.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1327_0000 | (PRESENT_LAST_FLAGS.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x1328_0000 | (PRESENT_LAST_STATUS.load(Ordering::Relaxed) & 0xFFFF));
}

pub unsafe extern "C" fn dxgkddi_present(
    h_context: IN_CONST_HANDLE,
    present: INOUT_PDXGKARG_PRESENT,
) -> NTSTATUS {
    let status = unsafe { dxgkddi_present_inner(h_context, present) };
    // Fixed-name telemetry survives the steady-state registry ring flood and
    // proves whether a failing UMD pfnPresentCb originated in this DDI. A
    // failure is never delayed. Successful frames update the same name on the
    // first call and every SAMPLE_EVERY calls (or every call at DiagLevel=1),
    // avoiding one synchronous RtlWriteRegistryValue in every production
    // Present while leaving the status ABI and debug cadence unchanged.
    if status != STATUS_SUCCESS || crate::diag::sample_tick(&PRESENT_RESULT_TRACE_TICK) {
        crate::diag::record_named_bytes(b"PBRet", status as u32);
    }
    status
}

unsafe fn dxgkddi_present_inner(
    h_context: IN_CONST_HANDLE,
    present: INOUT_PDXGKARG_PRESENT,
) -> NTSTATUS {
    PRESENT_COUNT.fetch_add(1, Ordering::Relaxed);
    if present.is_null() {
        PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }

    // `pfnPresentCb` drives this DDI. Validate the exact allocations supplied by
    // dxgkrnl. BLTs produce a scheduler submission below; MMIO flips do not
    // generate a DMA buffer and are completed through SetVidPnSourceAddress.
    let args = unsafe { &mut *present };
    PRESENT_LAST_SRC_COUNT.store(args.NumSrcAllocations, Ordering::Relaxed);
    PRESENT_LAST_DST_COUNT.store(args.NumDstAllocations, Ordering::Relaxed);
    PRESENT_LAST_DMA_SIZE.store(args.DmaSize, Ordering::Relaxed);
    PRESENT_LAST_PATCH_SIZE.store(args.PatchLocationListOutSize, Ordering::Relaxed);
    let present_flags = unsafe { args.Flags.__bindgen_anon_1.Value };
    PRESENT_LAST_FLAGS.store(present_flags, Ordering::Relaxed);

    // Decode the payload union arm ONCE, from the flags, before anything reads
    // it. Both this DDI and PresentToHwQueue used to pick `pAllocationList`
    // implicitly out of a three-arm union.
    // SAFETY: `args` is dxgkrnl's present struct; the arm its flags name is the
    // one it initialised.
    let payload = unsafe { PresentPayload::decode(args) };
    let allocation_list = match payload {
        PresentPayload::AllocationList(list) => list,
        PresentPayload::MultiPlaneOverlay(mpo) => {
            return unsafe { present_mpo_d5(args, mpo) };
        }
    };
    let payload_has_list = allocation_list.is_present();
    // SAFETY: `allocation_list` came from `PresentPayload::decode`, so it is the
    // fixed present allocation array.
    let present_allocations = unsafe { PresentAllocations::from_allocation_list(&allocation_list) };

    // The patch-capacity proof, acquired before any host GPU work on the BLT
    // path and consumed by the single write below.
    let mut patch_capacity: Option<PatchCapacity> = None;

    // Present-path IDENTITY trace, SAMPLED (R316). These are per-call dumps of
    // flags / counts / sizes that mattered during bring-up; at 60 Hz they were
    // ~8 synchronous kernel registry writes per frame before the surface block
    // below added ~40 more. One tick gates the whole DDI's identity output, so a
    // sampled frame is internally consistent, and `DiagLevel >= 1` restores the
    // per-call cadence.
    let sample = crate::diag::sample_tick(&PRESENT_TRACE_TICK);
    if sample {
        crate::diag::record_named_bytes(b"PBcall", PRESENT_COUNT.load(Ordering::Relaxed));
        crate::diag::record_named_bytes(b"PBflag", present_flags);
        crate::diag::record_named_bytes(
            b"PBcnt",
            (args.NumSrcAllocations << 16) | (args.NumDstAllocations & 0xFFFF),
        );
        crate::diag::record_named_bytes(b"PBalst", u32::from(payload_has_list));
        crate::diag::record_named_bytes(b"PBDma", args.DmaSize);
        crate::diag::record_named_bytes(b"PBPatch", args.PatchLocationListOutSize);
    }

    if sample {
        crate::diag::record_named_bytes(b"PBkpsz", args.DmaBufferPrivateDataSize);
    }
    let present_context = unsafe { ContextHandleRef::from_raw(h_context) };
    let adapter = present_context.as_ref().and_then(ContextHandleRef::adapter);
    let src_handle = present_allocations
        .source()
        .map(|allocation| allocation.handle())
        .unwrap_or(core::ptr::null_mut());
    let dst_handle = present_allocations
        .destination()
        .map(|allocation| allocation.handle())
        .unwrap_or(core::ptr::null_mut());
    let src_info = unsafe { present_alloc_info(src_handle) };
    let dst_info = unsafe { present_alloc_info(dst_handle) };
    if payload_has_list {
        PRESENT_LAST_SRC_OPEN_LOW.store(src_handle as usize as u32, Ordering::Relaxed);
        PRESENT_LAST_DST_OPEN_LOW.store(dst_handle as usize as u32, Ordering::Relaxed);

        // Present-blit feasibility trace (read-only), SAMPLED. Resolves the
        // composition source + destination surfaces to their venus resource ids
        // and geometry, and reports whether each is a tracked
        // host-visible-mappable blob. 38 registry writes plus two blob_lookup
        // round-trips under the device lock — per present, for values that change
        // only when the surface set changes. Note this block runs for the FLIP
        // shape too (the flip arm reads src_info, resolved from the allocation
        // list), so it was genuinely per-frame.
        if sample {
            // Trace-only identity, resolved ONLY here — inside the sampling gate.
            let src_diag = unsafe { crate::ddi::create_allocation::present_alloc_diag(src_handle) };
            let dst_diag = unsafe { crate::ddi::create_allocation::present_alloc_diag(dst_handle) };
            crate::diag::record_named_bytes(b"PBsrcH", (src_handle as usize as u32) & 0xFFFF);
            crate::diag::record_named_bytes(b"PBdstH", (dst_handle as usize as u32) & 0xFFFF);
            if let Some(s) = src_info {
                if let Some(dg) = src_diag {
                    crate::diag::record_named_bytes(b"PBsRtA", dg.runtime_allocation);
                }
                crate::diag::record_named_bytes(b"PBsrc", s.resource_id);
                crate::diag::record_named_bytes(b"PBsw", s.width);
                crate::diag::record_named_bytes(b"PBsh", s.height);
                crate::diag::record_named_bytes(b"PBsPch", s.pitch);
                crate::diag::record_named_bytes(b"PBsFmt", s.dxgi_format);
                crate::diag::record_named_bytes(b"PBsD3F", s.format);
                crate::diag::record_named_bytes(b"PBsBnd", s.bind_flags);
                crate::diag::record_named_bytes(b"PBsSto", s.storage as u32);
                crate::diag::record_named_bytes(b"PBsKnd", s.kind);
                crate::diag::record_named_bytes(b"PBsMt", s.memory_type_index);
                crate::diag::record_named_bytes(b"PBsSz", s.venus_alloc_size as u32);
                if let Some(dg) = src_diag {
                    crate::diag::record_named_bytes(b"PBsStd", dg.standard_allocation_type);
                    crate::diag::record_named_bytes(b"PBsGdi", dg.standard_gdi_surface_type);
                    crate::diag::record_named_bytes(b"PBsOF", dg.open_flags);
                    crate::diag::record_named_bytes(b"PBsRA", u32::from(dg.resource_associated));
                    crate::diag::record_named_bytes(b"PBsAPS", dg.allocation_private_size);
                    crate::diag::record_named_bytes(b"PBsRPS", dg.resource_private_size);
                }
                let lk = adapter
                    .and_then(|adapter| adapter.canonical_blob_lookup(s.resource_id).ok());
                // 0=untracked, else 0x1_0000 | (mapped<<8) | (size in 4KiB pages, low byte)
                let code = match lk {
                    Some(Some((_owner, size, mapped))) => {
                        0x0001_0000 | ((mapped as u32) << 8) | ((size / 4096) as u32 & 0xFF)
                    }
                    _ => 0,
                };
                crate::diag::record_named_bytes(b"PBstrk", code);
            } else {
                crate::diag::record_named_bytes(b"PBsrc", 0);
            }
            if let Some(d) = dst_info {
                if let Some(dg) = dst_diag {
                    crate::diag::record_named_bytes(b"PBdRtA", dg.runtime_allocation);
                }
                crate::diag::record_named_bytes(b"PBdst", d.resource_id);
                crate::diag::record_named_bytes(b"PBdw", d.width);
                crate::diag::record_named_bytes(b"PBdh", d.height);
                crate::diag::record_named_bytes(b"PBdPch", d.pitch);
                crate::diag::record_named_bytes(b"PBdFmt", d.dxgi_format);
                crate::diag::record_named_bytes(b"PBdD3F", d.format);
                crate::diag::record_named_bytes(b"PBdBnd", d.bind_flags);
                crate::diag::record_named_bytes(b"PBdSto", d.storage as u32);
                crate::diag::record_named_bytes(b"PBdKnd", d.kind);
                crate::diag::record_named_bytes(b"PBdMt", d.memory_type_index);
                crate::diag::record_named_bytes(b"PBdSz", d.venus_alloc_size as u32);
                if let Some(dg) = dst_diag {
                    crate::diag::record_named_bytes(b"PBdStd", dg.standard_allocation_type);
                    crate::diag::record_named_bytes(b"PBdGdi", dg.standard_gdi_surface_type);
                    crate::diag::record_named_bytes(b"PBdOF", dg.open_flags);
                    crate::diag::record_named_bytes(b"PBdRA", u32::from(dg.resource_associated));
                    crate::diag::record_named_bytes(b"PBdAPS", dg.allocation_private_size);
                    crate::diag::record_named_bytes(b"PBdRPS", dg.resource_private_size);
                }
                let lk = adapter
                    .and_then(|adapter| adapter.canonical_blob_lookup(d.resource_id).ok());
                let code = match lk {
                    Some(Some((_owner, size, mapped))) => {
                        0x0001_0000 | ((mapped as u32) << 8) | ((size / 4096) as u32 & 0xFF)
                    }
                    _ => 0,
                };
                crate::diag::record_named_bytes(b"PBdtrk", code);
            } else {
                crate::diag::record_named_bytes(b"PBdst", 0);
            }
        }

        // DXGK_PRESENTFLAGS.Blt is bit 0. Dxgkrnl has already resolved both
        // fixed allocation-list entries to our typed open handles. Perform the
        // actual full-surface source -> destination copy before emitting the
        // scheduler marker; a no-op Present leaves DWM's shared render target
        // black even though the application's source rendered correctly.
        if present_flags & 1 != 0 {
            let bytes = PRESENT_DMA_PACKET_BYTES as UINT;
            if args.pDmaBuffer.is_null() || args.DmaSize < bytes {
                PRESENT_LAST_STATUS.store(
                    STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER as u32,
                    Ordering::Relaxed,
                );
                return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
            }
            if args.pDmaBufferPrivateData.is_null()
                || (args.DmaBufferPrivateDataSize as usize)
                    < core::mem::size_of::<PresentSubmissionPrivate>()
            {
                PRESENT_LAST_STATUS.store(
                    STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER as u32,
                    Ordering::Relaxed,
                );
                return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
            }
            // BEFORE any host GPU work: an insufficient-buffer retry must not
            // be able to duplicate the BLT below. The token is carried to the
            // single write site rather than dropped, so the ordering is a
            // type-level fact on this path and not a convention.
            match present_allocations.validate_patch_capacity(args) {
                Ok(capacity) => patch_capacity = Some(capacity),
                Err(status) => {
                    PRESENT_LAST_STATUS.store(status as u32, Ordering::Relaxed);
                    return status;
                }
            }

            // ⚠ THE A3 GAP REACHING THE DISPLAY PATH, separated from the
            // handle-lifetime failure it used to be indistinguishable from.
            //
            // `present_alloc_info` answers `None` for every allocation while
            // `PresentAllocationStorage` has no producer (K4-CONTRACT §5: HWA2
            // carries no host resource id, so `DxgkDdiOpenAllocation` has
            // nothing to build one from and refuses to fabricate one). So this
            // arm is taken on EVERY present, and it used to report `0xE1` —
            // whose meaning here is "dxgkrnl handed us a handle we could not
            // resolve", a handle-lifetime bug. An operator would have chased the
            // wrong defect. `0xEA` is the A3 arm and
            // `create_allocation::PRESENT_NO_ALLOC_INFO` (`PrNoRid`) counts it,
            // because a last-value breadcrumb cannot say whether this fired once
            // or once per frame.
            let alloc_info_absent = src_info.is_none() || dst_info.is_none();
            let (Some(adapter), Some(source), Some(destination)) = (adapter, src_info, dst_info)
            else {
                if alloc_info_absent {
                    crate::ddi::create_allocation::PRESENT_NO_ALLOC_INFO
                        .fetch_add(1, Ordering::Relaxed);
                    crate::diag::record_named_bytes(b"PBCpy", 0xEA);
                } else {
                    crate::diag::record_named_bytes(b"PBCpy", 0xE1);
                }
                PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            };
            let source_dxgi_format = source.resolved_dxgi_format();
            let destination_dxgi_format = destination.resolved_dxgi_format();
            let (Some(source_dxgi_format), Some(destination_dxgi_format)) =
                (source_dxgi_format, destination_dxgi_format)
            else {
                crate::diag::record_named_bytes(b"PBCpy", 0xE2);
                PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            };
            if source.kind != HELIOS_WDDM_ALLOC_KIND_DEVICE_MEMORY {
                crate::diag::record_named_bytes(b"PBCpy", 0xE6);
                PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            }
            let source_desc = match source.storage {
                PresentAllocationStorage::OptimalCrossContextImage => {
                    OptimalPresentImageDesc::new_cross_context_dma_buf(
                        source.resource_id,
                        source.venus_alloc_size,
                        source.memory_type_index,
                        source.width,
                        source.height,
                        source.bind_flags,
                        source_dxgi_format,
                    )
                }
                PresentAllocationStorage::OptimalOpaqueFdImage => {
                    OptimalPresentImageDesc::new_opaque_fd(
                        source.resource_id,
                        source.venus_alloc_size,
                        source.memory_type_index,
                        source.width,
                        source.height,
                        source.bind_flags,
                        source_dxgi_format,
                    )
                }
                PresentAllocationStorage::PitchedStandardBuffer => None,
            };
            let destination_desc = match destination.storage {
                PresentAllocationStorage::PitchedStandardBuffer
                    if destination.kind == HELIOS_WDDM_ALLOC_KIND_STANDARD =>
                {
                    PresentBufferDesc::new(
                        destination.resource_id,
                        destination.venus_alloc_size,
                        destination.memory_type_index,
                        destination.width,
                        destination.height,
                        destination.pitch,
                        destination_dxgi_format,
                    )
                    .map(PresentDestinationDesc::StandardBuffer)
                }
                PresentAllocationStorage::OptimalCrossContextImage => {
                    OptimalPresentImageDesc::new_cross_context_dma_buf(
                        destination.resource_id,
                        destination.venus_alloc_size,
                        destination.memory_type_index,
                        destination.width,
                        destination.height,
                        destination.bind_flags,
                        destination_dxgi_format,
                    )
                    .map(PresentDestinationDesc::OptimalImage)
                }
                PresentAllocationStorage::OptimalOpaqueFdImage => {
                    OptimalPresentImageDesc::new_opaque_fd(
                        destination.resource_id,
                        destination.venus_alloc_size,
                        destination.memory_type_index,
                        destination.width,
                        destination.height,
                        destination.bind_flags,
                        destination_dxgi_format,
                    )
                    .map(PresentDestinationDesc::OptimalImage)
                }
                PresentAllocationStorage::PitchedStandardBuffer => None,
            };
            let (Some(source_desc), Some(destination_desc)) = (source_desc, destination_desc)
            else {
                crate::diag::record_named_bytes(b"PBCpy", 0xE2);
                PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            };
            if source.width != destination.width || source.height != destination.height {
                crate::diag::record_named_bytes(b"PBCpy", 0xE3);
                PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            }

            {
                // SAFETY: `DxgkDdiPresent` is documented "IRQL: PASSIVE_LEVEL" (WDK
                // DXGKDDI_PRESENT) — it is a pageable DDI, and the BLT arm below
                // waits on a wire fence and maps blob bytes, neither of which is
                // legal above PASSIVE. Note this is the BLT arm only; the MMIO-flip
                // arm generates no DMA buffer and is completed through
                // SetVidPnSourceAddress, whose DIRQL half holds no token at all.
                let passive = unsafe { crate::irql::PassiveLevel::assume() };
                let copy = adapter.with_venus_client(passive, |client| {
                    client.submit_present_blt(adapter, source_desc, destination_desc)
                });
                let gpu_fence = match copy {
                    Ok(Ok(fence)) => fence,
                    Ok(Err(VirtioError::OutOfMemory | VirtioError::QueueFull)) => {
                        crate::diag::record_named_bytes(b"PBCpy", 0xE4);
                        PRESENT_LAST_STATUS.store(STATUS_NO_MEMORY as u32, Ordering::Relaxed);
                        return STATUS_NO_MEMORY;
                    }
                    Ok(Err(_)) | Err(_) => {
                        crate::diag::record_named_bytes(b"PBCpy", 0xE5);
                        PRESENT_LAST_STATUS
                            .store(STATUS_DEVICE_NOT_READY as u32, Ordering::Relaxed);
                        return STATUS_DEVICE_NOT_READY;
                    }
                };
                crate::diag::record_named_bytes(
                    b"PBConv",
                    u32::from(source_dxgi_format != destination_dxgi_format),
                );
                // Windows can page a lockable standard staging destination from
                // the BAR/Venus allocation into system memory and keep DWM's CPU
                // view there. BuildPagingBuffer records that exact MDL-page
                // association by resource id. Once this Venus copy completes,
                // mirror into those pages before Present retires; otherwise later
                // frames update only the stale BAR blob.
                let has_system_backing = adapter.system_backings.contains(destination.resource_id);
                if has_system_backing {
                    match crate::virtio::ctrl::wait_fence(
                        passive,
                        adapter,
                        gpu_fence,
                        5_000_000_000,
                    ) {
                        crate::virtio::ctrl::WaitFenceOutcome::Complete => {
                            crate::diag::record_named_bytes(b"PBSyWt", 1);
                        }
                        crate::virtio::ctrl::WaitFenceOutcome::TimedOut => {
                            crate::diag::record_named_bytes(b"PBSyWt", 0xE1);
                            PRESENT_LAST_STATUS
                                .store(STATUS_DEVICE_NOT_READY as u32, Ordering::Relaxed);
                            return STATUS_DEVICE_NOT_READY;
                        }
                        crate::virtio::ctrl::WaitFenceOutcome::Invalid => {
                            crate::diag::record_named_bytes(b"PBSyWt", 0xE2);
                            PRESENT_LAST_STATUS
                                .store(STATUS_DEVICE_NOT_READY as u32, Ordering::Relaxed);
                            return STATUS_DEVICE_NOT_READY;
                        }
                    }
                }
                if has_system_backing {
                    match unsafe {
                        crate::ddi::build_paging_buffer::mirror_present_system_backing(
                            passive,
                            adapter,
                            destination.resource_id,
                        )
                    } {
                        Some(true) => crate::diag::record_named_bytes(b"PBSyCp", 1),
                        // Windows may page the allocation back to the BAR between
                        // the pre-check and completed fence. With no system
                        // backing, the Venus destination is authoritative again.
                        None => crate::diag::record_named_bytes(b"PBSyCp", 2),
                        Some(false) => {
                            crate::diag::record_named_bytes(b"PBSyCp", 0xE1);
                            PRESENT_LAST_STATUS
                                .store(STATUS_DEVICE_NOT_READY as u32, Ordering::Relaxed);
                            return STATUS_DEVICE_NOT_READY;
                        }
                    }
                } else {
                    crate::diag::record_named_bytes(b"PBSyCp", 0);
                }
                // Capacity was checked before host work was queued, so this cannot
                // fail. Merge preserves the newest fence if dxgkrnl batches more
                // than one Present into the same DMA private-data buffer.
                if let Err(status) = unsafe {
                    PresentSubmissionPrivate::merge_fence(
                        args.pDmaBufferPrivateData,
                        args.DmaBufferPrivateDataSize,
                        gpu_fence,
                    )
                } {
                    crate::diag::record_named_bytes(b"PBCpy", 0xE6);
                    PRESENT_LAST_STATUS.store(status as u32, Ordering::Relaxed);
                    return status;
                }
                // NOT sampled: PBCpy is the value a failed Present is read from
                // (its 0xE1..0xE6 arms), so its success arm has to keep the same
                // cadence or "last PBCpy" stops meaning "what the last BLT did".
                crate::diag::record_named_bytes(b"PBCpy", 1);
                crate::diag::record_named_bytes(b"PBFnc", gpu_fence as u32);
            }
        }
    }

    if present_flags & (1 << 2) != 0 {
        if adapter.is_none() {
            PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        }
        // DXGK_PRESENTFLAGS.Flip is an allocation-identity handoff, not a
        // no-op. The source slot contains the exact
        // hDeviceSpecificAllocation that dxgkrnl opened on this device. Select
        // scanout only from that Windows-owned handle and the immutable
        // private-data snapshot captured by OpenAllocation. In particular, do
        // not let the UMD command payload independently select a resource.
        // ⚠ As the BLT arm above: this is the A3 gap, not a handle-lifetime
        // failure, and it is the arm every DWM flip now takes. `0xEA` + `PrNoRid`
        // (`create_allocation::PRESENT_NO_ALLOC_INFO`) so the two are
        // distinguishable and countable; `0xE1` stays reserved for its original
        // meaning even though nothing can currently reach it here.
        let Some(source) = src_info else {
            crate::ddi::create_allocation::PRESENT_NO_ALLOC_INFO.fetch_add(1, Ordering::Relaxed);
            crate::diag::record_named_bytes(b"PBFlip", 0xEA);
            PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        };
        let Some(_dxgi_format) = source.resolved_dxgi_format() else {
            crate::diag::record_named_bytes(b"PBFlip", 0xE2);
            PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        };
        // Flip identity, SAMPLED (the 0xEA/0xE2 failure arms above stay
        // unconditional — those are the values a failed Present is read from).
        if sample {
            crate::diag::record_named_bytes(b"PBsrc", source.resource_id);
            crate::diag::record_named_bytes(b"PBsw", source.width);
            crate::diag::record_named_bytes(b"PBsh", source.height);
            crate::diag::record_named_bytes(b"PBsDir", u32::from(source.direct_scanout));
        }

        // The FLIP itself (epoch stamp, VidMm physical address, CRTC_VSYNC
        // retirement) always belongs to the allocation-list source. D4 removes
        // the former snapshot bind-target override; the Render-command stash is
        // still consumed only by the unrelated legacy BLT arm above.

        // It must not program scanout here: dxgkrnl subsequently names the
        // allocation that actually reached the VidPn source through
        // SetVidPnSourceAddress. DWM can legitimately compose this Present
        // source into a different managed primary, so publishing both creates
        // two competing selectors and lets retirement of the transient source
        // tear down the real desktop scanout.
        if sample {
            crate::diag::record_named_bytes(b"PBFlip", 1);
        }

        // FlipOnVSyncMmIo explicitly requires DxgkDdiPresent to generate no DMA
        // buffer. In that contract dxgkrnl passes pDmaBuffer == NULL and later
        // supplies the authoritative allocation/address to
        // SetVidPnSourceAddress. There is consequently no DMA address to patch:
        // the allocation-list source above is validation/identity input only.
        // Rejecting this zero-sized call as a depleted buffer makes the UMD's
        // otherwise valid pfnPresentCb fail before the VidPn handoff can occur.
        if args.pDmaBuffer.is_null() {
            crate::diag::record_named_bytes(b"PBMmio", 1);
            args.MultipassOffset = 0;
            PRESENT_LAST_STATUS.store(STATUS_SUCCESS as u32, Ordering::Relaxed);
            return STATUS_SUCCESS;
        }

        // DMA-BUFFER FLIP. dxgkrnl gave us a DMA buffer, which means it will
        // NOT call SetVidPnSourceAddress for this flip — the driver programs
        // the display when the buffer executes, and the submission fence is
        // what tells dxgkrnl the flip happened. Record the exact allocation and
        // the address dxgkrnl assigned it; `submit_command` picks both up and
        // arms the same deferred programming the MMIO path arms, and the flip's
        // DMA fence then retires behind it instead of ahead of it.
        //
        // The allocation list is the ONLY source for that address here (see
        // `PresentAllocation::physical_address`), which is why it is captured
        // now rather than resolved later.
        let flip_source = match present_allocations.source() {
            Some(allocation) => allocation,
            None => {
                crate::diag::record_named_bytes(b"PBFlip", 0xE4);
                PRESENT_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            }
        };
        // Preserve the exact device-specific open handle supplied in this
        // allocation-list slot. The DISPATCH flip arm resolves it through the
        // per-open canonical allocation association; no resource-id reverse
        // lookup, current-primary shortcut, or snapshot substitution remains.
        let flip_allocation = flip_source.handle();
        if let Err(status) = unsafe {
            crate::ddi::present_packet::PresentFlipPrivate::write(
                args.pDmaBufferPrivateData,
                args.DmaBufferPrivateDataSize,
                flip_allocation,
                flip_source.physical_address(),
                flip_source.segment_id(),
                0,
            )
        } {
            crate::diag::record_named_bytes(b"PBFlip", 0xE5);
            PRESENT_LAST_STATUS.store(status as u32, Ordering::Relaxed);
            return status;
        }
    }

    // The BLT path carried its token here; every other path queues nothing, so
    // acquiring immediately before the write is correct and says so.
    let capacity = match patch_capacity {
        Some(capacity) => capacity,
        None => match present_allocations.validate_patch_capacity(args) {
            Ok(capacity) => capacity,
            Err(status) => {
                PRESENT_LAST_STATUS.store(status as u32, Ordering::Relaxed);
                return status;
            }
        },
    };
    if let Err(status) = unsafe { present_allocations.write_patch_references(capacity, args) } {
        PRESENT_LAST_STATUS.store(status as u32, Ordering::Relaxed);
        return status;
    }

    if !args.pDmaBuffer.is_null() {
        let bytes = PRESENT_DMA_PACKET_BYTES as UINT;
        if args.DmaSize < bytes {
            PRESENT_LAST_STATUS.store(
                STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER as u32,
                Ordering::Relaxed,
            );
            return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
        }
        unsafe {
            // Keep the DMA record structurally non-empty. Its bytes carry no
            // identity; allocation references and the K9 boundary are separate
            // dxgkrnl-owned inputs.
            core::ptr::write_unaligned(args.pDmaBuffer.cast::<u32>(), 0);
            args.pDmaBuffer = (args.pDmaBuffer as *mut u8).add(bytes as usize).cast();
        }
        args.MultipassOffset = 0;
    }

    PRESENT_LAST_STATUS.store(STATUS_SUCCESS as u32, Ordering::Relaxed);
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_set_pointer_position(
    _adapter: IN_CONST_HANDLE,
    position: IN_CONST_PDXGKARG_SETPOINTERPOSITION,
) -> NTSTATUS {
    crate::diag::record(0x1300_0002);
    if !position.is_null() {
        crate::diag::record(0x1310_0000 | unsafe { (*position).VidPnSourceId & 0xFFFF });
    }
    // SetPointerPosition's legal set does NOT include STATUS_NOT_SUPPORTED — an
    // illegal return here is logged as a driver bug during the modeset (AzureTriage,
    // 36th session). With the display half up, accept the (software-cursor) position
    // as a no-op; render-only never receives this call.
    if unsafe { display_half_on(_adapter) } {
        STATUS_SUCCESS
    } else {
        STATUS_NOT_SUPPORTED
    }
}

pub unsafe extern "C" fn dxgkddi_set_pointer_shape(
    _adapter: IN_CONST_HANDLE,
    shape: IN_CONST_PDXGKARG_SETPOINTERSHAPE,
) -> NTSTATUS {
    crate::diag::record(0x1300_0003);
    if !shape.is_null() {
        crate::diag::record(0x1311_0000 | unsafe { (*shape).VidPnSourceId & 0xFFFF });
    }
    // As with SetPointerPosition: NOT_SUPPORTED is illegal for this DDI. Accept as a
    // no-op with the display half up (the OS software-composes the cursor).
    if unsafe { display_half_on(_adapter) } {
        STATUS_SUCCESS
    } else {
        STATUS_NOT_SUPPORTED
    }
}

/// True when the `DisplayHalf` knob was on at StartDevice (Option A, #1). The
/// VidPn DDIs stay NOT_SUPPORTED (render-only) unless this returns true.
///
/// # Safety
/// `h` is the miniport adapter handle dxgkrnl passes to a display DDI.
unsafe fn display_half_on(h: IN_CONST_HANDLE) -> bool {
    let p = h as *const AdapterContext;
    !p.is_null() && unsafe { (*p).display_half() }
}

pub unsafe extern "C" fn dxgkddi_is_supported_vidpn(
    _adapter: IN_CONST_HANDLE,
    is_supported: INOUT_PDXGKARG_ISSUPPORTEDVIDPN,
) -> NTSTATUS {
    crate::diag::record(0x1300_0004);
    if !is_supported.is_null() {
        crate::diag::record(
            0x1312_0000 | unsafe { ((*is_supported).hDesiredVidPn as usize as u32) & 0xFFFF },
        );
    }
    let p = _adapter as *const AdapterContext;
    if p.is_null() || !unsafe { (*p).display_half() } {
        return STATUS_GRAPHICS_INVALID_VIDPN;
    }
    if is_supported.is_null() {
        return STATUS_GRAPHICS_INVALID_VIDPN;
    }
    // Diagnostic: latch the max path count across every VidPn the OS validates.
    // `VpISp`>=1 ⇒ the OS DOES propose a 1-path VidPn we accept — so an empty COMMIT
    // (VpCN=0) means the OS rejects activation *after* our TRUE (flip/scanout/MPO
    // side), not a topology-synthesis gap. `VpISp`=0 ⇒ it only ever asks about the
    // empty VidPn (a topology/target problem persists).
    let adapter = unsafe { &*p };
    let pc =
        unsafe { crate::ddi::vidpn::topology_path_count(adapter, (*is_supported).hDesiredVidPn) };
    if pc != u32::MAX {
        static MAX_PC: AtomicU32 = AtomicU32::new(0);
        MAX_PC.fetch_max(pc, Ordering::Relaxed);
        crate::diag::record_named_bytes(b"VpISp", MAX_PC.load(Ordering::Relaxed));
    }
    // A single source + single target adapter can only ever be handed the
    // trivial (or empty) VidPn, so accept it.
    unsafe { (*is_supported).IsVidPnSupported = 1 };
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_recommend_functional_vidpn(
    _adapter: IN_CONST_HANDLE,
    _recommend: IN_CONST_PDXGKARG_RECOMMENDFUNCTIONALVIDPN_CONST,
) -> NTSTATUS {
    crate::diag::record(0x1300_0005);
    if !unsafe { display_half_on(_adapter) } {
        return STATUS_NOT_SUPPORTED;
    }
    // Decline: let the OS synthesize the simple one-path VidPn it then validates
    // via IsSupportedVidPn (enumerating-child-devices-of-a-display-adapter.md).
    crate::ddi::vidpn::STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN
}

pub unsafe extern "C" fn dxgkddi_enum_vidpn_cofunc_modality(
    _adapter: IN_CONST_HANDLE,
    enum_modality: IN_CONST_PDXGKARG_ENUMVIDPNCOFUNCMODALITY_CONST,
) -> NTSTATUS {
    crate::diag::record(0x1300_0006);
    if !enum_modality.is_null() {
        crate::diag::record(
            0x1313_0000 | unsafe { (*enum_modality).EnumPivotType as u32 & 0xFFFF },
        );
    }
    let p = _adapter as *const AdapterContext;
    if p.is_null() || !unsafe { (*p).display_half() } {
        return STATUS_NOT_SUPPORTED;
    }
    let adapter = unsafe { &*p };
    unsafe { crate::ddi::vidpn::enum_cofunc_modality(adapter, enum_modality) }
}

pub unsafe extern "C" fn dxgkddi_set_vidpn_source_visibility(
    _adapter: IN_CONST_HANDLE,
    visibility: IN_CONST_PDXGKARG_SETVIDPNSOURCEVISIBILITY,
) -> NTSTATUS {
    crate::diag::record(0x1300_0007);
    if !visibility.is_null() {
        crate::diag::record(0x1314_0000 | unsafe { (*visibility).VidPnSourceId & 0xFFFF });
    }
    let p = _adapter as *const AdapterContext;
    if p.is_null() || !unsafe { (*p).display_half() } {
        return STATUS_NOT_SUPPORTED;
    }
    if crate::virtio::KMD_D2_OWNER_ENABLED {
        if visibility.is_null() {
            return STATUS_INVALID_PARAMETER;
        }
        // SAFETY: this DDI is PASSIVE_LEVEL and dxgkrnl owns the argument for
        // the call. D2 consumes the exact source and visibility bit only.
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        let visibility = unsafe { &*visibility };
        return crate::ddi::direct_scanout::transition_visibility(
            passive,
            unsafe { &*p },
            visibility.VidPnSourceId,
            visibility.Visible != 0,
        );
    }
    // Legacy production behavior remains the accepted no-op.
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_commit_vidpn(
    _adapter: IN_CONST_HANDLE,
    commit: IN_CONST_PDXGKARG_COMMITVIDPN_CONST,
) -> NTSTATUS {
    crate::diag::record(0x1300_0008);
    if !commit.is_null() {
        crate::diag::record(0x1315_0000 | unsafe { (*commit).AffectedVidPnSourceId & 0xFFFF });
        crate::diag::record(0x1316_0000 | unsafe { (*commit).Flags.PathPoweredOff() & 0xFFFF });
    }
    let p = _adapter as *const AdapterContext;
    if p.is_null() || !unsafe { (*p).display_half() } {
        return STATUS_NOT_SUPPORTED;
    }
    crate::diag::record_named_bytes(b"VpCM", 1);
    // Inspect + validate the committed VidPn and record whether the OS pinned a
    // source mode on our source (the mode-set-retry-loop resolver — a bare
    // `return SUCCESS` that never checks the pin is exactly viogpu3d's "commit but
    // light nothing" failure). Scanout itself is issued from SetVidPnSourceAddress.
    let adapter = unsafe { &*p };
    if crate::virtio::KMD_D2_OWNER_ENABLED {
        // SAFETY: CommitVidPn is PASSIVE and `commit` remains live for this call.
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        let facts = match unsafe {
            crate::ddi::vidpn::inspect_committed_vidpn(
                adapter,
                passive,
                commit as *const DXGKARG_COMMITVIDPN,
            )
        } {
            Ok(facts) => facts,
            Err(error) => return error.status,
        };
        let status = crate::ddi::vidpn::legalize_vidpn(unsafe {
            crate::ddi::vidpn::commit_vidpn(adapter, commit as *const DXGKARG_COMMITVIDPN)
        });
        if status != STATUS_SUCCESS {
            return status;
        }
        return crate::ddi::direct_scanout::commit_mode(passive, adapter, facts);
    }
    crate::ddi::vidpn::legalize_vidpn(unsafe {
        crate::ddi::vidpn::commit_vidpn(adapter, commit as *const DXGKARG_COMMITVIDPN)
    })
}

pub unsafe extern "C" fn dxgkddi_update_active_vidpn_present_path(
    _adapter: IN_CONST_HANDLE,
    path: IN_CONST_PDXGKARG_UPDATEACTIVEVIDPNPRESENTPATH_CONST,
) -> NTSTATUS {
    crate::diag::record(0x1300_0009);
    if !path.is_null() {
        crate::diag::record(
            0x1317_0000 | unsafe { (*path).VidPnPresentPathInfo.VidPnSourceId & 0xFFFF },
        );
        crate::diag::record(
            0x1318_0000 | unsafe { (*path).VidPnPresentPathInfo.VidPnTargetId & 0xFFFF },
        );
    }
    if unsafe { display_half_on(_adapter) } {
        STATUS_SUCCESS
    } else {
        STATUS_NOT_SUPPORTED
    }
}

pub unsafe extern "C" fn dxgkddi_set_vidpn_source_address(
    _adapter: IN_CONST_HANDLE,
    address: IN_CONST_PDXGKARG_SETVIDPNSOURCEADDRESS,
) -> NTSTATUS {
    let p = _adapter as *const AdapterContext;
    if p.is_null() || !unsafe { (*p).display_half() } {
        return STATUS_NOT_SUPPORTED;
    }
    let adapter = unsafe { &*p };
    if crate::virtio::KMD_D2_OWNER_ENABLED {
        return unsafe { set_vidpn_source_address_d4(adapter, address) };
    }
    STATUS_NOT_SUPPORTED
}

const CLASSIC_MODE_CHANGE: u32 = 0x0000_0001;
const CLASSIC_FLIP_IMMEDIATE: u32 = 0x0000_0002;
const CLASSIC_FLIP_ON_NEXT_VSYNC: u32 = 0x0000_0004;
const CLASSIC_STEREO_MASK: u32 = 0x0000_0038;
const CLASSIC_SHARED_PRIMARY_TRANSITION: u32 = 0x0000_0040;
const CLASSIC_INDEPENDENT_FLIP_EXCLUSIVE: u32 = 0x0000_0080;
const CLASSIC_SUPPORTED_FLAG_MASK: u32 = CLASSIC_MODE_CHANGE
    | CLASSIC_FLIP_IMMEDIATE
    | CLASSIC_FLIP_ON_NEXT_VSYNC
    | CLASSIC_STEREO_MASK
    | CLASSIC_SHARED_PRIMARY_TRANSITION
    | CLASSIC_INDEPENDENT_FLIP_EXCLUSIVE;
const _: () = assert!(CLASSIC_SUPPORTED_FLAG_MASK == 0x0000_00ff);

static D4_CLASSIC_ACCEPTS: AtomicU32 = AtomicU32::new(0);
static D4_CLASSIC_REFUSALS: AtomicU32 = AtomicU32::new(0);

fn d4_queue_status(error: crate::virtio::VirtioError) -> NTSTATUS {
    match error {
        crate::virtio::VirtioError::QueueFull
        | crate::virtio::VirtioError::BindSequenceExhausted => STATUS_DEVICE_BUSY,
        _ => STATUS_IO_DEVICE_ERROR,
    }
}

/// Owner-enabled classic source switch. Its above-DISPATCH arm has one bounded
/// call graph: exact immutable validation -> move-only candidate -> fixed queue
/// publication. No PASSIVE token or general transport/adapter lock is
/// nameable from that arm.
unsafe fn set_vidpn_source_address_d4(
    adapter: &AdapterContext,
    address: IN_CONST_PDXGKARG_SETVIDPNSOURCEADDRESS,
) -> NTSTATUS {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_NOT_SUPPORTED;
    }
    if address.is_null()
        || !(address as *const DXGKARG_SETVIDPNSOURCEADDRESS).is_aligned()
    {
        D4_CLASSIC_REFUSALS.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let args = unsafe { &*address };
    let flags = unsafe { args.Flags.__bindgen_anon_1.Value };
    let operation = crate::ddi::direct_scanout::DirectScanoutOperation {
        immediate_flip: flags & CLASSIC_FLIP_IMMEDIATE != 0,
        stereo: flags & CLASSIC_STEREO_MASK != 0,
        shared_primary_transition: flags & CLASSIC_SHARED_PRIMARY_TRANSITION != 0,
        independent_flip_exclusive: flags & CLASSIC_INDEPENDENT_FLIP_EXCLUSIVE != 0,
        unsupported_or_reserved_flags: flags & !CLASSIC_SUPPORTED_FLAG_MASK,
    };
    let candidate = match unsafe {
        crate::ddi::direct_scanout::validate_direct_scanout_binding(
            adapter,
            args.hAllocation,
            args.VidPnSourceId,
            operation,
            helios_kmd_logic::direct_scanout_admission::PlaneFacts::Classic,
        )
    } {
        Ok(candidate) => candidate,
        Err(status) => {
            D4_CLASSIC_REFUSALS.fetch_add(1, Ordering::Relaxed);
            return status;
        }
    };
    let work = crate::ddi::direct_scanout::QueuedDirectScanoutBinding::from_exact_os_transition(
        candidate,
        args.VidPnSourceId,
        args.PrimarySegment,
        args.PrimaryAddress.QuadPart as u64,
        flags,
    );
    let queued = if let Some(proof) = SetVidPnDirql::mint(adapter) {
        // SAFETY: this is the sole proof mint and it is nested directly in the
        // WDK classic callback. The proof grants only the nonblocking fixed-slot
        // gateway; it does not claim an implicit dxgkrnl interrupt lock.
        unsafe { adapter.enqueue_d4_scanout_dirql(&proof, work) }
    } else {
        adapter.enqueue_d4_scanout_dispatch(work)
    };
    match queued {
        Ok(()) => {
            D4_CLASSIC_ACCEPTS.fetch_add(1, Ordering::Relaxed);
            STATUS_SUCCESS
        }
        Err(error) => {
            D4_CLASSIC_REFUSALS.fetch_add(1, Ordering::Relaxed);
            d4_queue_status(error)
        }
    }
}

/// Arm the deferred scan-out programming for the DMA-BUFFER FLIP contract.
///
/// The MMIO path reaches the same state through
/// [`set_vidpn_source_address_dirql`]; this is the entry point for flips
/// dxgkrnl never calls `SetVidPnSourceAddress` for, and it is called from the
/// submit path when the flip's DMA buffer is handed to the scheduler.
///
/// The DIFFERENCE THAT MATTERS is not in here, it is in the caller: on the MMIO
/// path the DDI returns STATUS_SUCCESS immediately and dxgkrnl treats that as
/// the flip having happened, so the programming this arms lands after dxgkrnl
/// has already moved on. Here the flip's DMA fence is still outstanding, so
/// dxgkrnl is still waiting when the programming runs.
///
/// Returns false if the handle could not be paired, in which case the gate is
/// NOT raised.
///
/// # Safety
/// `h_alloc` is the device-specific open handle dxgkrnl placed in the present
/// allocation list and this driver copied into kernel-only DMA private data.
pub(crate) unsafe fn arm_dma_flip_programming(
    adapter: &AdapterContext,
    h_alloc: HANDLE,
    primary_segment: u32,
    primary_address: u64,
    operation_flags: u32,
) -> bool {
    if crate::virtio::KMD_D2_OWNER_ENABLED {
        return unsafe {
            arm_dma_flip_d4(
                adapter,
                h_alloc,
                primary_segment,
                primary_address,
                operation_flags,
            )
        };
    }
    false
}

unsafe fn arm_dma_flip_d4(
    adapter: &AdapterContext,
    h_open_allocation: HANDLE,
    primary_segment: u32,
    primary_address: u64,
    operation_flags: u32,
) -> bool {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return false;
    }
    let Some((allocation, _facts)) = (unsafe {
        crate::ddi::create_allocation::open_direct_scanout_allocation_facts(h_open_allocation)
    }) else {
        D4_CLASSIC_REFUSALS.fetch_add(1, Ordering::Relaxed);
        return false;
    };
    let operation = crate::ddi::direct_scanout::DirectScanoutOperation {
        immediate_flip: operation_flags & CLASSIC_FLIP_IMMEDIATE != 0,
        stereo: operation_flags & CLASSIC_STEREO_MASK != 0,
        shared_primary_transition: operation_flags & CLASSIC_SHARED_PRIMARY_TRANSITION != 0,
        independent_flip_exclusive: operation_flags & CLASSIC_INDEPENDENT_FLIP_EXCLUSIVE != 0,
        unsupported_or_reserved_flags: operation_flags & !CLASSIC_SUPPORTED_FLAG_MASK,
    };
    let candidate = match unsafe {
        crate::ddi::direct_scanout::validate_direct_scanout_binding(
            adapter,
            allocation,
            0,
            operation,
            helios_kmd_logic::direct_scanout_admission::PlaneFacts::Classic,
        )
    } {
        Ok(candidate) => candidate,
        Err(_) => {
            D4_CLASSIC_REFUSALS.fetch_add(1, Ordering::Relaxed);
            return false;
        }
    };
    let work = crate::ddi::direct_scanout::QueuedDirectScanoutBinding::from_exact_os_transition(
        candidate,
        0,
        primary_segment,
        primary_address,
        operation_flags,
    );
    match adapter.enqueue_d4_scanout_dispatch(work) {
        Ok(()) => {
            D4_CLASSIC_ACCEPTS.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(_) => {
            D4_CLASSIC_REFUSALS.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

pub(crate) fn record_scanout_reject_counters() {
    crate::diag::record_named_bytes(
        b"D4ClsOk",
        D4_CLASSIC_ACCEPTS.load(Ordering::Relaxed),
    );
    crate::diag::record_named_bytes(
        b"D4ClsRef",
        D4_CLASSIC_REFUSALS.load(Ordering::Relaxed),
    );
    crate::ddi::direct_scanout::record_refusal_counters();
}

pub(crate) fn reset_scanout_reject_counters() {
    D4_CLASSIC_ACCEPTS.store(0, Ordering::Relaxed);
    D4_CLASSIC_REFUSALS.store(0, Ordering::Relaxed);
    crate::ddi::direct_scanout::reset_refusal_counters();
    record_scanout_reject_counters();
}

pub unsafe extern "C" fn dxgkddi_recommend_monitor_modes(
    _adapter: IN_CONST_HANDLE,
    _recommend: IN_CONST_PDXGKARG_RECOMMENDMONITORMODES_CONST,
) -> NTSTATUS {
    crate::diag::record(0x1300_000B);
    let p = _adapter as *const AdapterContext;
    if p.is_null() || !unsafe { (*p).display_half() } {
        return STATUS_NOT_SUPPORTED;
    }
    let adapter = unsafe { &*p };
    // Clamp to the DDI's legal return set: an out-of-contract NTSTATUS makes
    // dxgkrnl discard every VidPn (AzureTriage; 36th-session 0-paths root cause).
    crate::ddi::vidpn::legalize_vidpn(unsafe {
        crate::ddi::vidpn::recommend_monitor_modes(adapter, _recommend)
    })
}

pub unsafe extern "C" fn dxgkddi_query_vidpn_hw_capability(
    _adapter: IN_CONST_HANDLE,
    caps: INOUT_PDXGKARG_QUERYVIDPNHWCAPABILITY,
) -> NTSTATUS {
    crate::diag::record(0x1300_000C);
    if !unsafe { display_half_on(_adapter) } {
        return STATUS_NOT_SUPPORTED;
    }
    if caps.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // Advertise no HW interpolation/enhancements: the OS handles scaling/rotation
    // in software (there is no real scanout engine to do it).
    unsafe {
        core::ptr::write_bytes(
            core::ptr::addr_of_mut!((*caps).VidPnHWCaps) as *mut u8,
            0,
            core::mem::size_of::<D3DKMDT_VIDPN_HW_CAPABILITY>(),
        );
    }
    STATUS_SUCCESS
}

/// `DxgkDdiUpdateMonitorLinkInfo` — MANDATORY (non-null) once the adapter reports
/// a monitor target: dxgkrnl's StartAdapter fails the whole adapter with
/// `StartAdapter_DxgkDdiUpdateMonitorLinkInfoIsNull` (Code 43 FAILED_POST_START,
/// AzureTriage-confirmed 2026-07-08) if this slot is NULL. The virtual monitor
/// exposes no special link capabilities (no HDR/DSC/link-rate constraints), so we
/// accept and leave the caller's `MonitorLinkInfo` unchanged.
pub unsafe extern "C" fn dxgkddi_update_monitor_link_info(
    _adapter: IN_CONST_HANDLE,
    link_info: INOUT_PDXGKARG_UPDATEMONITORLINKINFO,
) -> NTSTATUS {
    crate::diag::record(0x1300_0010);
    if !link_info.is_null() {
        crate::diag::record(0x131D_0000 | unsafe { (*link_info).VideoPresentTargetId & 0xFFFF });
    }
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_get_scan_line(
    _adapter: IN_CONST_HANDLE,
    scan_line: INOUT_PDXGKARG_GETSCANLINE,
) -> NTSTATUS {
    crate::diag::record(0x1300_000D);
    if !scan_line.is_null() {
        crate::diag::record(0x131A_0000 | unsafe { (*scan_line).VidPnTargetId & 0xFFFF });
    }
    // NOT_SUPPORTED is illegal for GetScanLine. With the display half up, report a
    // benign "in vertical blank" (no real scanout engine to read a line from).
    if unsafe { display_half_on(_adapter) } {
        if !scan_line.is_null() {
            unsafe {
                (*scan_line).InVerticalBlank = 1;
                (*scan_line).ScanLine = 0;
            }
        }
        STATUS_SUCCESS
    } else {
        STATUS_NOT_SUPPORTED
    }
}

pub unsafe extern "C" fn dxgkddi_stop_device_and_release_post_display_ownership(
    _miniport_device_context: *mut c_void,
    target_id: D3DDDI_VIDEO_PRESENT_TARGET_ID,
    _display_info: PDXGK_DISPLAY_INFORMATION,
) -> NTSTATUS {
    crate::diag::record(0x1300_000E);
    crate::diag::record(0x131B_0000 | (target_id & 0xFFFF));
    STATUS_NOT_SUPPORTED
}

pub unsafe extern "C" fn dxgkddi_system_display_enable(
    _miniport_device_context: *mut c_void,
    target_id: D3DDDI_VIDEO_PRESENT_TARGET_ID,
    _flags: PDXGKARG_SYSTEM_DISPLAY_ENABLE_FLAGS,
    _width: *mut UINT,
    _height: *mut UINT,
    _color_format: *mut D3DDDIFORMAT,
) -> NTSTATUS {
    crate::diag::record(0x1300_000F);
    crate::diag::record(0x131C_0000 | (target_id & 0xFFFF));
    STATUS_NOT_SUPPORTED
}

pub unsafe extern "C" fn dxgkddi_system_display_write(
    _miniport_device_context: *mut c_void,
    _source: *mut c_void,
    _source_width: UINT,
    _source_height: UINT,
    _source_stride: UINT,
    _position_x: UINT,
    _position_y: UINT,
) {
}

pub unsafe extern "C" fn dxgkddi_exchange_pre_start_info(
    _adapter: IN_CONST_HANDLE,
    pre_start_info: IN_OUT_PDXGK_PRE_START_INFO,
) -> NTSTATUS {
    // 0x0E10_* = ExchangePreStartInfo. Entry was 0x0E00_0001, which device.rs
    // also records for DestroyDevice entry; the 0x0E00_* block is the
    // device-teardown family (0x0E01 owning handle, 0x0E02 blob-table size,
    // 0x0E03 reclaim counts, 0x0E04 ALLOC_BLOB owner), so this one moves out of
    // it rather than the other way round.
    crate::diag::record(0x0E10_0001);
    if pre_start_info.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    crate::diag::record(0x0E10_0002);
    STATUS_SUCCESS
}
