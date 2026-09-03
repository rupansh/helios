//! Ticket bookkeeping when dxgkrnl re-issues a batch (`Flags.Resubmission`)
//! after a preempt or cancel. Measured 2026-09-03 (Fire Strike GT1 stuck at
//! loading): a same-epoch replay of a batch whose only ticket had already died
//! was refused, the replay was then completed as if it had run, and the batch
//! never left its slot — seven such packets, `Nr2OuterRej` code 7.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayAction {
    /// A newer epoch but an old ticket is still live: the engine cannot hold
    /// two live tickets for one batch.
    Refuse,
    /// Every earlier ticket is dead (new epoch, or retired in this one): the
    /// replay starts the batch's bookkeeping over.
    Reset,
    /// Same epoch with a live ticket: append to the existing bookkeeping.
    Continue,
}

pub fn replay_action(new_epoch: bool, any_live: bool) -> ReplayAction {
    match (new_epoch, any_live) {
        (true, true) => ReplayAction::Refuse,
        (_, false) => ReplayAction::Reset,
        (false, true) => ReplayAction::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replay_after_the_old_ticket_died_starts_over_in_any_epoch() {
        assert_eq!(replay_action(true, false), ReplayAction::Reset);
        // The GT1 case: dxgkrnl re-issued the packet in the same epoch after
        // its first ticket retired; the old rule kept `commit_seen` and refused.
        assert_eq!(replay_action(false, false), ReplayAction::Reset);
    }

    #[test]
    fn a_live_ticket_keeps_its_epoch_or_refuses_a_newer_one() {
        assert_eq!(replay_action(false, true), ReplayAction::Continue);
        assert_eq!(replay_action(true, true), ReplayAction::Refuse);
    }
}
