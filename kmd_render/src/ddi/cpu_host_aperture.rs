//! `DxgkDdiMapCpuHostAperture` / `DxgkDdiUnmapCpuHostAperture`.
//!
//! This is the CPU side of "a segment-2 allocation's content IS its venus
//! blob". When dxgkrnl needs CPU access to one — `D3DKMTLock2`, `pfnLockCb`,
//! win32k GDI raster — it calls here with the aperture pages IT chose inside
//! our declared window, and we host-map the allocation's blob at exactly that
//! window offset. The CPU VAs dxgkrnl then builds over
//! `aperture_gpa + page*4K` read and write THE BLOB BYTES: one memory, shared
//! with the host renderer.
//!
//! ⚠ Without this the segment is accounting only, and a CPU-visible allocation
//! has no view of its own content — which is exactly the state K1 left the
//! driver in when it deleted this file and K2, which was to rebuild the CPU
//! view, was never started.
//!
//! A blob maps at ONE window offset, whole-blob, so only whole-allocation,
//! consecutive-page requests can be served; any other shape is refused loudly
//! (`ChE*`) rather than silently mis-mapped. Null-succeeding would let the CPU
//! read and write UNBACKED window offsets — dropped writes, 0xFF reads — which
//! is the silent-content-loss class this exists to kill.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use wdk_sys::ntddk::KeGetCurrentIrql;

use crate::adapter::AdapterContext;
use crate::ddi::create_allocation::{paging_alloc_info, PagingAllocInfo};
use crate::ddi::PASSIVE_LEVEL_IRQL;
use crate::dxgk::*;

pub static CPU_HOST_MAP_COUNT: AtomicU32 = AtomicU32::new(0);
pub static CPU_HOST_UNMAP_COUNT: AtomicU32 = AtomicU32::new(0);

static BAR_AP_MAPS: AtomicU32 = AtomicU32::new(0);
static BAR_AP_UNMAPS: AtomicU32 = AtomicU32::new(0);
static BAR_AP_LAST_RESID: AtomicU32 = AtomicU32::new(0);
// Any nonzero value below is a design gap to chase, not noise.
static BAR_AP_ERR_IRQL: AtomicU32 = AtomicU32::new(0);
static BAR_AP_ERR_ALLOC: AtomicU32 = AtomicU32::new(0);
static BAR_AP_ERR_PARTIAL: AtomicU32 = AtomicU32::new(0);
static BAR_AP_ERR_SPARSE: AtomicU32 = AtomicU32::new(0);
static BAR_AP_ERR_BOUNDS: AtomicU32 = AtomicU32::new(0);
static BAR_AP_ERR_MAP: AtomicU32 = AtomicU32::new(0);
static BAR_AP_ERR_UNRESOLVED_UNMAP: AtomicU32 = AtomicU32::new(0);
static BAR_AP_IRQL_ACK: AtomicU32 = AtomicU32::new(0);
static BAR_AP_IRQL_DEFER: AtomicU32 = AtomicU32::new(0);

/// Mirror the counters into the registry ring. PASSIVE only — the raised-IRQL
/// arms may bump atomics but must never touch the registry.
pub fn diag_dump_cpu_host_atomics() {
    for (name, counter) in [
        (&b"ChMc"[..], &CPU_HOST_MAP_COUNT),
        (&b"ChUc"[..], &CPU_HOST_UNMAP_COUNT),
        (&b"ChMap"[..], &BAR_AP_MAPS),
        (&b"ChUnm"[..], &BAR_AP_UNMAPS),
        (&b"ChRes"[..], &BAR_AP_LAST_RESID),
        (&b"ChEi"[..], &BAR_AP_ERR_IRQL),
        (&b"ChEa"[..], &BAR_AP_ERR_ALLOC),
        (&b"ChEp"[..], &BAR_AP_ERR_PARTIAL),
        (&b"ChEs"[..], &BAR_AP_ERR_SPARSE),
        (&b"ChEb"[..], &BAR_AP_ERR_BOUNDS),
        (&b"ChEm"[..], &BAR_AP_ERR_MAP),
        (&b"ChEu"[..], &BAR_AP_ERR_UNRESOLVED_UNMAP),
        (&b"ChIa"[..], &BAR_AP_IRQL_ACK),
        (&b"ChId"[..], &BAR_AP_IRQL_DEFER),
    ] {
        crate::diag::record_named_bytes(name, counter.load(Ordering::Relaxed));
    }
}

/// An aperture request proven whole-allocation, consecutive and in-bounds.
/// Constructible only by [`validate_aperture_request`], so a future path cannot
/// answer an unvalidated request — it cannot produce the token.
#[derive(Clone, Copy)]
struct ValidatedApertureRange {
    offset: u64,
}

#[derive(Clone, Copy)]
enum ApertureRefusal {
    Partial,
    NotWholeAllocation,
    Sparse,
    Bounds,
}

