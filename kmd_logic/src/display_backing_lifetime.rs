//! Pure atomic lifetime transitions for one separately allocated display backing.
//! Terminal closes wrapper-derived retain; the remaining count still owns the backend.
//! KMD keeps these bare witnesses and plans private behind a pointer-bound wrapper.

const TERMINAL: u64 = 1 << 63;
const FINAL_QUEUED: u64 = 1 << 62;
const COUNT_MASK: u64 = FINAL_QUEUED - 1;

pub const MAX_REFERENCE_COUNT: u64 = COUNT_MASK;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackingWord(u64);

impl BackingWord {
    pub const INITIAL: Self = Self(1);

    pub const fn from_atomic(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn into_atomic(self) -> u64 {
        self.0
    }

    pub const fn count(self) -> u64 {
        self.0 & COUNT_MASK
    }

    pub const fn is_terminal(self) -> bool {
        self.0 & TERMINAL != 0
    }

    pub const fn is_final_queued(self) -> bool {
        self.0 & FINAL_QUEUED != 0
    }

    const fn with_count(self, count: u64) -> Self {
        Self((self.0 & !COUNT_MASK) | count)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackingState {
    Live { count: u64 },
    Terminal { count: u64 },
    FinalQueued,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptWord {
    FinalQueuedWithoutTerminal { count: u64 },
    FinalQueuedWithReferences { count: u64 },
    TerminalZeroWithoutFinalQueue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WordRefusal {
    ZeroOrReused,
    Corrupt(CorruptWord),
}

pub const fn validate_word(word: BackingWord) -> Result<BackingState, WordRefusal> {
    let count = word.count();
    match (word.is_terminal(), word.is_final_queued(), count) {
        (false, false, 0) => Err(WordRefusal::ZeroOrReused),
        (false, false, _) => Ok(BackingState::Live { count }),
        (false, true, _) => Err(WordRefusal::Corrupt(
            CorruptWord::FinalQueuedWithoutTerminal { count },
        )),
        (true, false, 0) => Err(WordRefusal::Corrupt(
            CorruptWord::TerminalZeroWithoutFinalQueue,
        )),
        (true, false, _) => Ok(BackingState::Terminal { count }),
        (true, true, 0) => Ok(BackingState::FinalQueued),
        (true, true, _) => Err(WordRefusal::Corrupt(
            CorruptWord::FinalQueuedWithReferences { count },
        )),
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct WrapperOwner {
    private: (),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct DisplayLease {
    private: (),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct QueueEmbeddedFinalizer {
    private: (),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ConsumeEmbeddedFinalizer {
    private: (),
}

pub const fn initialize() -> (BackingWord, WrapperOwner) {
    (BackingWord::INITIAL, WrapperOwner { private: () })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainRefusal {
    ZeroOrReused,
    Saturated,
    RetainAfterTerminal,
    Corrupt(CorruptWord),
    LifecycleChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefusedRetain {
    reason: RetainRefusal,
    lifecycle: BackingWord,
}

impl RefusedRetain {
    pub const fn reason(self) -> RetainRefusal {
        self.reason
    }

    pub const fn lifecycle(self) -> BackingWord {
        self.lifecycle
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RetainPlan {
    expected: BackingWord,
    desired: BackingWord,
}

impl RetainPlan {
    pub const fn expected_word(&self) -> BackingWord {
        self.expected
    }

    pub const fn desired_word(&self) -> BackingWord {
        self.desired
    }

    pub const fn cas_succeeded(self) -> DisplayLease {
        DisplayLease { private: () }
    }

    pub const fn cas_failed(self, found: BackingWord) -> RefusedRetain {
        RefusedRetain {
            reason: match retain_refusal(found) {
                Some(reason) => reason,
                None => RetainRefusal::LifecycleChanged,
            },
            lifecycle: found,
        }
    }
}

pub const fn retain(current: BackingWord) -> Result<RetainPlan, RefusedRetain> {
    if let Some(reason) = retain_refusal(current) {
        return Err(RefusedRetain {
            reason,
            lifecycle: current,
        });
    }
    Ok(RetainPlan {
        expected: current,
        desired: current.with_count(current.count() + 1),
    })
}

const fn retain_refusal(current: BackingWord) -> Option<RetainRefusal> {
    match validate_word(current) {
        Err(WordRefusal::ZeroOrReused) => Some(RetainRefusal::ZeroOrReused),
        Err(WordRefusal::Corrupt(reason)) => Some(RetainRefusal::Corrupt(reason)),
        Ok(BackingState::Terminal { .. } | BackingState::FinalQueued) => {
            Some(RetainRefusal::RetainAfterTerminal)
        }
        Ok(BackingState::Live { count }) if count == MAX_REFERENCE_COUNT => {
            Some(RetainRefusal::Saturated)
        }
        Ok(BackingState::Live { .. }) => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalizeRefusal {
    ZeroOrReused,
    DoubleTerminalize,
    Corrupt(CorruptWord),
    LifecycleChanged,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedTerminalize {
    reason: TerminalizeRefusal,
    lifecycle: BackingWord,
    owner: WrapperOwner,
}

impl RefusedTerminalize {
    pub const fn reason(&self) -> TerminalizeRefusal {
        self.reason
    }

    pub const fn lifecycle(&self) -> BackingWord {
        self.lifecycle
    }

    pub fn into_owner(self) -> WrapperOwner {
        self.owner
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct TerminalizePlan {
    expected: BackingWord,
    desired: BackingWord,
    owner: WrapperOwner,
    queues_finalizer: bool,
}

impl TerminalizePlan {
    pub const fn expected_word(&self) -> BackingWord {
        self.expected
    }

    pub const fn desired_word(&self) -> BackingWord {
        self.desired
    }

    pub fn cas_succeeded(self) -> ReferenceDrop {
        reference_drop(self.queues_finalizer)
    }

    pub fn cas_failed(self, found: BackingWord) -> RefusedTerminalize {
        RefusedTerminalize {
            reason: match terminalize_refusal(found) {
                Some(reason) => reason,
                None => TerminalizeRefusal::LifecycleChanged,
            },
            lifecycle: found,
            owner: self.owner,
        }
    }
}

pub const fn terminalize(
    current: BackingWord,
    owner: WrapperOwner,
) -> Result<TerminalizePlan, RefusedTerminalize> {
    if let Some(reason) = terminalize_refusal(current) {
        return Err(RefusedTerminalize {
            reason,
            lifecycle: current,
            owner,
        });
    }

    let remaining = current.count() - 1;
    let queues_finalizer = remaining == 0;
    let mut desired = current.with_count(remaining);
    desired.0 |= TERMINAL;
    if queues_finalizer {
        desired.0 |= FINAL_QUEUED;
    }
    Ok(TerminalizePlan {
        expected: current,
        desired,
        owner,
        queues_finalizer,
    })
}

const fn terminalize_refusal(current: BackingWord) -> Option<TerminalizeRefusal> {
    match validate_word(current) {
        Err(WordRefusal::ZeroOrReused) => Some(TerminalizeRefusal::ZeroOrReused),
        Err(WordRefusal::Corrupt(reason)) => Some(TerminalizeRefusal::Corrupt(reason)),
        Ok(BackingState::Terminal { .. } | BackingState::FinalQueued) => {
            Some(TerminalizeRefusal::DoubleTerminalize)
        }
        Ok(BackingState::Live { .. }) => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseRefusal {
    ZeroOrReused,
    ReleaseUnderflow,
    FinalizerAlreadyQueued,
    Corrupt(CorruptWord),
    LifecycleChanged,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedRelease {
    reason: ReleaseRefusal,
    lifecycle: BackingWord,
    lease: DisplayLease,
}

impl RefusedRelease {
    pub const fn reason(&self) -> ReleaseRefusal {
        self.reason
    }

    pub const fn lifecycle(&self) -> BackingWord {
        self.lifecycle
    }

    pub fn into_lease(self) -> DisplayLease {
        self.lease
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasePlan {
    expected: BackingWord,
    desired: BackingWord,
    lease: DisplayLease,
    queues_finalizer: bool,
}

impl ReleasePlan {
    pub const fn expected_word(&self) -> BackingWord {
        self.expected
    }

    pub const fn desired_word(&self) -> BackingWord {
        self.desired
    }

    pub fn cas_succeeded(self) -> ReferenceDrop {
        reference_drop(self.queues_finalizer)
    }

    pub fn cas_failed(self, found: BackingWord) -> RefusedRelease {
        RefusedRelease {
            reason: match release_refusal(found) {
                Some(reason) => reason,
                None => ReleaseRefusal::LifecycleChanged,
            },
            lifecycle: found,
            lease: self.lease,
        }
    }
}

pub const fn release(
    current: BackingWord,
    lease: DisplayLease,
) -> Result<ReleasePlan, RefusedRelease> {
    if let Some(reason) = release_refusal(current) {
        return Err(RefusedRelease {
            reason,
            lifecycle: current,
            lease,
        });
    }

    let remaining = current.count() - 1;
    let queues_finalizer = current.is_terminal() && remaining == 0;
    let mut desired = current.with_count(remaining);
    if queues_finalizer {
        desired.0 |= FINAL_QUEUED;
    }
    Ok(ReleasePlan {
        expected: current,
        desired,
        lease,
        queues_finalizer,
    })
}

const fn release_refusal(current: BackingWord) -> Option<ReleaseRefusal> {
    match validate_word(current) {
        Err(WordRefusal::ZeroOrReused) => Some(ReleaseRefusal::ZeroOrReused),
        Err(WordRefusal::Corrupt(reason)) => Some(ReleaseRefusal::Corrupt(reason)),
        Ok(BackingState::FinalQueued) => Some(ReleaseRefusal::FinalizerAlreadyQueued),
        Ok(BackingState::Live { count }) if count == 1 => Some(ReleaseRefusal::ReleaseUnderflow),
        Ok(BackingState::Live { .. } | BackingState::Terminal { .. }) => None,
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub enum ReferenceDrop {
    ReferencesRemain,
    QueueEmbeddedFinalizer(QueueEmbeddedFinalizer),
}

fn reference_drop(queues_finalizer: bool) -> ReferenceDrop {
    if queues_finalizer {
        ReferenceDrop::QueueEmbeddedFinalizer(QueueEmbeddedFinalizer { private: () })
    } else {
        ReferenceDrop::ReferencesRemain
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizerRefusal {
    ZeroOrReused,
    NotTerminal,
    FinalizerNotQueued,
    Corrupt(CorruptWord),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedFinalizer {
    reason: FinalizerRefusal,
    lifecycle: BackingWord,
    queue: QueueEmbeddedFinalizer,
}

impl RefusedFinalizer {
    pub const fn reason(&self) -> FinalizerRefusal {
        self.reason
    }

    pub const fn lifecycle(&self) -> BackingWord {
        self.lifecycle
    }

    pub fn into_queue(self) -> QueueEmbeddedFinalizer {
        self.queue
    }
}

pub fn validate_finalizer_consumption(
    current: BackingWord,
    queue: QueueEmbeddedFinalizer,
) -> Result<ConsumeEmbeddedFinalizer, RefusedFinalizer> {
    let reason = match validate_word(current) {
        Ok(BackingState::FinalQueued) => {
            return Ok(ConsumeEmbeddedFinalizer { private: () });
        }
        Ok(BackingState::Live { .. }) => FinalizerRefusal::NotTerminal,
        Ok(BackingState::Terminal { .. }) => FinalizerRefusal::FinalizerNotQueued,
        Err(WordRefusal::ZeroOrReused) => FinalizerRefusal::ZeroOrReused,
        Err(WordRefusal::Corrupt(reason)) => FinalizerRefusal::Corrupt(reason),
    };
    Err(RefusedFinalizer {
        reason,
        lifecycle: current,
        queue,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn word(terminal: bool, final_queued: bool, count: u64) -> BackingWord {
        BackingWord(
            count
                | if terminal { TERMINAL } else { 0 }
                | if final_queued { FINAL_QUEUED } else { 0 },
        )
    }

    const fn owner() -> WrapperOwner {
        WrapperOwner { private: () }
    }

    const fn lease() -> DisplayLease {
        DisplayLease { private: () }
    }

    const fn queued() -> QueueEmbeddedFinalizer {
        QueueEmbeddedFinalizer { private: () }
    }

    fn apply_retain(current: &mut BackingWord, plan: RetainPlan) -> DisplayLease {
        assert_eq!(*current, plan.expected_word());
        *current = plan.desired_word();
        plan.cas_succeeded()
    }

    fn apply_terminalize(current: &mut BackingWord, plan: TerminalizePlan) -> ReferenceDrop {
        assert_eq!(*current, plan.expected_word());
        *current = plan.desired_word();
        plan.cas_succeeded()
    }

    fn apply_release(current: &mut BackingWord, plan: ReleasePlan) -> ReferenceDrop {
        assert_eq!(*current, plan.expected_word());
        *current = plan.desired_word();
        plan.cas_succeeded()
    }

    fn take_queue(action: ReferenceDrop) -> QueueEmbeddedFinalizer {
        match action {
            ReferenceDrop::QueueEmbeddedFinalizer(queue) => queue,
            ReferenceDrop::ReferencesRemain => panic!("expected finalizer queue"),
        }
    }

    #[test]
    fn raw_layout_and_initial_owner_are_exact() {
        let (initial, _owner) = initialize();
        assert_eq!(initial, BackingWord::INITIAL);
        assert_eq!(initial.into_atomic(), 1);
        assert_eq!(BackingWord::from_atomic(TERMINAL).count(), 0);
        assert!(BackingWord::from_atomic(TERMINAL).is_terminal());
        assert!(BackingWord::from_atomic(FINAL_QUEUED).is_final_queued());
        assert_eq!(MAX_REFERENCE_COUNT, (1 << 62) - 1);
    }

    #[test]
    fn every_flag_shape_and_count_boundary_has_one_validation_result() {
        let counts = [0, 1, 2, MAX_REFERENCE_COUNT - 1, MAX_REFERENCE_COUNT];
        for terminal in [false, true] {
            for final_queued in [false, true] {
                for count in counts {
                    let current = word(terminal, final_queued, count);
                    let expected = match (terminal, final_queued, count) {
                        (false, false, 0) => Err(WordRefusal::ZeroOrReused),
                        (false, false, _) => Ok(BackingState::Live { count }),
                        (false, true, _) => Err(WordRefusal::Corrupt(
                            CorruptWord::FinalQueuedWithoutTerminal { count },
                        )),
                        (true, false, 0) => Err(WordRefusal::Corrupt(
                            CorruptWord::TerminalZeroWithoutFinalQueue,
                        )),
                        (true, false, _) => Ok(BackingState::Terminal { count }),
                        (true, true, 0) => Ok(BackingState::FinalQueued),
                        (true, true, _) => Err(WordRefusal::Corrupt(
                            CorruptWord::FinalQueuedWithReferences { count },
                        )),
                    };
                    assert_eq!(validate_word(current), expected, "raw={:#x}", current.0);
                }
            }
        }
    }

    #[test]
    fn retain_and_terminalize_cas_interleavings_are_closed() {
        let (mut current, wrapper) = initialize();
        let retain_plan = retain(current).unwrap();
        let terminal_plan = terminalize(current, wrapper).unwrap();

        let display = apply_retain(&mut current, retain_plan);
        let refusal = terminal_plan.cas_failed(current);
        assert_eq!(refusal.reason(), TerminalizeRefusal::LifecycleChanged);
        let terminal_plan = terminalize(current, refusal.into_owner()).unwrap();
        assert_eq!(
            apply_terminalize(&mut current, terminal_plan),
            ReferenceDrop::ReferencesRemain
        );
        assert_eq!(
            validate_word(current),
            Ok(BackingState::Terminal { count: 1 })
        );
        let release_plan = release(current, display).unwrap();
        let queue = take_queue(apply_release(&mut current, release_plan));
        let _ = validate_finalizer_consumption(current, queue).unwrap();

        let (mut current, wrapper) = initialize();
        let retain_plan = retain(current).unwrap();
        let terminal_plan = terminalize(current, wrapper).unwrap();
        let queue = take_queue(apply_terminalize(&mut current, terminal_plan));
        let refusal = retain_plan.cas_failed(current);
        assert_eq!(refusal.reason(), RetainRefusal::RetainAfterTerminal);
        assert_eq!(refusal.lifecycle(), current);
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }

    #[test]
    fn release_and_terminalize_cas_interleavings_preserve_the_last_drop() {
        let (mut current, wrapper) = initialize();
        let plan = retain(current).unwrap();
        let display = apply_retain(&mut current, plan);
        let release_plan = release(current, display).unwrap();
        let terminal_plan = terminalize(current, wrapper).unwrap();

        assert_eq!(
            apply_release(&mut current, release_plan),
            ReferenceDrop::ReferencesRemain
        );
        let refusal = terminal_plan.cas_failed(current);
        assert_eq!(refusal.reason(), TerminalizeRefusal::LifecycleChanged);
        let plan = terminalize(current, refusal.into_owner()).unwrap();
        let queue = take_queue(apply_terminalize(&mut current, plan));
        let _ = validate_finalizer_consumption(current, queue).unwrap();

        let (mut current, wrapper) = initialize();
        let plan = retain(current).unwrap();
        let display = apply_retain(&mut current, plan);
        let release_plan = release(current, display).unwrap();
        let terminal_plan = terminalize(current, wrapper).unwrap();

        assert_eq!(
            apply_terminalize(&mut current, terminal_plan),
            ReferenceDrop::ReferencesRemain
        );
        let refusal = release_plan.cas_failed(current);
        assert_eq!(refusal.reason(), ReleaseRefusal::LifecycleChanged);
        let plan = release(current, refusal.into_lease()).unwrap();
        let queue = take_queue(apply_release(&mut current, plan));
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }

    #[test]
    fn retain_and_release_cas_interleavings_conserve_the_live_lease() {
        let (mut current, wrapper) = initialize();
        let plan = retain(current).unwrap();
        let old_display = apply_retain(&mut current, plan);
        let retain_plan = retain(current).unwrap();
        let release_plan = release(current, old_display).unwrap();

        let new_display = apply_retain(&mut current, retain_plan);
        let refused = release_plan.cas_failed(current);
        assert_eq!(refused.reason(), ReleaseRefusal::LifecycleChanged);
        let retry = release(current, refused.into_lease()).unwrap();
        assert_eq!(
            apply_release(&mut current, retry),
            ReferenceDrop::ReferencesRemain
        );
        let plan = terminalize(current, wrapper).unwrap();
        assert_eq!(
            apply_terminalize(&mut current, plan),
            ReferenceDrop::ReferencesRemain
        );
        let plan = release(current, new_display).unwrap();
        let queue = take_queue(apply_release(&mut current, plan));
        let _ = validate_finalizer_consumption(current, queue).unwrap();

        let (mut current, wrapper) = initialize();
        let plan = retain(current).unwrap();
        let old_display = apply_retain(&mut current, plan);
        let retain_plan = retain(current).unwrap();
        let release_plan = release(current, old_display).unwrap();

        assert_eq!(
            apply_release(&mut current, release_plan),
            ReferenceDrop::ReferencesRemain
        );
        let refused = retain_plan.cas_failed(current);
        assert_eq!(refused.reason(), RetainRefusal::LifecycleChanged);
        let retry = retain(current).unwrap();
        let new_display = apply_retain(&mut current, retry);
        let plan = terminalize(current, wrapper).unwrap();
        assert_eq!(
            apply_terminalize(&mut current, plan),
            ReferenceDrop::ReferencesRemain
        );
        let plan = release(current, new_display).unwrap();
        let queue = take_queue(apply_release(&mut current, plan));
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }

    #[test]
    fn multiple_display_references_release_once_each_and_only_last_queues() {
        let (mut current, wrapper) = initialize();
        let plan = retain(current).unwrap();
        let first = apply_retain(&mut current, plan);
        let plan = retain(current).unwrap();
        let second = apply_retain(&mut current, plan);
        let plan = retain(current).unwrap();
        let third = apply_retain(&mut current, plan);
        assert_eq!(current.count(), 4);

        let plan = release(current, first).unwrap();
        assert_eq!(
            apply_release(&mut current, plan),
            ReferenceDrop::ReferencesRemain
        );
        assert_eq!(current.count(), 3);
        let plan = terminalize(current, wrapper).unwrap();
        assert_eq!(
            apply_terminalize(&mut current, plan),
            ReferenceDrop::ReferencesRemain
        );
        assert_eq!(
            validate_word(current),
            Ok(BackingState::Terminal { count: 2 })
        );
        let plan = release(current, second).unwrap();
        assert_eq!(
            apply_release(&mut current, plan),
            ReferenceDrop::ReferencesRemain
        );
        let plan = release(current, third).unwrap();
        let queue = take_queue(apply_release(&mut current, plan));
        assert_eq!(validate_word(current), Ok(BackingState::FinalQueued));
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }

    #[test]
    fn terminalize_without_display_references_is_the_unique_last_drop() {
        let (mut current, wrapper) = initialize();
        let plan = terminalize(current, wrapper).unwrap();
        assert_eq!(plan.expected_word(), word(false, false, 1));
        assert_eq!(plan.desired_word(), word(true, true, 0));
        let queue = take_queue(apply_terminalize(&mut current, plan));
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }

    #[test]
    fn two_last_release_plans_have_only_one_cas_winner() {
        let mut current = word(true, false, 1);
        let first = release(current, lease()).unwrap();
        let second = release(current, lease()).unwrap();

        let queue = take_queue(apply_release(&mut current, first));
        let refusal = second.cas_failed(current);
        assert_eq!(refusal.reason(), ReleaseRefusal::FinalizerAlreadyQueued);
        assert_eq!(refusal.lifecycle(), word(true, true, 0));
        let _ = refusal.into_lease();
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }

    #[test]
    fn independent_release_racers_retry_the_loser_and_queue_exactly_once() {
        for first_wins in [true, false] {
            let (mut current, wrapper) = initialize();
            let plan = retain(current).unwrap();
            let first = apply_retain(&mut current, plan);
            let plan = retain(current).unwrap();
            let second = apply_retain(&mut current, plan);
            let plan = terminalize(current, wrapper).unwrap();
            assert_eq!(
                apply_terminalize(&mut current, plan),
                ReferenceDrop::ReferencesRemain
            );
            assert_eq!(
                validate_word(current),
                Ok(BackingState::Terminal { count: 2 })
            );

            let first_plan = release(current, first).unwrap();
            let second_plan = release(current, second).unwrap();
            let (winner, loser) = if first_wins {
                (first_plan, second_plan)
            } else {
                (second_plan, first_plan)
            };
            assert_eq!(
                apply_release(&mut current, winner),
                ReferenceDrop::ReferencesRemain
            );
            let refused = loser.cas_failed(current);
            assert_eq!(refused.reason(), ReleaseRefusal::LifecycleChanged);
            assert_eq!(
                validate_word(refused.lifecycle()),
                Ok(BackingState::Terminal { count: 1 })
            );
            let retry = release(current, refused.into_lease()).unwrap();
            let queue = take_queue(apply_release(&mut current, retry));
            assert_eq!(validate_word(current), Ok(BackingState::FinalQueued));
            let _ = validate_finalizer_consumption(current, queue).unwrap();
        }
    }

    #[test]
    fn saturation_and_count_boundaries_never_wrap() {
        let almost_full = word(false, false, MAX_REFERENCE_COUNT - 1);
        let plan = retain(almost_full).unwrap();
        assert_eq!(plan.desired_word(), word(false, false, MAX_REFERENCE_COUNT));
        let full_lease = plan.cas_succeeded();
        assert_eq!(
            retain(word(false, false, MAX_REFERENCE_COUNT))
                .unwrap_err()
                .reason(),
            RetainRefusal::Saturated
        );

        let mut full = word(false, false, MAX_REFERENCE_COUNT);
        let plan = terminalize(full, owner()).unwrap();
        let action = apply_terminalize(&mut full, plan);
        assert_eq!(action, ReferenceDrop::ReferencesRemain);
        assert_eq!(full, word(true, false, MAX_REFERENCE_COUNT - 1));
        let plan = release(full, full_lease).unwrap();
        assert_eq!(
            apply_release(&mut full, plan),
            ReferenceDrop::ReferencesRemain
        );
        assert_eq!(full.count(), MAX_REFERENCE_COUNT - 2);
    }

    #[test]
    fn zero_reuse_underflow_double_terminal_and_double_queue_are_distinct() {
        assert_eq!(
            retain(BackingWord::from_atomic(0)).unwrap_err().reason(),
            RetainRefusal::ZeroOrReused
        );
        assert_eq!(
            terminalize(BackingWord::from_atomic(0), owner())
                .unwrap_err()
                .reason(),
            TerminalizeRefusal::ZeroOrReused
        );
        assert_eq!(
            release(BackingWord::from_atomic(0), lease())
                .unwrap_err()
                .reason(),
            ReleaseRefusal::ZeroOrReused
        );
        assert_eq!(
            release(word(false, false, 1), lease())
                .unwrap_err()
                .reason(),
            ReleaseRefusal::ReleaseUnderflow
        );
        assert_eq!(
            terminalize(word(true, false, 1), owner())
                .unwrap_err()
                .reason(),
            TerminalizeRefusal::DoubleTerminalize
        );
        assert_eq!(
            release(word(true, true, 0), lease()).unwrap_err().reason(),
            ReleaseRefusal::FinalizerAlreadyQueued
        );
    }

    #[test]
    fn every_corrupt_word_is_refused_by_every_operation() {
        fn assert_all(current: BackingWord, reason: CorruptWord) {
            assert_eq!(
                retain(current).unwrap_err().reason(),
                RetainRefusal::Corrupt(reason)
            );
            assert_eq!(
                terminalize(current, owner()).unwrap_err().reason(),
                TerminalizeRefusal::Corrupt(reason)
            );
            assert_eq!(
                release(current, lease()).unwrap_err().reason(),
                ReleaseRefusal::Corrupt(reason)
            );
            assert_eq!(
                validate_finalizer_consumption(current, queued())
                    .unwrap_err()
                    .reason(),
                FinalizerRefusal::Corrupt(reason)
            );
        }

        let counts = [0, 1, 2, MAX_REFERENCE_COUNT - 1, MAX_REFERENCE_COUNT];
        for count in counts {
            assert_all(
                word(false, true, count),
                CorruptWord::FinalQueuedWithoutTerminal { count },
            );
            if count != 0 {
                assert_all(
                    word(true, true, count),
                    CorruptWord::FinalQueuedWithReferences { count },
                );
            }
        }
        assert_all(
            word(true, false, 0),
            CorruptWord::TerminalZeroWithoutFinalQueue,
        );
    }

    #[test]
    fn finalizer_consumption_requires_the_exact_terminal_queued_zero_word() {
        for (current, reason) in [
            (BackingWord::from_atomic(0), FinalizerRefusal::ZeroOrReused),
            (word(false, false, 1), FinalizerRefusal::NotTerminal),
            (word(true, false, 1), FinalizerRefusal::FinalizerNotQueued),
        ] {
            let refused = validate_finalizer_consumption(current, queued()).unwrap_err();
            assert_eq!(refused.reason(), reason);
            assert_eq!(refused.lifecycle(), current);
            let _ = refused.into_queue();
        }
        let _ = validate_finalizer_consumption(word(true, true, 0), queued()).unwrap();
    }

    #[test]
    fn stale_plans_return_move_only_ownership_for_retry() {
        let (mut current, wrapper) = initialize();
        let terminal_plan = terminalize(current, wrapper).unwrap();
        let plan = retain(current).unwrap();
        let display = apply_retain(&mut current, plan);
        let refused = terminal_plan.cas_failed(current);
        let wrapper = refused.into_owner();
        let plan = terminalize(current, wrapper).unwrap();
        let _ = apply_terminalize(&mut current, plan);

        let stale_release = release(current, display).unwrap();
        let extra = lease();
        let winning_release = release(current, extra).unwrap();
        current = winning_release.desired_word();
        let queue = take_queue(winning_release.cas_succeeded());
        let refused = stale_release.cas_failed(current);
        assert_eq!(refused.reason(), ReleaseRefusal::FinalizerAlreadyQueued);
        let _ = refused.into_lease();
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }

    #[test]
    fn terminal_is_not_final_while_a_backend_display_lease_remains() {
        let (mut current, wrapper) = initialize();
        let plan = retain(current).unwrap();
        let display = apply_retain(&mut current, plan);
        let plan = terminalize(current, wrapper).unwrap();
        assert_eq!(
            apply_terminalize(&mut current, plan),
            ReferenceDrop::ReferencesRemain
        );
        assert_eq!(
            validate_word(current),
            Ok(BackingState::Terminal { count: 1 })
        );
        assert_eq!(
            retain(current).unwrap_err().reason(),
            RetainRefusal::RetainAfterTerminal
        );
        assert_eq!(
            validate_finalizer_consumption(current, queued())
                .unwrap_err()
                .reason(),
            FinalizerRefusal::FinalizerNotQueued
        );
        let plan = release(current, display).unwrap();
        let queue = take_queue(apply_release(&mut current, plan));
        let _ = validate_finalizer_consumption(current, queue).unwrap();
    }
}
