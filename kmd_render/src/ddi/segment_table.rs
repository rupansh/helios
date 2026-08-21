//! The immutable segment table reported by the active WDDM surface.
//!
//! K8 owns two facts only: the ordinary linear aperture is always segment 1,
//! and an exact virtio host-visible capacity may be exposed as a non-CPU-
//! visible local-memory segment 2. Neither entry is a CPU-host aperture and no
//! registry value can add, reorder, or change their capabilities.

/// One reported segment, independent of the WDK descriptor generation used to
/// render it.
#[derive(Clone, Copy)]
pub(crate) enum SegmentSpec {
    /// The viogpu3d-style linear aperture. Always segment id 1.
    Aperture,
    /// Exact local VidMm capacity. This is an accounting/residency fact, not a
    /// CPU mapping window; the KMD content engine owns migration to the
    /// aperture when a CPU-visible allocation needs a CPU view.
    Local { gpu_base: u64, size: u64 },
}

/// The reported table, built once during StartDevice and rendered verbatim by
/// QuerySegment4. Segment ids are positional: index 0 is id 1.
#[derive(Clone, Copy)]
pub(crate) struct SegmentTable {
    entries: [Option<SegmentSpec>; Self::MAX],
    len: u32,
}

impl SegmentTable {
    pub(crate) const MAX: usize = 2;

    /// The transport-absent fallback. An empty segment table is not a shape
    /// this driver owns or has admitted.
    pub(crate) const APERTURE_ONLY: Self = Self {
        entries: [Some(SegmentSpec::Aperture), None],
        len: 1,
    };

    /// Add the exact local-memory capacity to the canonical aperture entry.
    pub(crate) const fn with_local(gpu_base: u64, size: u64) -> Self {
        Self {
            entries: [
                Some(SegmentSpec::Aperture),
                Some(SegmentSpec::Local { gpu_base, size }),
            ],
            len: 2,
        }
    }

    /// `NbSegment`. The count and descriptor pass read this same value.
    pub(crate) const fn len(&self) -> u32 {
        self.len
    }

    /// `(segment id, spec)` in report order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, SegmentSpec)> + '_ {
        self.entries
            .iter()
            .flatten()
            .enumerate()
            .map(|(idx, spec)| (idx as u32 + 1, *spec))
    }

    /// The local-memory segment id, if this exact table reports one.
    pub(crate) fn local_seg_id(&self) -> Option<u32> {
        self.iter()
            .find(|(_, spec)| matches!(spec, SegmentSpec::Local { .. }))
            .map(|(id, _)| id)
    }
}