impl ApertureRefusal {
    /// Atomics only, so the raised-IRQL arm can report a refusal at all.
    fn count(self, resource_id: u32) {
        match self {
            Self::Partial | Self::NotWholeAllocation => &BAR_AP_ERR_PARTIAL,
            Self::Sparse => &BAR_AP_ERR_SPARSE,
            Self::Bounds => &BAR_AP_ERR_BOUNDS,
        }
        .fetch_add(1, Ordering::Relaxed);
        BAR_AP_LAST_RESID.store(resource_id, Ordering::Relaxed);
    }
}

/// The ONE aperture-request validation rule, for both IRQL paths.
///
/// ⚠ It used to exist twice and the raised-IRQL copy was incomplete: it checked
/// the page count and whether page 0 matched, but never that the remaining
/// pages were consecutive. A request of `[P, P+7, P+8, …]` was ACKNOWLEDGED,
/// telling dxgkrnl the whole range was backed while pages 1..n-1 addressed
/// foreign window offsets. One rule, both callers.
///
/// # Safety
/// `pCpuHostAperturePages` must hold `NumberOfPages` entries for the call.
/// Reading them above PASSIVE is sound: the array is dxgkrnl-owned and must be
/// non-paged, since this DDI is callable above PASSIVE.
unsafe fn validate_aperture_request(
    args: &DXGKARG_MAPCPUHOSTAPERTURE,
    alloc: &PagingAllocInfo,
    window_len: u64,
) -> Result<ValidatedApertureRange, ApertureRefusal> {
    let n = args.NumberOfPages;
    if n == 0 || args.pCpuHostAperturePages.is_null() {
        return Err(ApertureRefusal::Partial);
    }
    // Whole-allocation only: a blob maps at one offset, whole-blob, so a
    // partial map would spill its pages over neighbouring assignments.
    let blob_pages = (alloc.size.saturating_add(4095) >> 12).max(1);
    if n != blob_pages {
        return Err(ApertureRefusal::NotWholeAllocation);
    }
    // SAFETY: n entries exist per the fn contract.
    let page0 = unsafe { core::ptr::read_unaligned(args.pCpuHostAperturePages) } as u64;
    for i in 1..n {
        // SAFETY: as above, i < n.
        let p =
            unsafe { core::ptr::read_unaligned(args.pCpuHostAperturePages.add(i as usize)) } as u64;
        if p != page0 + i {
            return Err(ApertureRefusal::Sparse);
        }
    }
    let offset = page0 << 12;
    if offset.saturating_add(n << 12) > window_len {
        return Err(ApertureRefusal::Bounds);
    }
    Ok(ValidatedApertureRange { offset })
}

