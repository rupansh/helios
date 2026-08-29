//! Transport-generation local-memory facts.

/// Exact local VidMm capacity derived from the virtio host-visible capability,
/// together with the window that makes it a CPU host aperture.
#[derive(Clone, Copy)]
pub struct LocalSegment {
    /// Capacity reported to VidMm.
    pub size: u64,
    /// The WDDM segment id tied to that capacity.
    pub seg_id: u32,
    /// Guest-physical base of the virtio host-visible window, and its length.
    /// This is what makes the segment a CPU host aperture: dxgkrnl builds CPU
    /// views over `aperture_gpa + page*4K`, and the KMD maps the allocation's
    /// venus blob at exactly that window offset, so those views read the blob
    /// bytes. Without it the segment is accounting only and a CPU-visible
    /// allocation has no view of its own content.
    pub aperture_gpa: u64,
    pub aperture_len: u64,
}
