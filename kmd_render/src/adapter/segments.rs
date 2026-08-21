//! Transport-generation local-memory facts.

/// Exact non-CPU-visible local VidMm capacity derived from the virtio
/// host-visible capability. This object carries no mapping address: ordinary
/// blob mappings remain KMD-owned and CPU access migrates through the linear
/// aperture/content engine.
pub struct LocalSegment {
    /// Capacity reported to VidMm.
    pub size: u64,
    /// The WDDM segment id tied to that capacity.
    pub seg_id: u32,
}
