//! Atomic committed-mode coordinator whose reset close can preempt an in-flight writer.

#![allow(
    dead_code,
    reason = "D2 committed-mode storage remains unwired until adapter integration"
)]

use core::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::{
    committed_mode::{
        CommittedModePublication, CommittedModeState, ModeCommitFacts, ModePolicySnapshot,
        PowerSubject, PublicationReason, Refusal as LogicRefusal,
    },
    committed_mode_lifecycle::{
        claim, classify_claim_failure, classify_interruption, clear_interrupted,
        clear_writer_atomic_fallback, crash_close, crash_close_atomic_fallback,
        poison_atomic_fallback, read_state, validate_removed_tombstone, ClaimRefusal, ClaimToken,
        ClearIssue, CrashCloseRefusal, FinishDecision, InterruptedBy, LifecycleWord, ReadState,
        RemovedTombstoneRefusal, TransactionClass,
    },
    direct_scanout_admission::CommittedMode,
    SEQ_READ_ATTEMPTS,
};

use crate::irql::PassiveLevel;

const MODE_ACTIVE: u32 = 1 << 0;
const MODE_VISIBLE: u32 = 1 << 1;
const MODE_POWERED: u32 = 1 << 2;
const MODE_FLAGS: u32 = MODE_ACTIVE | MODE_VISIBLE | MODE_POWERED;

const POLICY_VISIBLE: u32 = 1 << 0;
const POLICY_ADAPTER_POWERED: u32 = 1 << 1;
const POLICY_TARGET_POWERED: u32 = 1 << 2;
const POLICY_PATH_KNOWN: u32 = 1 << 3;
const POLICY_PATH_POWERED: u32 = 1 << 4;
const POLICY_FLAGS: u32 = POLICY_VISIBLE
    | POLICY_ADAPTER_POWERED
    | POLICY_TARGET_POWERED
    | POLICY_PATH_KNOWN
    | POLICY_PATH_POWERED;

const REASON_NONE: u32 = 0;
const REASON_COMMITTED: u32 = 1;
const REASON_VISIBILITY: u32 = 2;
const REASON_POWER: u32 = 3;
const REASON_INVALIDATED: u32 = 4;
const REASON_RESET: u32 = 5;
const REASON_REMOVED: u32 = 6;

const LOGIC_REFUSAL_COUNT: usize = 19;
const CLAIM_REFUSAL_COUNT: usize = 7;
const CLEAR_ISSUE_COUNT: usize = 2;
const CRASH_CLOSE_REFUSAL_COUNT: usize = 4;
const REMOVED_TOMBSTONE_REFUSAL_COUNT: usize = 6;

pub(crate) static COMMITTED_MODE_READ_RETRIES: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_ABSENT: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_RESET_CLOSED: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_REMOVED: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_POISONED: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_RETRY_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_CORRUPT: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_STORED_STATE_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_STORED_REASON_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_READ_STORED_PAYLOAD_INVALID: AtomicU32 = AtomicU32::new(0);

pub(crate) static COMMITTED_MODE_WRITE_CLAIM_REFUSALS: [AtomicU32; CLAIM_REFUSAL_COUNT] =
    [const { AtomicU32::new(0) }; CLAIM_REFUSAL_COUNT];
pub(crate) static COMMITTED_MODE_WRITE_CLEAR_ISSUES: [AtomicU32; CLEAR_ISSUE_COUNT] =
    [const { AtomicU32::new(0) }; CLEAR_ISSUE_COUNT];
pub(crate) static COMMITTED_MODE_WRITE_GENERATION_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_STORED_STATE_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_STORED_REASON_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_STORED_PAYLOAD_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_TRANSITION_REFUSED: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_PUBLICATION_INVARIANT_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_LIFECYCLE_CORRUPT: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_INTERRUPTED_BY_RESET: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_WRITE_INTERRUPTED_BY_POISON: AtomicU32 = AtomicU32::new(0);

pub(crate) static COMMITTED_MODE_RESET_CLOSE_REFUSALS: [AtomicU32; CRASH_CLOSE_REFUSAL_COUNT] =
    [const { AtomicU32::new(0) }; CRASH_CLOSE_REFUSAL_COUNT];
pub(crate) static COMMITTED_MODE_RESET_CLOSE_LIFECYCLE_CORRUPT: AtomicU32 = AtomicU32::new(0);

pub(crate) static COMMITTED_MODE_REMOVED_TOMBSTONE_REFUSALS: [AtomicU32;
    REMOVED_TOMBSTONE_REFUSAL_COUNT] =
    [const { AtomicU32::new(0) }; REMOVED_TOMBSTONE_REFUSAL_COUNT];
