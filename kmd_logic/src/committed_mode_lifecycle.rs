//! Pure control-word transitions for atomic committed-mode publication.
//! Payload ownership remains with the platform publisher.

const WRITER: u64 = 1 << 0;
const PRESENT: u64 = 1 << 1;
const RESET_CLOSED: u64 = 1 << 2;
const REMOVED: u64 = 1 << 3;
const POISONED: u64 = 1 << 4;
const REVISION_SHIFT: u32 = 5;
const FLAGS: u64 = (1 << REVISION_SHIFT) - 1;
const REVISION_MAX: u64 = u64::MAX >> REVISION_SHIFT;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleWord(u64);

impl LifecycleWord {
    pub const INITIAL: Self = Self(0);

    pub const fn from_atomic(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn into_atomic(self) -> u64 {
        self.0
    }

    pub const fn revision(self) -> u64 {
        self.0 >> REVISION_SHIFT
    }

    pub const fn has_writer(self) -> bool {
        self.0 & WRITER != 0
    }

    pub const fn is_present(self) -> bool {
        self.0 & PRESENT != 0
    }

    pub const fn is_reset_closed(self) -> bool {
        self.0 & RESET_CLOSED != 0
    }

    pub const fn is_removed(self) -> bool {
        self.0 & REMOVED != 0
    }

    pub const fn is_poisoned(self) -> bool {
        self.0 & POISONED != 0
    }

    const fn with_revision(self, revision: u64) -> Self {
        Self((self.0 & FLAGS) | (revision << REVISION_SHIFT))
    }

    const fn with_flags(self, set: u64, clear: u64) -> Self {
        Self((self.0 | set) & !clear)
    }
}

impl Default for LifecycleWord {
    fn default() -> Self {
        Self::INITIAL
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionClass {
    Normal,
    ResetBarrier,
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadState {
    Readable { present: bool },
    WriterInFlight,
    ResetClosed,
    Removed,
    Poisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimRefusal {
    WriterCollision,
    ResetClosed,
    ResetNotClosed,
    Removed,
    Poisoned,
    RevisionExhausted,
    LifecycleChanged,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ClaimToken {
    claimed: LifecycleWord,
    class: TransactionClass,
}

impl ClaimToken {
    pub const fn claimed_word(&self) -> LifecycleWord {
        self.claimed
    }

    pub const fn finish(self, present: bool) -> FinishPlan {
        let mut flags = (self.claimed.0 & FLAGS) & !WRITER;
        if present {
            flags |= PRESENT;
        } else {
            flags &= !PRESENT;
        }
        match self.class {
            TransactionClass::Normal => {}
            TransactionClass::ResetBarrier => flags &= !RESET_CLOSED,
            TransactionClass::Remove => {
                flags &= !RESET_CLOSED;
                flags |= REMOVED;
            }
        }
        FinishPlan {
            expected: self.claimed,
            desired: LifecycleWord(flags).with_revision(self.claimed.revision() + 1),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefusedClaim {
    reason: ClaimRefusal,
    lifecycle: LifecycleWord,
}

impl RefusedClaim {
    pub const fn reason(self) -> ClaimRefusal {
        self.reason
    }

    pub const fn lifecycle(self) -> LifecycleWord {
        self.lifecycle
    }
}

pub fn claim(current: LifecycleWord, class: TransactionClass) -> Result<ClaimToken, RefusedClaim> {
    let refusal = if current.is_poisoned() {
        Some(ClaimRefusal::Poisoned)
    } else if current.is_removed() {
        Some(ClaimRefusal::Removed)
    } else if class == TransactionClass::Normal && current.is_reset_closed() {
        Some(ClaimRefusal::ResetClosed)
    } else if class == TransactionClass::ResetBarrier && !current.is_reset_closed() {
        Some(ClaimRefusal::ResetNotClosed)
    } else if current.has_writer() {
        Some(ClaimRefusal::WriterCollision)
    } else {
        None
    };
    if let Some(reason) = refusal {
        return Err(RefusedClaim {
            reason,
            lifecycle: current,
        });
    }
    if current.revision() > REVISION_MAX - 2 {
        return Err(RefusedClaim {
            reason: ClaimRefusal::RevisionExhausted,
            lifecycle: poison(current),
        });
    }

    Ok(ClaimToken {
        claimed: current
            .with_flags(WRITER, 0)
            .with_revision(current.revision() + 1),
        class,
    })
}

pub const fn classify_claim_failure(class: TransactionClass, found: LifecycleWord) -> ClaimRefusal {
    if found.is_poisoned() {
        ClaimRefusal::Poisoned
    } else if found.is_removed() {
        ClaimRefusal::Removed
    } else if matches!(class, TransactionClass::Normal) && found.is_reset_closed() {
        ClaimRefusal::ResetClosed
    } else if found.has_writer() {
        ClaimRefusal::WriterCollision
    } else {
        ClaimRefusal::LifecycleChanged
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrashCloseRefusal {
    Removed,
    Poisoned,
    RevisionExhausted,
    ContentionExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefusedCrashClose {
    reason: CrashCloseRefusal,
    lifecycle: LifecycleWord,
}

impl RefusedCrashClose {
    pub const fn reason(self) -> CrashCloseRefusal {
        self.reason
    }

    pub const fn lifecycle(self) -> LifecycleWord {
        self.lifecycle
    }
}

pub const fn crash_close(current: LifecycleWord) -> Result<LifecycleWord, RefusedCrashClose> {
    if current.is_poisoned() {
        return Err(RefusedCrashClose {
            reason: CrashCloseRefusal::Poisoned,
            lifecycle: current,
        });
    }
    if current.is_removed() {
        return Err(RefusedCrashClose {
            reason: CrashCloseRefusal::Removed,
            lifecycle: current,
        });
    }

    let required_revisions = if current.has_writer() { 4 } else { 3 };
    if current.revision() > REVISION_MAX - required_revisions {
        return Err(RefusedCrashClose {
            reason: CrashCloseRefusal::RevisionExhausted,
            lifecycle: poison(current.with_flags(RESET_CLOSED, 0)),
        });
    }

    Ok(current
        .with_flags(RESET_CLOSED, 0)
        .with_revision(current.revision() + 1))
}

pub const fn force_crash_close_after_contention(current: LifecycleWord) -> RefusedCrashClose {
    RefusedCrashClose {
        reason: CrashCloseRefusal::ContentionExhausted,
        lifecycle: crash_close_atomic_fallback().apply(current),
    }
}

pub const fn poison(current: LifecycleWord) -> LifecycleWord {
    poison_atomic_fallback().apply(current)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomicFallbackPlan {
    set_operand: u64,
    clear_operand: Option<u64>,
}

impl AtomicFallbackPlan {
    pub const fn set_operand(self) -> u64 {
        self.set_operand
    }

    pub const fn clear_operand(self) -> Option<u64> {
        self.clear_operand
    }

    pub const fn apply(self, current: LifecycleWord) -> LifecycleWord {
        let set = current.0 | self.set_operand;
        LifecycleWord(match self.clear_operand {
            Some(clear) => set & clear,
            None => set,
        })
    }
}

pub const fn poison_atomic_fallback() -> AtomicFallbackPlan {
    AtomicFallbackPlan {
        set_operand: POISONED,
        clear_operand: None,
    }
}

pub const fn crash_close_atomic_fallback() -> AtomicFallbackPlan {
    AtomicFallbackPlan {
        set_operand: RESET_CLOSED | POISONED,
        clear_operand: None,
    }
}

pub const fn clear_writer_atomic_fallback() -> AtomicFallbackPlan {
    AtomicFallbackPlan {
        set_operand: POISONED,
        clear_operand: Some(!WRITER),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedBy {
    LifecycleChange,
    Reset,
    Remove,
    Poison,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinishDecision {
    Commit(LifecycleWord),
    RestorePreviousThenClear(InterruptedBy),
}

#[derive(Debug, Eq, PartialEq)]
pub struct FinishPlan {
    expected: LifecycleWord,
    desired: LifecycleWord,
}

impl FinishPlan {
    pub const fn expected_word(&self) -> LifecycleWord {
        self.expected
    }

    pub const fn desired_word(&self) -> LifecycleWord {
        self.desired
    }

    pub const fn decide(self, observed: LifecycleWord) -> FinishDecision {
        if observed.0 == self.expected.0 {
            FinishDecision::Commit(self.desired)
        } else {
            FinishDecision::RestorePreviousThenClear(classify_interruption(observed))
        }
    }
}

pub const fn classify_interruption(word: LifecycleWord) -> InterruptedBy {
    if word.is_poisoned() {
        InterruptedBy::Poison
    } else if word.is_removed() {
        InterruptedBy::Remove
    } else if word.is_reset_closed() {
        InterruptedBy::Reset
    } else {
        InterruptedBy::LifecycleChange
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearIssue {
    RevisionExhausted,
    ContentionExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClearResult {
    lifecycle: LifecycleWord,
    issue: Option<ClearIssue>,
}

impl ClearResult {
    pub const fn lifecycle(self) -> LifecycleWord {
        self.lifecycle
    }

    pub const fn issue(self) -> Option<ClearIssue> {
        self.issue
    }
}

pub const fn clear_interrupted(current: LifecycleWord, poison_on_clear: bool) -> ClearResult {
    if !current.has_writer() {
        return ClearResult {
            lifecycle: poison(current),
            issue: Some(ClearIssue::ContentionExhausted),
        };
    }

    let poison_on_clear = poison_on_clear || current.revision() == REVISION_MAX;
    let cleared = current.with_flags(0, WRITER);
    let cleared = if poison_on_clear {
        poison(cleared)
    } else {
        cleared
    };
    if current.revision() == REVISION_MAX {
        ClearResult {
            lifecycle: cleared,
            issue: Some(ClearIssue::RevisionExhausted),
        }
    } else {
        ClearResult {
            lifecycle: cleared.with_revision(current.revision() + 1),
            issue: None,
        }
    }
}

pub const fn force_clear_after_contention(current: LifecycleWord) -> ClearResult {
    ClearResult {
        lifecycle: clear_writer_atomic_fallback().apply(current),
        issue: Some(ClearIssue::ContentionExhausted),
    }
}

pub const fn read_state(word: LifecycleWord) -> ReadState {
    if word.is_poisoned() {
        ReadState::Poisoned
    } else if word.is_removed() {
        ReadState::Removed
    } else if word.is_reset_closed() {
        ReadState::ResetClosed
    } else if word.has_writer() {
        ReadState::WriterInFlight
    } else {
        ReadState::Readable {
            present: word.is_present(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemovedTombstoneRefusal {
    NotRemoved,
    Poisoned,
    WriterHeld,
    Present,
    ResetClosed,
    RevisionZero,
}

pub const fn validate_removed_tombstone(
    word: LifecycleWord,
) -> Result<(), RemovedTombstoneRefusal> {
    if !word.is_removed() {
        Err(RemovedTombstoneRefusal::NotRemoved)
    } else if word.is_poisoned() {
        Err(RemovedTombstoneRefusal::Poisoned)
    } else if word.has_writer() {
        Err(RemovedTombstoneRefusal::WriterHeld)
    } else if word.is_present() {
        Err(RemovedTombstoneRefusal::Present)
    } else if word.is_reset_closed() {
        Err(RemovedTombstoneRefusal::ResetClosed)
    } else if word.revision() == 0 {
        Err(RemovedTombstoneRefusal::RevisionZero)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn word(revision: u64, flags: u64) -> LifecycleWord {
        LifecycleWord(flags).with_revision(revision)
    }

    fn finish_success(token: ClaimToken, present: bool) -> LifecycleWord {
        let plan = token.finish(present);
        let expected = plan.expected_word();
        match plan.decide(expected) {
            FinishDecision::Commit(next) => next,
            FinishDecision::RestorePreviousThenClear(_) => unreachable!(),
        }
    }

    fn close_success(current: LifecycleWord) -> LifecycleWord {
        crash_close(current).unwrap()
    }

    fn complete_reset(current: LifecycleWord) -> LifecycleWord {
        finish_success(
            claim(current, TransactionClass::ResetBarrier).unwrap(),
            false,
        )
    }

    #[test]
    fn raw_layout_matches_the_atomic_kmd_control_word() {
        assert_eq!(LifecycleWord::INITIAL.into_atomic(), 0);
        for (raw, predicate) in [
            (1, LifecycleWord::has_writer as fn(LifecycleWord) -> bool),
            (2, LifecycleWord::is_present),
            (4, LifecycleWord::is_reset_closed),
            (8, LifecycleWord::is_removed),
            (16, LifecycleWord::is_poisoned),
        ] {
            assert!(predicate(LifecycleWord::from_atomic(raw)));
        }
        assert_eq!(LifecycleWord::from_atomic(32).revision(), 1);
        assert_eq!(
            LifecycleWord::from_atomic(u64::MAX).revision(),
            REVISION_MAX
        );

        let token = claim(LifecycleWord::INITIAL, TransactionClass::Normal).unwrap();
        assert_eq!(token.claimed_word().into_atomic(), 33);
        assert_eq!(finish_success(token, true).into_atomic(), 66);
    }

    #[test]
    fn every_flag_combination_and_boundary_revision_matches_the_explicit_table() {
        let revisions = [
            0,
            1,
            REVISION_MAX - 4,
            REVISION_MAX - 3,
            REVISION_MAX - 2,
            REVISION_MAX - 1,
            REVISION_MAX,
        ];
        let classes = [
            TransactionClass::Normal,
            TransactionClass::ResetBarrier,
            TransactionClass::Remove,
        ];

        for revision in revisions {
            for flags in 0..=FLAGS {
                let current = LifecycleWord::from_atomic((revision << REVISION_SHIFT) | flags);
                let writer = flags & WRITER != 0;
                let present = flags & PRESENT != 0;
                let reset_closed = flags & RESET_CLOSED != 0;
                let removed = flags & REMOVED != 0;
                let poisoned = flags & POISONED != 0;

                let expected_read = if poisoned {
                    ReadState::Poisoned
                } else if removed {
                    ReadState::Removed
                } else if reset_closed {
                    ReadState::ResetClosed
                } else if writer {
                    ReadState::WriterInFlight
                } else {
                    ReadState::Readable { present }
                };
                assert_eq!(read_state(current), expected_read);

                let expected_interruption = if poisoned {
                    InterruptedBy::Poison
                } else if removed {
                    InterruptedBy::Remove
                } else if reset_closed {
                    InterruptedBy::Reset
                } else {
                    InterruptedBy::LifecycleChange
                };
                assert_eq!(classify_interruption(current), expected_interruption);

                for class in classes {
                    let expected_classification = if poisoned {
                        ClaimRefusal::Poisoned
                    } else if removed {
                        ClaimRefusal::Removed
                    } else if class == TransactionClass::Normal && reset_closed {
                        ClaimRefusal::ResetClosed
                    } else if writer {
                        ClaimRefusal::WriterCollision
                    } else {
                        ClaimRefusal::LifecycleChanged
                    };
                    assert_eq!(
                        classify_claim_failure(class, current),
                        expected_classification
                    );

                    let expected_claim_refusal = if poisoned {
                        Some(ClaimRefusal::Poisoned)
                    } else if removed {
                        Some(ClaimRefusal::Removed)
                    } else if class == TransactionClass::Normal && reset_closed {
                        Some(ClaimRefusal::ResetClosed)
                    } else if class == TransactionClass::ResetBarrier && !reset_closed {
                        Some(ClaimRefusal::ResetNotClosed)
                    } else if writer {
                        Some(ClaimRefusal::WriterCollision)
                    } else if revision > REVISION_MAX - 2 {
                        Some(ClaimRefusal::RevisionExhausted)
                    } else {
                        None
                    };
                    match (claim(current, class), expected_claim_refusal) {
                        (Ok(token), None) => {
                            assert_eq!(token.claimed_word(), word(revision + 1, flags | WRITER))
                        }
                        (Err(refused), Some(expected)) => {
                            assert_eq!(refused.reason(), expected);
                            let expected_lifecycle = if expected == ClaimRefusal::RevisionExhausted
                            {
                                word(revision, flags | POISONED)
                            } else {
                                current
                            };
                            assert_eq!(refused.lifecycle(), expected_lifecycle);
                        }
                        (Ok(_), Some(expected)) => {
                            panic!("claim unexpectedly admitted {class:?} with {expected:?}")
                        }
                        (Err(refused), None) => {
                            panic!("claim unexpectedly refused {class:?}: {refused:?}")
                        }
                    }
                }

                let required_revisions = if writer { 4 } else { 3 };
                let expected_close_refusal = if poisoned {
                    Some(CrashCloseRefusal::Poisoned)
                } else if removed {
                    Some(CrashCloseRefusal::Removed)
                } else if revision > REVISION_MAX - required_revisions {
                    Some(CrashCloseRefusal::RevisionExhausted)
                } else {
                    None
                };
                match (crash_close(current), expected_close_refusal) {
                    (Ok(closed), None) => {
                        assert_eq!(closed, word(revision + 1, flags | RESET_CLOSED))
                    }
                    (Err(refused), Some(expected)) => {
                        assert_eq!(refused.reason(), expected);
                        let expected_lifecycle = if expected == CrashCloseRefusal::RevisionExhausted
                        {
                            word(revision, flags | RESET_CLOSED | POISONED)
                        } else {
                            current
                        };
                        assert_eq!(refused.lifecycle(), expected_lifecycle);
                    }
                    (Ok(_), Some(expected)) => {
                        panic!("close unexpectedly admitted with {expected:?}")
                    }
                    (Err(refused), None) => {
                        panic!("close unexpectedly refused: {refused:?}")
                    }
                }

                let expected_tombstone = if !removed {
                    Err(RemovedTombstoneRefusal::NotRemoved)
                } else if poisoned {
                    Err(RemovedTombstoneRefusal::Poisoned)
                } else if writer {
                    Err(RemovedTombstoneRefusal::WriterHeld)
                } else if present {
                    Err(RemovedTombstoneRefusal::Present)
                } else if reset_closed {
                    Err(RemovedTombstoneRefusal::ResetClosed)
                } else if revision == 0 {
                    Err(RemovedTombstoneRefusal::RevisionZero)
                } else {
                    Ok(())
                };
                assert_eq!(validate_removed_tombstone(current), expected_tombstone);
            }
        }
    }

    #[test]
    fn initial_active_and_empty_publications_have_exact_words() {
        assert_eq!(
            read_state(LifecycleWord::INITIAL),
            ReadState::Readable { present: false }
        );

        let active_claim = claim(LifecycleWord::INITIAL, TransactionClass::Normal).unwrap();
        assert_eq!(active_claim.claimed_word(), word(1, WRITER));
        let active = finish_success(active_claim, true);
        assert_eq!(active, word(2, PRESENT));
        assert_eq!(read_state(active), ReadState::Readable { present: true });

        let empty_claim = claim(active, TransactionClass::Normal).unwrap();
        assert_eq!(empty_claim.claimed_word(), word(3, WRITER | PRESENT));
        let empty = finish_success(empty_claim, false);
        assert_eq!(empty, word(4, 0));
        assert_eq!(read_state(empty), ReadState::Readable { present: false });
    }

    #[test]
    fn crash_close_before_claim_blocks_normal_writers_until_barrier_completion() {
        let active = word(2, PRESENT);
        let closed = close_success(active);
        assert_eq!(closed, word(3, PRESENT | RESET_CLOSED));
        assert_eq!(read_state(closed), ReadState::ResetClosed);
        assert_eq!(
            claim(closed, TransactionClass::Normal)
                .unwrap_err()
                .reason(),
            ClaimRefusal::ResetClosed
        );

        let reset = complete_reset(closed);
        assert_eq!(reset, word(5, 0));
        assert_eq!(read_state(reset), ReadState::Readable { present: false });
    }

    #[derive(Clone, Copy)]
    enum CutPoint {
        AfterClaimBeforePayload,
        MidPayload,
        BeforeFinalCas,
    }

    #[test]
    fn every_pre_final_crash_cut_restores_payload_before_writer_clear() {
        for cut in [
            CutPoint::AfterClaimBeforePayload,
            CutPoint::MidPayload,
            CutPoint::BeforeFinalCas,
        ] {
            let stable = word(2, PRESENT);
            let token = claim(stable, TransactionClass::Normal).unwrap();
            let claimed = token.claimed_word();
            let previous_payload = 0x11_u64;
            let (mut payload, payload_was_touched) = match cut {
                CutPoint::AfterClaimBeforePayload => (previous_payload, false),
                CutPoint::MidPayload => (0xdead_beef, true),
                CutPoint::BeforeFinalCas => (0x22, true),
            };
            assert_eq!(payload != previous_payload, payload_was_touched);

            let closed = close_success(claimed);
            assert_eq!(closed, word(4, WRITER | PRESENT | RESET_CLOSED));
            let plan = token.finish(true);
            assert_eq!(
                plan.decide(closed),
                FinishDecision::RestorePreviousThenClear(InterruptedBy::Reset)
            );
            assert!(closed.has_writer());
            payload = previous_payload;
            let cleared = clear_interrupted(closed, false);
            assert_eq!(cleared.issue(), None);
            assert_eq!(cleared.lifecycle(), word(5, PRESENT | RESET_CLOSED));
            assert_eq!(payload, previous_payload);

            let reset = complete_reset(cleared.lifecycle());
            assert_eq!(reset, word(7, 0));
        }
    }

    #[test]
    fn crash_close_after_final_cas_resets_the_landed_publication() {
        let token = claim(word(2, PRESENT), TransactionClass::Normal).unwrap();
        let landed = finish_success(token, true);
        assert_eq!(landed, word(4, PRESENT));
        let closed = close_success(landed);
        assert_eq!(closed, word(5, PRESENT | RESET_CLOSED));
        assert_eq!(complete_reset(closed), word(7, 0));
    }

    #[test]
    fn a_close_during_the_barrier_forces_rollback_and_a_fresh_barrier() {
        let closed = close_success(word(2, PRESENT));
        let token = claim(closed, TransactionClass::ResetBarrier).unwrap();
        let second_close = close_success(token.claimed_word());
        let plan = token.finish(false);
        assert_eq!(
            plan.decide(second_close),
            FinishDecision::RestorePreviousThenClear(InterruptedBy::Reset)
        );
        let cleared = clear_interrupted(second_close, false).lifecycle();
        assert_eq!(cleared, word(6, PRESENT | RESET_CLOSED));
        assert_eq!(complete_reset(cleared), word(8, 0));
    }

    #[test]
    fn collision_and_changed_lifecycle_are_distinct_claim_failures() {
        let stable = word(10, PRESENT);
        let first = claim(stable, TransactionClass::Normal).unwrap();
        let found_writer = first.claimed_word();
        assert_eq!(
            classify_claim_failure(TransactionClass::Normal, found_writer),
            ClaimRefusal::WriterCollision
        );

        let completed = finish_success(first, true);
        assert_eq!(
            classify_claim_failure(TransactionClass::Normal, completed),
            ClaimRefusal::LifecycleChanged
        );
        assert_eq!(
            claim(found_writer, TransactionClass::Normal)
                .unwrap_err()
                .reason(),
            ClaimRefusal::WriterCollision
        );
    }

    #[test]
    fn every_transaction_class_obeys_its_reset_gate() {
        let open = word(2, PRESENT);
        let closed = word(3, PRESENT | RESET_CLOSED);
        assert_eq!(
            claim(open, TransactionClass::ResetBarrier)
                .unwrap_err()
                .reason(),
            ClaimRefusal::ResetNotClosed
        );
        assert_eq!(
            claim(closed, TransactionClass::Normal)
                .unwrap_err()
                .reason(),
            ClaimRefusal::ResetClosed
        );
        assert!(claim(closed, TransactionClass::ResetBarrier).is_ok());
        assert!(claim(closed, TransactionClass::Remove).is_ok());

        let closed_writer = word(4, WRITER | PRESENT | RESET_CLOSED);
        assert_eq!(
            claim(closed_writer, TransactionClass::Normal)
                .unwrap_err()
                .reason(),
            ClaimRefusal::ResetClosed
        );
        assert_eq!(
            claim(closed_writer, TransactionClass::Remove)
                .unwrap_err()
                .reason(),
            ClaimRefusal::WriterCollision
        );
    }

    #[test]
    fn finish_requires_the_exact_claim_token_word() {
        let token = claim(word(8, PRESENT), TransactionClass::Normal).unwrap();
        let expected = token.claimed_word();
        let plan = token.finish(true);
        assert_eq!(
            plan.decide(word(expected.revision() + 1, WRITER | PRESENT)),
            FinishDecision::RestorePreviousThenClear(InterruptedBy::LifecycleChange)
        );
    }

    #[test]
    fn terminal_precedence_matches_the_kmd_reader_and_interruption_paths() {
        assert_eq!(read_state(word(1, WRITER)), ReadState::WriterInFlight);
        assert_eq!(
            read_state(word(1, WRITER | RESET_CLOSED)),
            ReadState::ResetClosed
        );
        assert_eq!(
            read_state(word(1, WRITER | RESET_CLOSED | REMOVED)),
            ReadState::Removed
        );
        let all = word(1, WRITER | RESET_CLOSED | REMOVED | POISONED);
        assert_eq!(read_state(all), ReadState::Poisoned);
        assert_eq!(classify_interruption(all), InterruptedBy::Poison);
        assert_eq!(
            classify_claim_failure(TransactionClass::Normal, all),
            ClaimRefusal::Poisoned
        );
        assert_eq!(
            crash_close(all).unwrap_err().reason(),
            CrashCloseRefusal::Poisoned
        );
    }

    #[test]
    fn removed_tombstones_are_exact_and_poison_is_a_separate_terminal() {
        assert_eq!(validate_removed_tombstone(word(2, REMOVED)), Ok(()));
        for (candidate, expected) in [
            (word(2, 0), RemovedTombstoneRefusal::NotRemoved),
            (
                word(2, REMOVED | POISONED),
                RemovedTombstoneRefusal::Poisoned,
            ),
            (
                word(2, REMOVED | WRITER),
                RemovedTombstoneRefusal::WriterHeld,
            ),
            (word(2, REMOVED | PRESENT), RemovedTombstoneRefusal::Present),
            (
                word(2, REMOVED | RESET_CLOSED),
                RemovedTombstoneRefusal::ResetClosed,
            ),
            (word(0, REMOVED), RemovedTombstoneRefusal::RevisionZero),
        ] {
            assert_eq!(validate_removed_tombstone(candidate), Err(expected));
        }
    }

    #[test]
    fn poison_preserves_revision_and_every_other_flag() {
        let current = word(17, WRITER | PRESENT | RESET_CLOSED | REMOVED);
        assert_eq!(
            poison(current),
            word(17, WRITER | PRESENT | RESET_CLOSED | REMOVED | POISONED)
        );
    }

    #[test]
    fn atomic_fallback_plans_are_the_exact_unconditional_rmw_operands() {
        let current = word(17, WRITER | PRESENT | REMOVED);
        let poison_plan = poison_atomic_fallback();
        assert_eq!(poison_plan.set_operand(), 16);
        assert_eq!(poison_plan.clear_operand(), None);
        assert_eq!(poison_plan.apply(current), poison(current));

        let close_plan = crash_close_atomic_fallback();
        assert_eq!(close_plan.set_operand(), 20);
        assert_eq!(close_plan.clear_operand(), None);
        assert_eq!(
            close_plan.apply(current),
            word(17, WRITER | PRESENT | RESET_CLOSED | REMOVED | POISONED)
        );

        let clear_plan = clear_writer_atomic_fallback();
        assert_eq!(clear_plan.set_operand(), 16);
        assert_eq!(clear_plan.clear_operand(), Some(u64::MAX - 1));
        assert_eq!(
            clear_plan.apply(current),
            word(17, PRESENT | REMOVED | POISONED)
        );
    }

    #[test]
    fn interrupted_clear_preserves_flags_and_advances_once() {
        let interrupted = word(19, WRITER | PRESENT | RESET_CLOSED);
        let clear = clear_interrupted(interrupted, false);
        assert_eq!(clear.issue(), None);
        assert_eq!(clear.lifecycle(), word(20, PRESENT | RESET_CLOSED));

        let poisoned = clear_interrupted(interrupted, true);
        assert_eq!(poisoned.issue(), None);
        assert_eq!(
            poisoned.lifecycle(),
            word(20, PRESENT | RESET_CLOSED | POISONED)
        );
    }

    #[test]
    fn a_post_claim_generation_failure_poison_clears_without_publishing() {
        let stable = word(2, PRESENT);
        let claimed = claim(stable, TransactionClass::Normal)
            .unwrap()
            .claimed_word();
        let clear = clear_interrupted(claimed, true);
        assert_eq!(clear.issue(), None);
        assert_eq!(clear.lifecycle(), word(4, PRESENT | POISONED));
    }

    #[test]
    fn reset_close_reserves_exactly_three_or_four_revisions() {
        let no_writer = word(REVISION_MAX - 3, PRESENT);
        let closed = close_success(no_writer);
        assert_eq!(closed.revision(), REVISION_MAX - 2);
        assert_eq!(complete_reset(closed).revision(), REVISION_MAX);

        let writer = word(REVISION_MAX - 4, WRITER | PRESENT);
        let closed_writer = close_success(writer);
        assert_eq!(closed_writer.revision(), REVISION_MAX - 3);
        let cleared = clear_interrupted(closed_writer, false).lifecycle();
        assert_eq!(cleared.revision(), REVISION_MAX - 2);
        assert_eq!(complete_reset(cleared).revision(), REVISION_MAX);
    }

    #[test]
    fn max_minus_four_through_max_fail_closed_without_wrap() {
        for revision in [REVISION_MAX - 4, REVISION_MAX - 3, REVISION_MAX - 2] {
            let token = claim(word(revision, PRESENT), TransactionClass::Normal).unwrap();
            assert_eq!(token.claimed_word().revision(), revision + 1);
            assert_eq!(finish_success(token, true).revision(), revision + 2);
        }
        for revision in [REVISION_MAX - 1, REVISION_MAX] {
            let refused = claim(word(revision, PRESENT), TransactionClass::Normal).unwrap_err();
            assert_eq!(refused.reason(), ClaimRefusal::RevisionExhausted);
            assert_eq!(refused.lifecycle().revision(), revision);
            assert!(refused.lifecycle().is_poisoned());
        }

        let final_claim = claim(word(REVISION_MAX - 2, PRESENT), TransactionClass::Normal).unwrap();
        assert_eq!(final_claim.claimed_word().revision(), REVISION_MAX - 1);
        assert_eq!(finish_success(final_claim, true).revision(), REVISION_MAX);

        for revision in [REVISION_MAX - 2, REVISION_MAX - 1, REVISION_MAX] {
            let refused = crash_close(word(revision, PRESENT)).unwrap_err();
            assert_eq!(refused.reason(), CrashCloseRefusal::RevisionExhausted);
            assert_eq!(refused.lifecycle().revision(), revision);
            assert!(refused.lifecycle().is_reset_closed());
            assert!(refused.lifecycle().is_poisoned());
        }

        for revision in [
            REVISION_MAX - 3,
            REVISION_MAX - 2,
            REVISION_MAX - 1,
            REVISION_MAX,
        ] {
            let refused = crash_close(word(revision, WRITER | PRESENT)).unwrap_err();
            assert_eq!(refused.reason(), CrashCloseRefusal::RevisionExhausted);
            assert_eq!(refused.lifecycle().revision(), revision);
            assert!(refused.lifecycle().is_reset_closed());
            assert!(refused.lifecycle().is_poisoned());
        }
    }

    #[test]
    fn clear_at_max_and_force_fallbacks_poison_without_wrap() {
        for revision in [
            REVISION_MAX - 4,
            REVISION_MAX - 3,
            REVISION_MAX - 2,
            REVISION_MAX - 1,
        ] {
            let clear = clear_interrupted(word(revision, WRITER | PRESENT), false);
            assert_eq!(clear.issue(), None);
            assert_eq!(clear.lifecycle(), word(revision + 1, PRESENT));
        }

        let at_max = word(REVISION_MAX, WRITER | PRESENT | RESET_CLOSED);
        let clear = clear_interrupted(at_max, false);
        assert_eq!(clear.issue(), Some(ClearIssue::RevisionExhausted));
        assert_eq!(
            clear.lifecycle(),
            word(REVISION_MAX, PRESENT | RESET_CLOSED | POISONED)
        );

        let missing = clear_interrupted(word(7, PRESENT), false);
        assert_eq!(missing.issue(), Some(ClearIssue::ContentionExhausted));
        assert_eq!(missing.lifecycle(), word(7, PRESENT | POISONED));

        let forced_clear = force_clear_after_contention(word(7, WRITER | PRESENT));
        assert_eq!(forced_clear.lifecycle(), word(7, PRESENT | POISONED));
        assert_eq!(forced_clear.issue(), Some(ClearIssue::ContentionExhausted));
        let forced_close = force_crash_close_after_contention(word(7, WRITER | PRESENT));
        assert_eq!(
            forced_close.lifecycle(),
            word(7, WRITER | PRESENT | RESET_CLOSED | POISONED)
        );
        assert_eq!(
            forced_close.reason(),
            CrashCloseRefusal::ContentionExhausted
        );
    }

    #[test]
    fn repeated_reset_and_remove_transitions_are_terminally_ordered() {
        let once = close_success(word(2, PRESENT));
        let twice = close_success(once);
        assert_eq!(twice, word(4, PRESENT | RESET_CLOSED));
        let reset = complete_reset(twice);
        assert_eq!(reset, word(6, 0));
        assert_eq!(
            claim(reset, TransactionClass::ResetBarrier)
                .unwrap_err()
                .reason(),
            ClaimRefusal::ResetNotClosed
        );

        let remove = finish_success(claim(reset, TransactionClass::Remove).unwrap(), false);
        assert_eq!(remove, word(8, REMOVED));
        assert_eq!(read_state(remove), ReadState::Removed);
        for class in [
            TransactionClass::Normal,
            TransactionClass::ResetBarrier,
            TransactionClass::Remove,
        ] {
            assert_eq!(
                claim(remove, class).unwrap_err().reason(),
                ClaimRefusal::Removed
            );
        }
        assert_eq!(
            crash_close(remove).unwrap_err().reason(),
            CrashCloseRefusal::Removed
        );
    }

    #[test]
    fn remove_clears_present_and_an_outstanding_reset_close() {
        let closed = close_success(word(2, PRESENT));
        let removed = finish_success(claim(closed, TransactionClass::Remove).unwrap(), false);
        assert_eq!(removed, word(5, REMOVED));
    }
}
