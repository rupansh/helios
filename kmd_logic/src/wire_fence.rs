//! The one wire-fence id allocator for a transport generation.
//!
//! ⛔ THIS EXISTS BECAUSE THERE WERE TWO OF THEM, AND THAT WAS A BLACK DESKTOP.
//! `VirtioGpu::next_wire_fence` (virtio spinlock) and
//! `InterruptQueueCore::next_direct_fence` (DIRQL access gate) split each
//! generation's range in half so neither domain needed the other's lock. Both
//! fed the same `direct_scanout_lifetime::PlaneState`, which enforces ONE
//! monotonic `fence_id_high_water`, and both emitted ctx-0 `SET_SCANOUT_BLOB`
//! into the same host retirement space, where QEMU retires every id <= the
//! callback id. The D4 half was unconditionally above the control half, so the
//! first control bind after any D4 bind went backwards -- `FenceIdWentBackward`,
//! plane poisoned, `SetVidPnSourceVisibility(TRUE)` refused with
//! DEVICE_NOT_READY, `SetDisplayMode` E_FAIL, DWM never presented.
//!
//! A partitioned id space cannot fix an allocator problem whose cause is "DIRQL
//! cannot take the lock". A lock-free allocator can, and that is all this is.

use core::sync::atomic::{AtomicU64, Ordering};

/// Lock-free monotonic id allocator over `[base, limit)`.
pub struct WireFenceAllocator {
    next: AtomicU64,
    limit: u64,
}

impl WireFenceAllocator {
    /// `base` is the first id this generation may issue; `limit` the first it
    /// may not. A `base >= limit` allocator is legal and immediately exhausted.
    pub const fn new(base: u64, limit: u64) -> Self {
        Self {
            next: AtomicU64::new(base),
            limit,
        }
    }

    /// The next id, or `None` once the range is spent.
    ///
    /// Callable at any IRQL: one relaxed `fetch_add` and a comparison. Relaxed
    /// is sufficient because this orders nothing but itself — every consumer of
    /// an id establishes its own ordering through the queue it is written into.
    ///
    /// ⚠ The id is spent even if the caller then fails to use it. Two
    /// serialization domains cannot share a peek-then-commit, so that property
    /// is traded for monotonicity, which is the one `PlaneState` requires.
    pub fn reserve(&self) -> Option<u64> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if id >= self.limit {
            // The counter may run past `limit`; every id at or above it is
            // refused, so none can be issued twice. Clamping would race another
            // reserver and could hand the same id out again.
            return None;
        }
        Some(id)
    }

    /// One past the highest id that may have been issued, for in-flight
    /// retirement predicates. Never reports beyond the range.
    pub fn watermark(&self) -> u64 {
        let next = self.next.load(Ordering::Relaxed);
        if next > self.limit {
            self.limit
        } else {
            next
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 1_000;
    const LIMIT: u64 = 1_008;

    #[test]
    fn ids_are_strictly_increasing_and_inside_the_range() {
        let alloc = WireFenceAllocator::new(BASE, LIMIT);
        let mut previous = None;
        while let Some(id) = alloc.reserve() {
            assert!((BASE..LIMIT).contains(&id), "{id} outside the range");
            if let Some(previous) = previous {
                assert!(id > previous, "{id} did not advance past {previous}");
            }
            previous = Some(id);
        }
        assert_eq!(previous, Some(LIMIT - 1), "the range was not fully issued");
    }

    /// The exact regression this type exists to prevent, stated as the
    /// difference between the two designs.
    ///
    /// `PlaneState::validate_new_fence_id` refuses any id at or below its
    /// `fence_id_high_water`. Both the control path and the D4 path publish
    /// into ONE plane, so the ids they issue share one high water.
    #[test]
    fn one_allocator_fixes_the_split_that_poisoned_the_plane() {
        const SPLIT: u64 = BASE + (LIMIT - BASE) / 2;

        // OLD: disjoint halves, D4 unconditionally above control. A D4 bind
        // followed by a control bind hands the plane a LOWER id -- which is
        // `FenceIdWentBackward`, measured as D2PsnRfs=10 on KMD 22.22.346.0.
        let control = WireFenceAllocator::new(BASE, SPLIT);
        let d4 = WireFenceAllocator::new(SPLIT, LIMIT);
        let from_d4 = d4.reserve().expect("d4 half not exhausted");
        let from_control = control.reserve().expect("control half not exhausted");
        assert!(
            from_control < from_d4,
            "the split is precisely what made the second id go backwards"
        );

        // NEW: one allocator, the same interleaving, monotonic by construction.
        let shared = WireFenceAllocator::new(BASE, LIMIT);
        let mut high_water = 0;
        for _ in 0..(LIMIT - BASE) {
            let id = shared.reserve().expect("range not exhausted");
            assert!(
                id > high_water,
                "id {id} did not advance past high water {high_water}"
            );
            high_water = id;
        }
    }

    #[test]
    fn exhaustion_refuses_forever_and_never_wraps_or_repeats() {
        let alloc = WireFenceAllocator::new(LIMIT - 1, LIMIT);
        assert_eq!(alloc.reserve(), Some(LIMIT - 1));
        for _ in 0..64 {
            assert_eq!(alloc.reserve(), None);
        }
        assert_eq!(alloc.watermark(), LIMIT, "watermark ran past the range");
    }

    #[test]
    fn an_empty_range_issues_nothing() {
        let alloc = WireFenceAllocator::new(BASE, BASE);
        assert_eq!(alloc.reserve(), None);
        assert_eq!(alloc.watermark(), BASE);
    }

    #[test]
    fn watermark_is_one_past_the_last_issued_id() {
        let alloc = WireFenceAllocator::new(BASE, LIMIT);
        assert_eq!(alloc.watermark(), BASE);
        let first = alloc.reserve().unwrap();
        assert_eq!(alloc.watermark(), first + 1);
    }
}
