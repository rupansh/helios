//! Exact queue execution boundaries, independent of Present/consumer release.
//! Used by the production DMA private-data and retirement paths.

const MAGIC: u32 = 0x5845_5048; // HPEX, kernel-private (never UMD authority).
const VERSION: u32 = 1;

pub const fn stream(boundary: u64) -> Option<u32> {
    let handle = ((boundary >> 32) & 0x7fff_ffff) as u32;
    if boundary >> 63 == 1 && handle != 0 && boundary as u32 != 0 {
        Some(handle)
    } else {
        None
    }
}

/// A Render publishes once. Preemption replays the kernel-private record via
/// SubmitCommand; it must not re-publish an old user command as new work.
pub fn advances_context(previous: u64, next: u64) -> bool {
    stream(next).is_some()
        && (previous == 0 || (stream(previous) == stream(next) && next as u32 > previous as u32))
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Record {
    magic: u32,
    version: u32,
    boundary: u64,
}

impl Record {
    /// The caller authenticated the stream against this exact dxgkrnl context.
    /// A same-context recycled buffer is harmless: every new ECL advances its
    /// stream. Keeping the largest value also preserves batched predecessors.
    pub fn merge(self, boundary: u64) -> Option<Self> {
        let handle = stream(boundary)?;
        let previous = self.boundary_for(handle).unwrap_or(0);
        Some(Self {
            magic: MAGIC,
            version: VERSION,
            boundary: previous.max(boundary),
        })
    }

    pub fn boundary_for(self, context_stream: u32) -> Option<u64> {
        (self.magic == MAGIC
            && self.version == VERSION
            && stream(self.boundary) == Some(context_stream))
        .then_some(self.boundary)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    boundary: u64,
    completed: bool,
}

impl Wait {
    pub fn new(boundary: u64) -> Option<Self> {
        stream(boundary)?;
        Some(Self {
            boundary,
            completed: false,
        })
    }

    pub fn boundary(self) -> u64 {
        self.boundary
    }
    pub fn completed(self) -> bool {
        self.completed
    }

    /// Only an actual registered stream retirement is evidence. Latch it before
    /// registration teardown, so losing a live slot cannot erase completed work.
    pub fn observe(&mut self, handle: u32, retired_value: u32) {
        if stream(self.boundary) == Some(handle) && retired_value >= self.boundary as u32 {
            self.completed = true;
        }
    }
}

/// Receipt for an enqueued tagged Venus submission. Only its authenticated,
/// successful used-ring response may advance stream completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Submission {
    pub stream: u32,
    pub value: u32,
    pub wire_fence: u64,
}

/// Queue-marker retirement is the sole GPU completion source. Consumer release
/// still requires the separate Present reader/ownership contract.
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    retired: u32,
}

impl Progress {
    pub const EMPTY: Self = Self { retired: 0 };
    pub fn completed(self) -> u32 {
        self.retired
    }
    pub fn retired(self) -> u32 {
        self.retired
    }

    /// Called only on a successful used-ring response for the original tag.
    pub fn wire(&mut self, handle: u32, tag: Submission) -> bool {
        if tag.stream != handle || handle == 0 || tag.value == 0 || tag.wire_fence == 0 {
            return false;
        }
        self.retired = self.retired.max(tag.value);
        true
    }
}

const _: () = assert!(core::mem::size_of::<Record>() == 16);

#[cfg(test)]
mod tests {
    use super::*;
    fn boundary(handle: u32, value: u32) -> u64 {
        (1u64 << 63) | ((handle as u64) << 32) | value as u64
    }

    #[test]
    fn validates_namespace_and_nonzero_parts() {
        for invalid in [0, 1, 1 << 63, boundary(0, 1), boundary(1, 0)] {
            assert!(Record::default().merge(invalid).is_none());
            assert!(Wait::new(invalid).is_none());
        }
    }

    #[test]
    fn batch_merge_preserves_latest_exact_epoch_and_replay() {
        let record = Record::default()
            .merge(boundary(7, 5))
            .unwrap()
            .merge(boundary(7, 8))
            .unwrap()
            .merge(boundary(7, 6))
            .unwrap();
        assert_eq!(record.boundary_for(7), Some(boundary(7, 8)));
        assert_eq!(record.boundary_for(7), Some(boundary(7, 8)));
    }

    #[test]
    fn recycled_context_cannot_inherit_another_generation() {
        let old = Record::default().merge(boundary(7, 100)).unwrap();
        assert_eq!(old.boundary_for(8), None);
        assert_eq!(old.boundary_for(0), None);
        let new = old.merge(boundary(8, 1)).unwrap();
        assert_eq!(new.boundary_for(8), Some(boundary(8, 1)));
        assert_eq!(new.boundary_for(7), None);
    }