pub(crate) static COMMITTED_MODE_REMOVED_STORED_STATE_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_REMOVED_STORED_REASON_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_REMOVED_STORED_PAYLOAD_INVALID: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_REMOVED_RETRY_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
pub(crate) static COMMITTED_MODE_LOGIC_REFUSALS: [AtomicU32; LOGIC_REFUSAL_COUNT] =
    [const { AtomicU32::new(0) }; LOGIC_REFUSAL_COUNT];

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CommittedModeObservation {
    lifecycle: LifecycleWord,
    mode: CommittedMode,
    reason: PublicationReason,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CommittedModeRead {
    Present(CommittedModeObservation),
    Absent {
        generation: u64,
        reason: Option<PublicationReason>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommittedModeReadRefusal {
    ResetClosed,
    Removed,
    Poisoned,
    RetryExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommittedModeWriteRefusal {
    Claim(ClaimRefusal),
    Clear(ClearIssue),
    GenerationExhausted,
    StoredStateInvalid(LogicRefusal),
    StoredReasonInvalid,
    StoredPayloadInvalid,
    TransitionRefused(LogicRefusal),
    PublicationInvariantInvalid,
    LifecycleCorrupt,
    InterruptedByReset,
    InterruptedByPoison,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommittedModeResetCloseRefusal {
    Lifecycle(CrashCloseRefusal),
    LifecycleCorrupt,
}

#[derive(Clone, Copy)]
enum Transition {
    PublishActive(ModeCommitFacts),
    PublishEmpty {
        source_id: u32,
    },
    Visibility {
        source_id: u32,
        visible: bool,
    },
    Power {
        subject: PowerSubject,
        powered: bool,
    },
    Invalidate {
        source_id: u32,
    },
    Reset,
    Remove,
}

impl Transition {
    const fn transaction_class(self) -> TransactionClass {
        match self {
            Self::Reset => TransactionClass::ResetBarrier,
            Self::Remove => TransactionClass::Remove,
            _ => TransactionClass::Normal,
        }
    }
}

#[derive(Clone, Copy)]
struct StoredSnapshot {
    high_water: u64,
    mode: Option<CommittedMode>,
    reason: Option<PublicationReason>,
    policy: ModePolicySnapshot,
}

#[derive(Clone, Copy)]
struct RawSnapshot {
    high_water: u64,
    source_id: u32,
    target_id: u32,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    mode_state: u32,
    policy_state: u32,
    reason: u32,
    present: bool,
}

enum StoredValidationError {
    Logic(LogicRefusal),
    Reason,
    Payload,
}

struct RestoredSnapshot {
    stored: StoredSnapshot,
    state: CommittedModeState,
}

struct PlatformClearResult {
    word: LifecycleWord,
    issue: Option<ClearIssue>,
}

enum RemovedTerminalError {
    Lifecycle(RemovedTombstoneRefusal),
    StoredState(LogicRefusal),
    StoredReason,
    StoredPayload,
    RetryExhausted,
}

pub(crate) struct CommittedModeStorage {
    lifecycle: AtomicU64,
    generation: AtomicU64,
    source_id: AtomicU32,
    target_id: AtomicU32,
    source_width: AtomicU32,
    source_height: AtomicU32,
    target_width: AtomicU32,
    target_height: AtomicU32,
    mode_state: AtomicU32,
    policy_state: AtomicU32,
    reason: AtomicU32,
}

impl CommittedModeStorage {
    pub(crate) const fn new() -> Self {
        Self {
            lifecycle: AtomicU64::new(LifecycleWord::INITIAL.into_atomic()),
            generation: AtomicU64::new(0),
            source_id: AtomicU32::new(0),
            target_id: AtomicU32::new(0),
            source_width: AtomicU32::new(0),
            source_height: AtomicU32::new(0),
            target_width: AtomicU32::new(0),
            target_height: AtomicU32::new(0),
            mode_state: AtomicU32::new(0),
            policy_state: AtomicU32::new(0),
            reason: AtomicU32::new(REASON_NONE),
        }
    }

    pub(crate) fn read(&self) -> Result<CommittedModeRead, CommittedModeReadRefusal> {
        for _ in 0..SEQ_READ_ATTEMPTS {
            let before = self.load_lifecycle(Ordering::Acquire);
            match read_state(before) {
                ReadState::Poisoned => {
                    return Err(record_read_refusal(CommittedModeReadRefusal::Poisoned));
                }
                ReadState::Removed => {
                    if self.validate_removed_terminal(before).is_err() {
                        COMMITTED_MODE_READ_CORRUPT.fetch_add(1, Ordering::Relaxed);
                        self.mark_poisoned();
                        return Err(record_read_refusal(CommittedModeReadRefusal::Poisoned));
                    }
                    return Err(record_read_refusal(CommittedModeReadRefusal::Removed));
                }
                ReadState::ResetClosed => {
                    return Err(record_read_refusal(CommittedModeReadRefusal::ResetClosed));
                }
                ReadState::WriterInFlight => {
                    COMMITTED_MODE_READ_RETRIES.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                ReadState::Readable { .. } => {}
            }

            let raw = self.load_raw(before);
            fence(Ordering::Acquire);
            let after = self.load_lifecycle(Ordering::Relaxed);
            if before != after {
                COMMITTED_MODE_READ_RETRIES.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            let restored = match restore_snapshot(raw, false) {
                Ok(restored) => restored,
                Err(error) => {
                    record_read_stored_validation_error(error);
                    COMMITTED_MODE_READ_CORRUPT.fetch_add(1, Ordering::Relaxed);
                    self.mark_poisoned();
                    return Err(record_read_refusal(CommittedModeReadRefusal::Poisoned));
                }
            };
            return match (restored.stored.mode, restored.stored.reason) {
                (Some(mode), Some(reason)) => {
                    Ok(CommittedModeRead::Present(CommittedModeObservation {
                        lifecycle: before,
                        mode,
                        reason,
                    }))
                }
                (None, reason) => {
                    COMMITTED_MODE_READ_ABSENT.fetch_add(1, Ordering::Relaxed);
                    Ok(CommittedModeRead::Absent {
                        generation: restored.stored.high_water,
                        reason,
                    })
                }
                (Some(_), None) => {
                    COMMITTED_MODE_READ_STORED_REASON_INVALID.fetch_add(1, Ordering::Relaxed);
                    COMMITTED_MODE_READ_CORRUPT.fetch_add(1, Ordering::Relaxed);
                    self.mark_poisoned();
                    Err(record_read_refusal(CommittedModeReadRefusal::Poisoned))
                }
            };
        }

        Err(record_read_refusal(
            CommittedModeReadRefusal::RetryExhausted,
        ))
    }

    pub(crate) fn publish_active(
        &self,
        passive: PassiveLevel,
        facts: ModeCommitFacts,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        self.transact(passive, Transition::PublishActive(facts))
    }

    pub(crate) fn publish_empty_commit(
        &self,
        passive: PassiveLevel,
        source_id: u32,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        self.transact(passive, Transition::PublishEmpty { source_id })
    }

    pub(crate) fn transition_visibility(
        &self,
        passive: PassiveLevel,
        source_id: u32,
        visible: bool,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        self.transact(passive, Transition::Visibility { source_id, visible })
    }

    pub(crate) fn transition_power(
        &self,
        passive: PassiveLevel,
        subject: PowerSubject,
        powered: bool,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        self.transact(passive, Transition::Power { subject, powered })
    }

    pub(crate) fn invalidate(
        &self,
        passive: PassiveLevel,
        source_id: u32,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        self.transact(passive, Transition::Invalidate { source_id })
    }

    pub(crate) fn crash_reset_close(&self) -> Result<(), CommittedModeResetCloseRefusal> {
        for _ in 0..SEQ_READ_ATTEMPTS {
            let current = self.load_lifecycle(Ordering::Acquire);
            let closed = match crash_close(current) {
                Ok(closed) => closed,
                Err(refused) => {
                    let reason = refused.reason();
                    if reason == CrashCloseRefusal::Removed
                        && self.validate_removed_terminal(current).is_err()
                    {
                        self.mark_poisoned();
                        return Err(record_reset_close_refusal(
                            CommittedModeResetCloseRefusal::LifecycleCorrupt,
                        ));
                    }
                    if reason == CrashCloseRefusal::RevisionExhausted {
                        self.apply_atomic_fallback(crash_close_atomic_fallback());
                    }
                    return Err(record_reset_close_refusal(
                        CommittedModeResetCloseRefusal::Lifecycle(reason),
                    ));
                }
            };
            if self
                .lifecycle
                .compare_exchange(
                    current.into_atomic(),
                    closed.into_atomic(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(());
            }
        }

        self.apply_atomic_fallback(crash_close_atomic_fallback());
        Err(record_reset_close_refusal(
            CommittedModeResetCloseRefusal::Lifecycle(CrashCloseRefusal::ContentionExhausted),
        ))
    }

    pub(crate) fn complete_reset_barrier(
        &self,
        passive: PassiveLevel,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        self.transact(passive, Transition::Reset)
    }

    pub(crate) fn remove_permanently(
        &self,
        passive: PassiveLevel,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        self.transact(passive, Transition::Remove)
    }

    fn transact(
        &self,
        _passive: PassiveLevel,
        transition: Transition,
    ) -> Result<u64, CommittedModeWriteRefusal> {
        let claim = self.claim_writer(transition)?;
        let claimed = claim.claimed_word();
        let restored = match restore_snapshot(self.load_raw(claimed), false) {
            Ok(restored) => restored,
            Err(StoredValidationError::Logic(refusal)) => {
                self.poison_and_clear_writer();
                return Err(record_write_refusal(
                    CommittedModeWriteRefusal::StoredStateInvalid(refusal),
                ));
            }
            Err(StoredValidationError::Reason) => {
                self.poison_and_clear_writer();
                return Err(record_write_refusal(
                    CommittedModeWriteRefusal::StoredReasonInvalid,
                ));
            }
            Err(StoredValidationError::Payload) => {
                self.poison_and_clear_writer();
                return Err(record_write_refusal(
                    CommittedModeWriteRefusal::StoredPayloadInvalid,
                ));
            }
        };
        let previous = restored.stored;
        let mut state = restored.state;
        let generation = match state.next_generation() {
            Ok(generation) => generation,
            Err(LogicRefusal::GenerationExhausted { high_water }) => {
                bump_logic_refusal(LogicRefusal::GenerationExhausted { high_water });
                self.poison_and_clear_writer();
                return Err(record_write_refusal(
                    CommittedModeWriteRefusal::GenerationExhausted,
                ));
            }
            Err(refusal) => {
                self.poison_and_clear_writer();
                return Err(record_write_refusal(
                    CommittedModeWriteRefusal::StoredStateInvalid(refusal),
                ));
            }
        };

        let publication = match apply_transition(&mut state, generation, transition) {
            Ok(publication) => publication,
            Err(refusal) => {
                let refusal =
                    record_write_refusal(CommittedModeWriteRefusal::TransitionRefused(refusal));
                let cleared = self.clear_writer(false);
                let interruption = match classify_interruption(cleared.word) {
                    InterruptedBy::Reset => Some(record_write_refusal(
                        CommittedModeWriteRefusal::InterruptedByReset,
                    )),
                    InterruptedBy::Poison => Some(record_write_refusal(
                        CommittedModeWriteRefusal::InterruptedByPoison,
                    )),
                    InterruptedBy::Remove => {
                        self.mark_poisoned();
                        Some(record_write_refusal(
                            CommittedModeWriteRefusal::LifecycleCorrupt,
                        ))
                    }
                    InterruptedBy::LifecycleChange => None,
                };
                if let Some(issue) = cleared.issue {
                    return Err(record_write_refusal(CommittedModeWriteRefusal::Clear(
                        issue,
                    )));
                }
                return Err(interruption.unwrap_or(refusal));
            }
        };
        let removed = publication.reason() == PublicationReason::Removed;
        if !validate_publication(state, publication, removed) {
            self.poison_and_clear_writer();
            return Err(record_write_refusal(
                CommittedModeWriteRefusal::PublicationInvariantInvalid,
            ));
        }

        let next = StoredSnapshot {
            high_water: publication.generation(),
            mode: publication.mode(),
            reason: Some(publication.reason()),
            policy: publication.policy_snapshot(),
        };
        let finish = claim.finish(next.mode.is_some());
        if matches!(transition, Transition::Remove) {
            if let Err(refusal) = validate_removed_tombstone(finish.desired_word()) {
                record_removed_terminal_error(&RemovedTerminalError::Lifecycle(refusal));
                self.poison_and_clear_writer();
                return Err(record_write_refusal(
                    CommittedModeWriteRefusal::LifecycleCorrupt,
                ));
            }
        }
        self.store_snapshot(next);

        let result = self.lifecycle.compare_exchange(
            finish.expected_word().into_atomic(),
            finish.desired_word().into_atomic(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        if result.is_ok() {
            return Ok(generation);
        }

        let found = LifecycleWord::from_atomic(result.unwrap_err());
        if matches!(finish.decide(found), FinishDecision::Commit(_)) {
            self.store_snapshot(previous);
            let cleared = self.clear_writer(true);
            let refusal = record_write_refusal(CommittedModeWriteRefusal::LifecycleCorrupt);
            if let Some(issue) = cleared.issue {
                return Err(record_write_refusal(CommittedModeWriteRefusal::Clear(
                    issue,
                )));
            }
            return Err(refusal);
        }
        // A closer may change the lifecycle while payload stores are in flight.
        // Restore the prior payload before the owning writer acknowledges that close.
        self.store_snapshot(previous);
        let cleared = self.clear_writer(false);
        let refusal = match classify_interruption(cleared.word) {
            InterruptedBy::Reset => CommittedModeWriteRefusal::InterruptedByReset,
            InterruptedBy::Poison => CommittedModeWriteRefusal::InterruptedByPoison,
            InterruptedBy::LifecycleChange | InterruptedBy::Remove => {
                self.mark_poisoned();
                CommittedModeWriteRefusal::LifecycleCorrupt
            }
        };
        let refusal = record_write_refusal(refusal);
        if let Some(issue) = cleared.issue {
            return Err(record_write_refusal(CommittedModeWriteRefusal::Clear(
                issue,
            )));
        }
        Err(refusal)
    }

    fn claim_writer(
        &self,
        transition: Transition,
    ) -> Result<ClaimToken, CommittedModeWriteRefusal> {
        let class = transition.transaction_class();
        let current = self.load_lifecycle(Ordering::Acquire);
        let token = match claim(current, class) {
            Ok(token) => token,
            Err(refused) => {
                let reason = refused.reason();
                if reason == ClaimRefusal::Removed
                    && self.validate_removed_terminal(current).is_err()
                {
                    self.mark_poisoned();
                    return Err(record_write_refusal(
                        CommittedModeWriteRefusal::LifecycleCorrupt,
                    ));
                }
                if reason == ClaimRefusal::RevisionExhausted {
                    self.apply_atomic_fallback(poison_atomic_fallback());
                }
                return Err(record_write_refusal(CommittedModeWriteRefusal::Claim(
                    reason,
                )));
            }
        };
        let claimed = token.claimed_word();
        match self.lifecycle.compare_exchange(
            current.into_atomic(),
            claimed.into_atomic(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(token),
            Err(found) => {
                let found = LifecycleWord::from_atomic(found);
                let reason = classify_claim_failure(class, found);
                if reason == ClaimRefusal::Removed && self.validate_removed_terminal(found).is_err()
                {
                    self.mark_poisoned();
                    return Err(record_write_refusal(
                        CommittedModeWriteRefusal::LifecycleCorrupt,
                    ));
                }
                Err(record_write_refusal(CommittedModeWriteRefusal::Claim(
                    reason,
                )))
            }
        }
    }

    fn load_lifecycle(&self, ordering: Ordering) -> LifecycleWord {
        LifecycleWord::from_atomic(self.lifecycle.load(ordering))
    }

    fn load_raw(&self, lifecycle: LifecycleWord) -> RawSnapshot {
        RawSnapshot {
            high_water: self.generation.load(Ordering::Relaxed),
            source_id: self.source_id.load(Ordering::Relaxed),
            target_id: self.target_id.load(Ordering::Relaxed),
            source_width: self.source_width.load(Ordering::Relaxed),
            source_height: self.source_height.load(Ordering::Relaxed),
            target_width: self.target_width.load(Ordering::Relaxed),
            target_height: self.target_height.load(Ordering::Relaxed),
            mode_state: self.mode_state.load(Ordering::Relaxed),
            policy_state: self.policy_state.load(Ordering::Relaxed),
            reason: self.reason.load(Ordering::Relaxed),
            present: lifecycle.is_present(),
        }
    }

    fn store_snapshot(&self, snapshot: StoredSnapshot) {
        self.generation
            .store(snapshot.high_water, Ordering::Relaxed);
        self.reason
            .store(encode_reason(snapshot.reason), Ordering::Relaxed);
        self.source_id
            .store(snapshot.policy.source_id(), Ordering::Relaxed);
        self.target_id
            .store(snapshot.policy.target_id(), Ordering::Relaxed);
        self.policy_state
            .store(encode_policy_state(snapshot.policy), Ordering::Relaxed);
        match snapshot.mode {
            Some(mode) => {
                self.source_width
                    .store(mode.source_width, Ordering::Relaxed);
                self.source_height
                    .store(mode.source_height, Ordering::Relaxed);
                self.target_width
                    .store(mode.target_width, Ordering::Relaxed);
                self.target_height
                    .store(mode.target_height, Ordering::Relaxed);
                self.mode_state
                    .store(encode_mode_state(mode), Ordering::Relaxed);
            }
            None => {
                self.source_width.store(0, Ordering::Relaxed);
                self.source_height.store(0, Ordering::Relaxed);
                self.target_width.store(0, Ordering::Relaxed);
                self.target_height.store(0, Ordering::Relaxed);
                self.mode_state.store(0, Ordering::Relaxed);
            }
        }
    }

    fn validate_removed_terminal(
        &self,
        initial: LifecycleWord,
    ) -> Result<(), RemovedTerminalError> {
        let mut before = initial;
        for _ in 0..SEQ_READ_ATTEMPTS {
            if let Err(refusal) = validate_removed_tombstone(before) {
                let error = RemovedTerminalError::Lifecycle(refusal);
                record_removed_terminal_error(&error);
                return Err(error);
            }
            let raw = self.load_raw(before);
            fence(Ordering::Acquire);
            let after = self.load_lifecycle(Ordering::Relaxed);
            if before != after {
                before = self.load_lifecycle(Ordering::Acquire);
                continue;
            }
            restore_snapshot(raw, true).map_err(|error| {
                let error = match error {
                    StoredValidationError::Logic(refusal) => {
                        RemovedTerminalError::StoredState(refusal)
                    }
                    StoredValidationError::Reason => RemovedTerminalError::StoredReason,
                    StoredValidationError::Payload => RemovedTerminalError::StoredPayload,
                };
                record_removed_terminal_error(&error);
                error
            })?;
            return Ok(());
        }
        let error = RemovedTerminalError::RetryExhausted;
        record_removed_terminal_error(&error);
        Err(error)
    }

    fn mark_poisoned(&self) {
        self.apply_atomic_fallback(poison_atomic_fallback());
    }

    fn apply_atomic_fallback(
        &self,
        plan: helios_kmd_logic::committed_mode_lifecycle::AtomicFallbackPlan,
    ) -> LifecycleWord {
        self.lifecycle
            .fetch_or(plan.set_operand(), Ordering::AcqRel);
        if let Some(clear) = plan.clear_operand() {
            self.lifecycle.fetch_and(clear, Ordering::AcqRel);
        }
        self.load_lifecycle(Ordering::Acquire)
    }

    fn poison_and_clear_writer(&self) {
        if let Some(issue) = self.clear_writer(true).issue {
            let _ = record_write_refusal(CommittedModeWriteRefusal::Clear(issue));
        }
    }

    fn clear_writer(&self, poison_on_clear: bool) -> PlatformClearResult {
        for _ in 0..SEQ_READ_ATTEMPTS {
            let current = self.load_lifecycle(Ordering::Acquire);
            let cleared = clear_interrupted(current, poison_on_clear);
            if self
                .lifecycle
                .compare_exchange(
                    current.into_atomic(),
                    cleared.lifecycle().into_atomic(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return PlatformClearResult {
                    word: cleared.lifecycle(),
                    issue: cleared.issue(),
                };
            }
        }

        let word = self.apply_atomic_fallback(clear_writer_atomic_fallback());
        PlatformClearResult {
            word,
            issue: Some(ClearIssue::ContentionExhausted),
        }
    }
}

fn restore_snapshot(
    raw: RawSnapshot,
    removed: bool,
) -> Result<RestoredSnapshot, StoredValidationError> {
    if raw.mode_state & !MODE_FLAGS != 0
        || raw.policy_state & !POLICY_FLAGS != 0
        || raw.policy_state & POLICY_PATH_POWERED != 0 && raw.policy_state & POLICY_PATH_KNOWN == 0
        || (!raw.present
            && (raw.source_width != 0
                || raw.source_height != 0
                || raw.target_width != 0
                || raw.target_height != 0
                || raw.mode_state != 0))
    {
        return Err(StoredValidationError::Payload);
    }
    let reason = decode_reason(raw.reason).ok_or(StoredValidationError::Reason)?;
    let path_powered = if raw.policy_state & POLICY_PATH_KNOWN != 0 {
        Some(raw.policy_state & POLICY_PATH_POWERED != 0)
    } else {
        None
    };
    let policy = ModePolicySnapshot::from_stored(
        raw.source_id,
        raw.target_id,
        raw.policy_state & POLICY_VISIBLE != 0,
        raw.policy_state & POLICY_ADAPTER_POWERED != 0,
        raw.policy_state & POLICY_TARGET_POWERED != 0,
        path_powered,
    )
    .map_err(StoredValidationError::Logic)?;
    let mode = raw.present.then_some(CommittedMode {
        generation: raw.high_water,
        source_id: raw.source_id,
        target_id: raw.target_id,
        source_width: raw.source_width,
        source_height: raw.source_height,
        target_width: raw.target_width,
        target_height: raw.target_height,
        active: raw.mode_state & MODE_ACTIVE != 0,
        visible: raw.mode_state & MODE_VISIBLE != 0,
        powered: raw.mode_state & MODE_POWERED != 0,
    });
    validate_reason_shape(raw.high_water, mode, reason, removed)
        .map_err(|()| StoredValidationError::Reason)?;
    let state = CommittedModeState::restore(raw.high_water, mode, removed, policy)
        .map_err(StoredValidationError::Logic)?;
    Ok(RestoredSnapshot {
        stored: StoredSnapshot {
            high_water: raw.high_water,
            mode,
            reason,
            policy,
        },
        state,
    })
}

fn validate_publication(
    state: CommittedModeState,
    publication: CommittedModePublication,
    removed: bool,
) -> bool {
    state.high_water() == publication.generation()
        && state.current() == publication.mode()
        && state.is_removed() == removed
        && state.policy_snapshot() == publication.policy_snapshot()
        && validate_reason_shape(
            publication.generation(),
            publication.mode(),
            Some(publication.reason()),
            removed,
        )
        .is_ok()
        && CommittedModeState::restore(
            publication.generation(),
            publication.mode(),
            removed,
            publication.policy_snapshot(),
        )
        .is_ok()
}

fn validate_reason_shape(
    generation: u64,
    mode: Option<CommittedMode>,
    reason: Option<PublicationReason>,
    removed: bool,
) -> Result<(), ()> {
    match (generation, mode, reason, removed) {
        (0, None, None, false) => Ok(()),
        (
            1..,
            Some(_),
            Some(
                PublicationReason::Committed
                | PublicationReason::VisibilityTransition
                | PublicationReason::PowerTransition,
            ),
            false,
        ) => Ok(()),
        (
            1..,
            None,
            Some(
                PublicationReason::Committed
                | PublicationReason::VisibilityTransition
                | PublicationReason::PowerTransition
                | PublicationReason::Invalidated
                | PublicationReason::Reset,
            ),
            false,
        ) => Ok(()),
        (1.., None, Some(PublicationReason::Removed), true) => Ok(()),
        _ => Err(()),
    }
}

fn apply_transition(
    state: &mut CommittedModeState,
    generation: u64,
    transition: Transition,
) -> Result<CommittedModePublication, LogicRefusal> {
    match transition {
        Transition::PublishActive(facts) => state.publish_active(generation, facts),
        Transition::PublishEmpty { source_id } => state.publish_empty_commit(generation, source_id),
        Transition::Visibility { source_id, visible } => {
            state.transition_visibility(generation, source_id, visible)
        }
        Transition::Power { subject, powered } => {
            state.transition_power(generation, subject, powered)
        }
        Transition::Invalidate { source_id } => state.invalidate(generation, source_id),
        Transition::Reset => state.reset(generation),
        Transition::Remove => state.remove(generation),
    }
}

fn record_read_refusal(refusal: CommittedModeReadRefusal) -> CommittedModeReadRefusal {
    let counter = match refusal {
        CommittedModeReadRefusal::ResetClosed => &COMMITTED_MODE_READ_RESET_CLOSED,
        CommittedModeReadRefusal::Removed => &COMMITTED_MODE_READ_REMOVED,
        CommittedModeReadRefusal::Poisoned => &COMMITTED_MODE_READ_POISONED,
        CommittedModeReadRefusal::RetryExhausted => &COMMITTED_MODE_READ_RETRY_EXHAUSTED,
    };
    counter.fetch_add(1, Ordering::Relaxed);
    refusal
}

fn record_read_stored_validation_error(error: StoredValidationError) {
    match error {
        StoredValidationError::Logic(refusal) => {
            bump_logic_refusal(refusal);
            COMMITTED_MODE_READ_STORED_STATE_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        StoredValidationError::Reason => {
            COMMITTED_MODE_READ_STORED_REASON_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        StoredValidationError::Payload => {
            COMMITTED_MODE_READ_STORED_PAYLOAD_INVALID.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn record_write_refusal(refusal: CommittedModeWriteRefusal) -> CommittedModeWriteRefusal {
    match refusal {
        CommittedModeWriteRefusal::Claim(reason) => {
            COMMITTED_MODE_WRITE_CLAIM_REFUSALS[claim_refusal_index(reason)]
                .fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::Clear(issue) => {
            COMMITTED_MODE_WRITE_CLEAR_ISSUES[clear_issue_index(issue)]
                .fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::GenerationExhausted => {
            COMMITTED_MODE_WRITE_GENERATION_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::StoredStateInvalid(reason) => {
            bump_logic_refusal(reason);
            COMMITTED_MODE_WRITE_STORED_STATE_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::StoredReasonInvalid => {
            COMMITTED_MODE_WRITE_STORED_REASON_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::StoredPayloadInvalid => {
            COMMITTED_MODE_WRITE_STORED_PAYLOAD_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::TransitionRefused(reason) => {
            bump_logic_refusal(reason);
            COMMITTED_MODE_WRITE_TRANSITION_REFUSED.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::PublicationInvariantInvalid => {
            COMMITTED_MODE_WRITE_PUBLICATION_INVARIANT_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::LifecycleCorrupt => {
            COMMITTED_MODE_WRITE_LIFECYCLE_CORRUPT.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::InterruptedByReset => {
            COMMITTED_MODE_WRITE_INTERRUPTED_BY_RESET.fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeWriteRefusal::InterruptedByPoison => {
            COMMITTED_MODE_WRITE_INTERRUPTED_BY_POISON.fetch_add(1, Ordering::Relaxed);
        }
    }
    refusal
}

fn record_reset_close_refusal(
    refusal: CommittedModeResetCloseRefusal,
) -> CommittedModeResetCloseRefusal {
    match refusal {
        CommittedModeResetCloseRefusal::Lifecycle(reason) => {
            COMMITTED_MODE_RESET_CLOSE_REFUSALS[crash_close_refusal_index(reason)]
                .fetch_add(1, Ordering::Relaxed);
        }
        CommittedModeResetCloseRefusal::LifecycleCorrupt => {
            COMMITTED_MODE_RESET_CLOSE_LIFECYCLE_CORRUPT.fetch_add(1, Ordering::Relaxed);
        }
    }
    refusal
}

fn record_removed_terminal_error(error: &RemovedTerminalError) {
    match error {
        RemovedTerminalError::Lifecycle(reason) => {
            COMMITTED_MODE_REMOVED_TOMBSTONE_REFUSALS[removed_tombstone_refusal_index(*reason)]
                .fetch_add(1, Ordering::Relaxed);
        }
        RemovedTerminalError::StoredState(reason) => {
            bump_logic_refusal(*reason);
            COMMITTED_MODE_REMOVED_STORED_STATE_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        RemovedTerminalError::StoredReason => {
            COMMITTED_MODE_REMOVED_STORED_REASON_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        RemovedTerminalError::StoredPayload => {
            COMMITTED_MODE_REMOVED_STORED_PAYLOAD_INVALID.fetch_add(1, Ordering::Relaxed);
        }
        RemovedTerminalError::RetryExhausted => {
            COMMITTED_MODE_REMOVED_RETRY_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn bump_logic_refusal(refusal: LogicRefusal) {
    let index = match refusal {
        LogicRefusal::Removed => 0,
        LogicRefusal::GenerationZero => 1,
        LogicRefusal::GenerationReused { .. } => 2,
        LogicRefusal::GenerationWentBackward { .. } => 3,
        LogicRefusal::GenerationExhausted { .. } => 4,
        LogicRefusal::SourceUninitialized => 5,
        LogicRefusal::TargetUninitialized => 6,
        LogicRefusal::SourceIdentityMismatch { .. } => 7,
        LogicRefusal::TargetIdentityMismatch { .. } => 8,
        LogicRefusal::SourceExtentZero => 9,
        LogicRefusal::TargetExtentZero => 10,
        LogicRefusal::RemovedStateHasMode => 11,
        LogicRefusal::RemovedStateGenerationZero => 12,
        LogicRefusal::CurrentGenerationDoesNotMatchHighWater { .. } => 13,
        LogicRefusal::CurrentModeInactive => 14,
        LogicRefusal::PresentModeMissingPathPower => 15,
        LogicRefusal::AbsentModeHasPathPower => 16,
        LogicRefusal::CurrentModeVisibilityDoesNotMatchPolicy { .. } => 17,
        LogicRefusal::CurrentModePowerDoesNotMatchPolicy { .. } => 18,
    };
    COMMITTED_MODE_LOGIC_REFUSALS[index].fetch_add(1, Ordering::Relaxed);
}

const fn claim_refusal_index(refusal: ClaimRefusal) -> usize {
    match refusal {
        ClaimRefusal::WriterCollision => 0,
        ClaimRefusal::ResetClosed => 1,
        ClaimRefusal::ResetNotClosed => 2,
        ClaimRefusal::Removed => 3,
        ClaimRefusal::Poisoned => 4,
        ClaimRefusal::RevisionExhausted => 5,
        ClaimRefusal::LifecycleChanged => 6,
    }
}

const fn clear_issue_index(issue: ClearIssue) -> usize {
    match issue {
        ClearIssue::RevisionExhausted => 0,
        ClearIssue::ContentionExhausted => 1,
    }
}

const fn crash_close_refusal_index(refusal: CrashCloseRefusal) -> usize {
    match refusal {
        CrashCloseRefusal::Removed => 0,
        CrashCloseRefusal::Poisoned => 1,
        CrashCloseRefusal::RevisionExhausted => 2,
        CrashCloseRefusal::ContentionExhausted => 3,
    }
}

const fn removed_tombstone_refusal_index(refusal: RemovedTombstoneRefusal) -> usize {
    match refusal {
        RemovedTombstoneRefusal::NotRemoved => 0,
        RemovedTombstoneRefusal::Poisoned => 1,
        RemovedTombstoneRefusal::WriterHeld => 2,
        RemovedTombstoneRefusal::Present => 3,
        RemovedTombstoneRefusal::ResetClosed => 4,
        RemovedTombstoneRefusal::RevisionZero => 5,
    }
}

fn encode_mode_state(mode: CommittedMode) -> u32 {
    u32::from(mode.active) * MODE_ACTIVE
        | u32::from(mode.visible) * MODE_VISIBLE
        | u32::from(mode.powered) * MODE_POWERED
}

fn encode_policy_state(policy: ModePolicySnapshot) -> u32 {
    u32::from(policy.visible()) * POLICY_VISIBLE
        | u32::from(policy.adapter_powered()) * POLICY_ADAPTER_POWERED
        | u32::from(policy.target_powered()) * POLICY_TARGET_POWERED
        | u32::from(policy.path_powered().is_some()) * POLICY_PATH_KNOWN
        | u32::from(policy.path_powered() == Some(true)) * POLICY_PATH_POWERED
}

fn encode_reason(reason: Option<PublicationReason>) -> u32 {
    match reason {
        None => REASON_NONE,
        Some(PublicationReason::Committed) => REASON_COMMITTED,
        Some(PublicationReason::VisibilityTransition) => REASON_VISIBILITY,
        Some(PublicationReason::PowerTransition) => REASON_POWER,
        Some(PublicationReason::Invalidated) => REASON_INVALIDATED,
        Some(PublicationReason::Reset) => REASON_RESET,
        Some(PublicationReason::Removed) => REASON_REMOVED,
    }
}

fn decode_reason(raw: u32) -> Option<Option<PublicationReason>> {
    match raw {
        REASON_NONE => Some(None),
        REASON_COMMITTED => Some(Some(PublicationReason::Committed)),
        REASON_VISIBILITY => Some(Some(PublicationReason::VisibilityTransition)),
        REASON_POWER => Some(Some(PublicationReason::PowerTransition)),
        REASON_INVALIDATED => Some(Some(PublicationReason::Invalidated)),
        REASON_RESET => Some(Some(PublicationReason::Reset)),
        REASON_REMOVED => Some(Some(PublicationReason::Removed)),
        _ => None,
    }
}
