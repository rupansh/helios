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

/// Receipt for one actually enqueued tagged Venus submission. A feedback
/// notification must match this receipt, never just a watermark or cookie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Submission {
    pub stream: u32,
    pub value: u32,
    pub wire_fence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackResult {
    Accepted,
    WireRetired,
    Rejected,
}

/// GPU execution and wire retirement protect different lifetimes. This state
/// is embedded in the live KMD stream slot; only the wire edge can release
/// Present readers, transport storage, or the closing registration.
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    completed: u32,
    retired: u32,
    last_wire: Option<Submission>,
}

impl Progress {
    pub const EMPTY: Self = Self {
        completed: 0,
        retired: 0,
        last_wire: None,
    };
    pub fn completed(self) -> u32 {
        self.completed
    }
    pub fn retired(self) -> u32 {
        self.retired
    }

    /// The caller holds notify -> virtio locks, authenticates the live owner /
    /// context / cookie, and supplies the original in-flight receipt if present.
    /// A removed receipt is NOT evidence. Only its exact successful wire
    /// retirement may acknowledge a notification racing the real response.
    pub fn feedback(
        &mut self,
        handle: u32,
        tag: Submission,
        admitted: Option<Submission>,
    ) -> FeedbackResult {
        if tag.stream != handle || handle == 0 || tag.value == 0 || tag.wire_fence == 0 {
            return FeedbackResult::Rejected;
        }
        if admitted == Some(tag) {
            self.completed = self.completed.max(tag.value);
            FeedbackResult::Accepted
        } else if self.last_wire == Some(tag) {
            FeedbackResult::WireRetired
        } else {
            FeedbackResult::Rejected
        }
    }

