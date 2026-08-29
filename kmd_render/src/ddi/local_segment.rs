//! Deterministic local VidMm capacity and the immutable reported segment table.
//!
//! The capacity comes only from the exact virtio host-visible capability
//! length. It is not a CPU mapping aperture, does not reserve a window prefix,
//! and has no registry-selected topology or flag word.

/// Keep the local-memory address range disjoint from the linear aperture.
pub(super) const VIDMM_MEMORY_BASE: u64 = 1 << 32;
const VIDMM_LOCAL_MIN_BYTES: u64 = 256 << 20;
const VIDMM_LOCAL_MAX_BYTES: u64 = 64 << 30;

/// Derive the one optional local-memory fact owned by the active transport.
pub(super) fn setup_local_segment(
    gpu: &crate::virtio::VirtioGpu,
) -> Option<crate::adapter::LocalSegment> {
    let window = gpu.host_visible()?;
    let size = window.len;
    if !(VIDMM_LOCAL_MIN_BYTES..=VIDMM_LOCAL_MAX_BYTES).contains(&size) || size & 4095 != 0 {
        crate::diag::fault(
            crate::diag::FaultCounter::StBar,
            (size >> 20).min(u32::MAX as u64) as u32,
        );
        return None;
    }

    crate::diag::record(0x0B00_0008);
    crate::diag::record((size >> 20).min(u32::MAX as u64) as u32);
    Some(crate::adapter::LocalSegment {
        size,
        // Positional: the aperture is index 0/id 1, so local memory is id 2.
        // It is also the LAST reported segment, which a SupportsCpuHostAperture
        // segment must be (AddAdapter Code 43, ETW-proven 2026-07-05).
        seg_id: crate::ddi::gpummu::MEMORY_SEGMENT_ID,
        aperture_gpa: window.base,
        aperture_len: window.len,
    })
}

/// Build the reported table from the exact object every placement consumer
/// reads. QuerySegment4 renders this value; it does not re-derive capacity.
pub(super) fn build_segment_table(
    local_segment: Option<&crate::adapter::LocalSegment>,
) -> crate::ddi::segment_table::SegmentTable {
    match local_segment {
        Some(local) => crate::ddi::segment_table::SegmentTable::with_local(
            VIDMM_MEMORY_BASE,
            local.size,
            local.aperture_gpa,
            local.aperture_len,
        ),
        None => crate::ddi::segment_table::SegmentTable::APERTURE_ONLY,
    }
}