    #[test]
    fn context_publication_rejects_replay_regression_and_stream_switch() {
        assert!(advances_context(0, boundary(7, 1)));
        assert!(advances_context(boundary(7, 1), boundary(7, 3)));
        assert!(!advances_context(boundary(7, 3), boundary(7, 3)));
        assert!(!advances_context(boundary(7, 3), boundary(7, 1)));
        assert!(!advances_context(boundary(7, 3), boundary(8, 4)));
        assert!(!advances_context(boundary(7, u32::MAX), boundary(7, 1)));
    }

    #[test]
    fn publication_before_retirement_waits_for_exact_value() {
        let mut wait = Wait::new(boundary(3, 9)).unwrap();
        for value in [0, 1, 8] {
            wait.observe(3, value);
            assert!(!wait.completed());
        }
        wait.observe(3, 9);
        assert!(wait.completed());
    }

    #[test]
    fn already_retired_at_submission_and_out_of_order_observations() {
        let mut wait = Wait::new(boundary(3, 9)).unwrap();
        wait.observe(3, 12);
        wait.observe(3, 8);
        assert!(wait.completed());
    }

    #[test]
    fn stale_generation_never_satisfies_or_erases_completion() {
        let mut wait = Wait::new(boundary(3, 9)).unwrap();
        wait.observe(4, u32::MAX);
        assert!(!wait.completed());
        wait.observe(3, 9);
        wait.observe(4, 0);
        assert!(wait.completed());
    }

    #[test]
    fn teardown_cancellation_and_elapsed_time_supply_no_retirement() {
        let wait = Wait::new(boundary(3, 9)).unwrap();
        // There is deliberately no timeout/cancel-to-success transition. A
        // scheduler reset discards this waiter with its failed epoch.
        assert!(!wait.completed());
        let next_generation = Wait::new(boundary(4, 9)).unwrap();
        assert!(!next_generation.completed());
    }

    fn tag(stream: u32, value: u32, wire_fence: u64) -> Submission {
        Submission {
            stream,
            value,
            wire_fence,
        }
    }

    #[test]
    fn execution_waits_for_exact_successful_wire_response() {
        let mut progress = Progress::EMPTY;
        let mut wait = Wait::new(boundary(3, 9)).unwrap();
        wait.observe(3, progress.completed());
        assert!(!wait.completed());
        for invalid in [tag(4, 9, 500), tag(0, 9, 500), tag(3, 0, 500), tag(3, 9, 0)] {
            assert!(!progress.wire(3, invalid));
            assert_eq!(progress.completed(), 0);
        }
        assert!(progress.wire(3, tag(3, 9, 500)));
        wait.observe(3, progress.completed());
        assert!(wait.completed());
        assert_eq!(progress.retired(), 9);
    }

    #[test]
    fn out_of_order_wire_responses_never_regress_progress() {
        let mut progress = Progress::EMPTY;
        assert!(progress.wire(3, tag(3, 12, 510)));
        assert!(progress.wire(3, tag(3, 9, 500)));
        assert_eq!(progress.completed(), 12);
        assert_eq!(progress.retired(), 12);
    }

    #[test]
    fn late_response_cannot_complete_a_recreated_stream() {
        let mut progress = Progress::EMPTY;
        assert!(!progress.wire(4, tag(3, 9, 500)));
        assert_eq!(progress.completed(), 0);
        assert!(progress.wire(4, tag(4, 1, 600)));
        assert_eq!(progress.completed(), 1);
    }

    #[test]
    fn publication_before_or_after_wire_observes_same_producer_epoch() {
        use crate::producer_completion::{Predicate, Table};
        for publish_first in [false, true] {
            let mut progress = Progress::EMPTY;
            let mut table = Table::new(1, 2, 2).unwrap();
            let key = table.register(100).unwrap();
            let mut epoch = 0;
            if publish_first {
                epoch = table.publish(key, 3, 9, false).unwrap();
                assert_eq!(table.predicate(key, epoch), Ok(Predicate::Pending));
            }
            assert!(progress.wire(3, tag(3, 9, 500)));
            table.complete(3, progress.completed());
            if !publish_first {
                epoch = table.publish(key, 3, 9, progress.completed() >= 9).unwrap();
            }
            assert_eq!(table.predicate(key, epoch), Ok(Predicate::Ready));
        }
    }

    #[test]
    fn producer_cancellation_is_terminal_after_a_late_wire_response() {
        use crate::producer_completion::{Predicate, Table, CANCELLED};
        let mut table = Table::new(1, 2, 2).unwrap();
        let key = table.register(100).unwrap();
        let epoch = table.publish(key, 3, 9, false).unwrap();
        table.fail_stream(3, CANCELLED);
        let mut progress = Progress::EMPTY;
        assert!(progress.wire(3, tag(3, 9, 500)));
        table.complete(3, progress.completed());
        assert_eq!(table.predicate(key, epoch), Ok(Predicate::Terminal(CANCELLED)));
    }
}