    /// Called only on a successful used-ring response for the original tag.
    pub fn wire(&mut self, handle: u32, tag: Submission) -> bool {
        if tag.stream != handle || handle == 0 || tag.value == 0 || tag.wire_fence == 0 {
            return false;
        }
        self.completed = self.completed.max(tag.value);
        if tag.value > self.retired {
            self.retired = tag.value;
            self.last_wire = Some(tag);
        }
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
    fn feedback_completes_execution_without_retiring_wire_or_consumer() {
        let mut progress = Progress::EMPTY;
        let tag = tag(3, 9, 500);
        let mut wait = Wait::new(boundary(3, 9)).unwrap();
        assert_eq!(
            progress.feedback(3, tag, Some(tag)),
            FeedbackResult::Accepted
        );
        wait.observe(3, progress.completed());
        assert!(wait.completed());
        assert_eq!(progress.retired(), 0);
        assert!(!crate::present_stream::slot_ready(
            true,
            1,
            0,
            64,
            9,
            progress.retired()
        ));
        assert!(progress.wire(3, tag));
        assert_eq!(progress.retired(), 9);
        assert!(crate::present_stream::slot_ready(
            true,
            1,
            0,
            64,
            9,
            progress.retired()
        ));
    }

    #[test]
    fn feedback_and_wire_response_can_arrive_in_either_order() {
        for wire_first in [false, true] {
            let mut progress = Progress::EMPTY;
            let tag = tag(3, 9, 500);
            if wire_first {
                assert!(progress.wire(3, tag));
                assert_eq!(progress.feedback(3, tag, None), FeedbackResult::WireRetired);
            } else {
                assert_eq!(
                    progress.feedback(3, tag, Some(tag)),
                    FeedbackResult::Accepted
                );
                assert!(progress.wire(3, tag));
            }
            assert_eq!(progress.completed(), 9);
            assert_eq!(progress.retired(), 9);
        }
    }

    #[test]
    fn missing_mismatched_and_cancelled_receipts_cannot_complete() {
        let mut progress = Progress::EMPTY;
        let tag = tag(3, 9, 500);
        for admitted in [
            None,
            Some(Submission { stream: 4, ..tag }),
            Some(Submission { value: 10, ..tag }),
            Some(Submission {
                wire_fence: 501,
                ..tag
            }),
        ] {
            assert_eq!(
                progress.feedback(3, tag, admitted),
                FeedbackResult::Rejected
            );
        }
        assert_eq!(progress.completed(), 0);
        assert_eq!(progress.retired(), 0);
        // Cancellation removes the real in-flight receipt. It creates no
        // successful wire receipt, even when the same numeric value is reused.
        let next = Submission {
            stream: 4,
            value: 9,
            wire_fence: 501,
        };
        assert_eq!(
            progress.feedback(4, tag, Some(next)),
            FeedbackResult::Rejected
        );
        assert!(!progress.wire(4, tag));
        assert_eq!(progress.completed(), 0);
    }

    #[test]
    fn out_of_order_feedback_never_moves_wire_or_gpu_progress_backwards() {
        let mut progress = Progress::EMPTY;
        let older = tag(3, 9, 500);
        let newer = tag(3, 12, 510);
        assert_eq!(
            progress.feedback(3, newer, Some(newer)),
            FeedbackResult::Accepted
        );
        assert!(progress.wire(3, older));
        assert_eq!(progress.completed(), 12);
        assert_eq!(progress.retired(), 9);
        assert_eq!(
            progress.feedback(3, older, None),
            FeedbackResult::WireRetired
        );
        assert!(progress.wire(3, newer));
        assert!(progress.wire(3, older));
        assert_eq!(progress.completed(), 12);
        assert_eq!(progress.retired(), 12);
        // No historical receipt guessing from the higher watermark. An old
        // notification may be refused; its real response has already completed.
        assert_eq!(progress.feedback(3, older, None), FeedbackResult::Rejected);
    }

    #[test]
    fn delayed_notification_cannot_reuse_a_recreated_stream() {
        let old = tag(3, 9, 500);
        let new = tag(4, 1, 600);
        let mut progress = Progress::EMPTY;
        assert_eq!(
            progress.feedback(4, old, Some(new)),
            FeedbackResult::Rejected
        );
        let mut wait = Wait::new(boundary(4, 1)).unwrap();
        wait.observe(4, progress.completed());
        assert!(!wait.completed());
        assert_eq!(
            progress.feedback(4, new, Some(new)),
            FeedbackResult::Accepted
        );
        wait.observe(4, progress.completed());
        assert!(wait.completed());
    }

    #[test]
    fn publication_and_submission_observe_already_completed_feedback() {
        use crate::producer_completion::{Predicate, Table};
        for publish_first in [false, true] {
            let mut progress = Progress::EMPTY;
            let mut table = Table::new(1, 2, 2).unwrap();
            let key = table.register(100).unwrap();
            let tag = tag(3, 9, 500);
            let mut epoch = 0;
            if publish_first {
                epoch = table.publish(key, 3, 9, progress.completed() >= 9).unwrap();
                assert_eq!(table.predicate(key, epoch), Ok(Predicate::Pending));
            }
            assert_eq!(
                progress.feedback(3, tag, Some(tag)),
                FeedbackResult::Accepted
            );
            table.complete(3, progress.completed());
            if !publish_first {
                epoch = table.publish(key, 3, 9, progress.completed() >= 9).unwrap();
            }
            assert_eq!(table.predicate(key, epoch), Ok(Predicate::Ready));
            let mut wait = Wait::new(boundary(3, 9)).unwrap();
            wait.observe(3, progress.completed());
            assert!(wait.completed());
            assert_eq!(progress.retired(), 0);
        }
    }

    #[test]
    fn producer_cancellation_is_terminal_even_after_a_late_gpu_observation() {
        use crate::producer_completion::{Predicate, Table, CANCELLED};
        let mut table = Table::new(1, 2, 2).unwrap();
        let key = table.register(100).unwrap();
        let epoch = table.publish(key, 3, 9, false).unwrap();
        table.fail_stream(3, CANCELLED);
        let mut progress = Progress::EMPTY;
        let tag = tag(3, 9, 500);
        assert_eq!(
            progress.feedback(3, tag, Some(tag)),
            FeedbackResult::Accepted
        );
        table.complete(3, progress.completed());
        assert_eq!(
            table.predicate(key, epoch),
            Ok(Predicate::Terminal(CANCELLED))
        );
        assert_eq!(progress.retired(), 0);
    }
}