/// # Safety
/// dxgkrnl passes its own adapter context and a valid argument block.
pub unsafe extern "C" fn dxgkddi_map_cpu_host_aperture(
    h_adapter: *mut c_void,
    map: IN_CONST_PDXGKARG_MAPCPUHOSTAPERTURE,
) -> NTSTATUS {
    if h_adapter.is_null() || map.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract.
    let args = unsafe { &*map };
    CPU_HOST_MAP_COUNT.fetch_add(1, Ordering::Relaxed);
    // SAFETY: dxgkrnl hands back our AdapterContext as the miniport context.
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };

    let Some(bar) = adapter
        .bar_segment()
        .filter(|b| b.seg_id == args.SegmentId as u32)
    else {
        // Not our aperture-capable segment. Acknowledging is correct: the
        // linear aperture (id 1) redirects system-memory MDLs and holds no
        // bits of ours.
        return STATUS_SUCCESS;
    };

    // SAFETY: KeGetCurrentIrql is callable at any IRQL.
    let irql = unsafe { KeGetCurrentIrql() };
    if irql != PASSIVE_LEVEL_IRQL {
        // ⛔ Documented PASSIVE, but display activation drives it at DISPATCH on
        // the scan-out primary. `map_blob_at` needs a host round-trip, so a NEW
        // mapping cannot be established here. Two rules bind the return value:
        //  1. NEVER `STATUS_UNSUCCESSFUL` — out of this DDI's legal set, so
        //     dxgkrnl calls it a driver bug and DISCARDS THE WHOLE VidPn: no
        //     display, no diagnostic.
        //  2. Already mapped at the offset dxgkrnl chose ⇒ the window is live,
        //     acknowledge. Otherwise defer with a legal retryable status so it
        //     re-issues at PASSIVE.
        BAR_AP_ERR_IRQL.fetch_add(1, Ordering::Relaxed);
        let already = (unsafe { paging_alloc_info(args.hAllocation) })
            .filter(|a| a.bar_eligible)
            .and_then(|a| {
                // SAFETY: dxgkrnl owns the page array for the call.
                match unsafe { validate_aperture_request(args, &a, bar.aperture_len) } {
                    Ok(range) => Some((a, range)),
                    Err(refusal) => {
                        refusal.count(a.resource_id);
                        None
                    }
                }
            })
            .is_some_and(|(a, range)| {
                adapter.resource_mapped_at_offset(a.resource_id, range.offset)
            });
        if already {
            BAR_AP_IRQL_ACK.fetch_add(1, Ordering::Relaxed);
            return STATUS_SUCCESS;
        }
        BAR_AP_IRQL_DEFER.fetch_add(1, Ordering::Relaxed);
        return STATUS_NO_MEMORY;
    }

    // SAFETY: downstream of the runtime IRQL check above, not of the doc
    // annotation. The DISPATCH arm holds no token, so it cannot reach
    // `map_blob_at` even by accident.
    let passive = unsafe { crate::irql::PassiveLevel::assume() };
    let Some(alloc) = (unsafe { paging_alloc_info(args.hAllocation) }) else {
        BAR_AP_ERR_ALLOC.fetch_add(1, Ordering::Relaxed);
        diag_dump_cpu_host_atomics();
        return STATUS_NO_MEMORY;
    };
    if !alloc.bar_eligible || alloc.resource_id == 0 {
        // The allocation contract says this object is device-local or has no
        // host blob. Never reinterpret it as CPU-addressable blob bytes.
        BAR_AP_ERR_ALLOC.fetch_add(1, Ordering::Relaxed);
        BAR_AP_LAST_RESID.store(alloc.resource_id, Ordering::Relaxed);
        diag_dump_cpu_host_atomics();
        return STATUS_NO_MEMORY;
    }
    // SAFETY: dxgkrnl owns `pCpuHostAperturePages` for the call.
    let range = match unsafe { validate_aperture_request(args, &alloc, bar.aperture_len) } {
        Ok(range) => range,
        Err(refusal) => {
            refusal.count(alloc.resource_id);
            diag_dump_cpu_host_atomics();
            return STATUS_NO_MEMORY;
        }
    };

    match crate::virtio::ctrl::map_blob_at(passive, adapter, alloc.resource_id, range.offset) {
        Ok(_) => {
            BAR_AP_MAPS.fetch_add(1, Ordering::Relaxed);
            BAR_AP_LAST_RESID.store(alloc.resource_id, Ordering::Relaxed);
            // No flush on success: a registry write per aperture map is the
            // storm R317 removed. The refusal paths and the mode-set hook flush.
            STATUS_SUCCESS
        }
        Err(_) => {
            BAR_AP_ERR_MAP.fetch_add(1, Ordering::Relaxed);
            BAR_AP_LAST_RESID.store(alloc.resource_id, Ordering::Relaxed);
            diag_dump_cpu_host_atomics();
            // Legal and retryable, NOT the out-of-contract STATUS_UNSUCCESSFUL.
            STATUS_NO_MEMORY
        }
    }
}

/// # Safety
/// dxgkrnl passes its own adapter context and a valid argument block.
pub unsafe extern "C" fn dxgkddi_unmap_cpu_host_aperture(
    h_adapter: *mut c_void,
    unmap: IN_CONST_PDXGKARG_UNMAPCPUHOSTAPERTURE,
) -> NTSTATUS {
    if h_adapter.is_null() || unmap.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: valid per the DDI contract.
    let args = unsafe { &*unmap };
    CPU_HOST_UNMAP_COUNT.fetch_add(1, Ordering::Relaxed);
    // SAFETY: our AdapterContext, per the DDI contract.
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    let is_bar = adapter
        .bar_segment()
        .is_some_and(|b| b.seg_id == args.SegmentId as u32);
    if !is_bar || args.NumberOfPages == 0 || args.pCpuHostAperturePages.is_null() {
        return STATUS_SUCCESS;
    }
    // SAFETY: the array holds NumberOfPages entries for the call.
    let page0 = unsafe { core::ptr::read_unaligned(args.pCpuHostAperturePages) } as u64;
    let offset = page0 << 12;
    // SAFETY: callable at any IRQL.
    if unsafe { KeGetCurrentIrql() } != PASSIVE_LEVEL_IRQL {
        // Cannot run the unmap round-trip here. Leave the mapping (counted):
        // the next `map_blob_at` over the range evicts it, so this self-heals.
        BAR_AP_ERR_IRQL.fetch_add(1, Ordering::Relaxed);
        return STATUS_SUCCESS;
    }
    // SAFETY: downstream of the runtime check, as in the map DDI.
    let passive = unsafe { crate::irql::PassiveLevel::assume() };
    match adapter
        .canonical_mapped_resource_at_offset(offset)
        .unwrap_or(None)
    {
        Some(res) => {
            let _ = crate::virtio::ctrl::resource_unmap_blob(passive, adapter, res);
            BAR_AP_UNMAPS.fetch_add(1, Ordering::Relaxed);
        }
        None => {
            // Nothing mapped there: already torn down at DestroyAllocation, or
            // a map this driver refused. Counted for visibility.
            BAR_AP_ERR_UNRESOLVED_UNMAP.fetch_add(1, Ordering::Relaxed);
            diag_dump_cpu_host_atomics();
        }
    }
    STATUS_SUCCESS
}
