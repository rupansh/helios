//! Pure committed-mode publication model. Every accepted transition consumes a
//! strictly newer generation and returns a complete by-value record for a
//! platform-owned atomic publisher; this state owns no display object.

use crate::direct_scanout_admission::CommittedMode;
use helios_protocol::D3DDDI_ID_UNINITIALIZED;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModeCommitFacts {
    pub source_id: u32,
    pub target_id: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub target_width: u32,
    pub target_height: u32,
    pub path_powered: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerSubject {
    Adapter,
    Target { target_id: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationReason {
    Committed,
    VisibilityTransition,
    PowerTransition,
    Invalidated,
    Reset,
    Removed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModePolicySnapshot {
    source_id: u32,
    target_id: u32,
    visible: bool,
    adapter_powered: bool,
    target_powered: bool,
    path_powered: Option<bool>,
}

impl ModePolicySnapshot {
    pub fn from_stored(
        source_id: u32,
        target_id: u32,
        visible: bool,
        adapter_powered: bool,
        target_powered: bool,
        path_powered: Option<bool>,
    ) -> Result<Self, Refusal> {
        validate_bound_identity(source_id, target_id)?;
        Ok(Self {
            source_id,
            target_id,
            visible,
            adapter_powered,
            target_powered,
            path_powered,
        })
    }

    pub const fn source_id(self) -> u32 {
        self.source_id
    }

    pub const fn target_id(self) -> u32 {
        self.target_id
    }

    pub const fn visible(self) -> bool {
        self.visible
    }

    pub const fn adapter_powered(self) -> bool {
        self.adapter_powered
    }

    pub const fn target_powered(self) -> bool {
        self.target_powered
    }

    pub const fn path_powered(self) -> Option<bool> {
        self.path_powered
    }

    const fn effective_powered(self) -> Option<bool> {
        match self.path_powered {
            Some(path_powered) => Some(path_powered && self.adapter_powered && self.target_powered),
            None => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedModePublication {
    generation: u64,
    mode: Option<CommittedMode>,
    reason: PublicationReason,
    policy: ModePolicySnapshot,
}

impl CommittedModePublication {
    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub const fn mode(self) -> Option<CommittedMode> {
        self.mode
    }

    pub const fn reason(self) -> PublicationReason {
        self.reason
    }

    pub const fn policy_snapshot(self) -> ModePolicySnapshot {
        self.policy
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Removed,
    GenerationZero,
    GenerationReused { value: u64 },
    GenerationWentBackward { high_water: u64, found: u64 },
    GenerationExhausted { high_water: u64 },
    SourceUninitialized,
    TargetUninitialized,
    SourceIdentityMismatch { expected: u32, found: u32 },
    TargetIdentityMismatch { expected: u32, found: u32 },
    SourceExtentZero,
    TargetExtentZero,
    RemovedStateHasMode,
    RemovedStateGenerationZero,
    CurrentGenerationDoesNotMatchHighWater { high_water: u64, current: u64 },
    CurrentModeInactive,
    PresentModeMissingPathPower,
    AbsentModeHasPathPower,
    CurrentModeVisibilityDoesNotMatchPolicy { expected: bool, found: bool },
    CurrentModePowerDoesNotMatchPolicy { expected: bool, found: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedModeState {
    high_water: u64,
    current: Option<CommittedMode>,
    removed: bool,
    policy: ModePolicySnapshot,
}

impl CommittedModeState {
    pub fn new(source_id: u32, target_id: u32) -> Result<Self, Refusal> {
        Ok(Self {
            high_water: 0,
            current: None,
            removed: false,
            policy: ModePolicySnapshot::from_stored(
                source_id, target_id, false, false, false, None,
            )?,
        })
    }

    pub const fn high_water(&self) -> u64 {
        self.high_water
    }

    pub const fn current(&self) -> Option<CommittedMode> {
        self.current
    }

    pub const fn is_removed(&self) -> bool {
        self.removed
    }

    pub const fn policy_snapshot(&self) -> ModePolicySnapshot {
        self.policy
    }

    pub fn restore(
        high_water: u64,
        current: Option<CommittedMode>,
        removed: bool,
        policy: ModePolicySnapshot,
    ) -> Result<Self, Refusal> {
        validate_bound_identity(policy.source_id, policy.target_id)?;
        if removed && current.is_some() {
            return Err(Refusal::RemovedStateHasMode);
        }
        if removed && high_water == 0 {
            return Err(Refusal::RemovedStateGenerationZero);
        }
        match current {
            Some(mode) => validate_restored_mode(high_water, mode, policy)?,
            None if policy.path_powered.is_some() => {
                return Err(Refusal::AbsentModeHasPathPower);
            }
            None => {}
        }
        Ok(Self {
            high_water,
            current,
            removed,
            policy,
        })
    }

    pub fn next_generation(&self) -> Result<u64, Refusal> {
        if self.removed {
            return Err(Refusal::Removed);
        }
        self.high_water
            .checked_add(1)
            .ok_or(Refusal::GenerationExhausted {
                high_water: self.high_water,
            })
    }

    pub fn publish_active(
        &mut self,
        generation: u64,
        facts: ModeCommitFacts,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        validate_commit_facts(facts, self.policy)?;
        self.policy.path_powered = Some(facts.path_powered);
        let mode = CommittedMode {
            generation,
            source_id: facts.source_id,
            target_id: facts.target_id,
            source_width: facts.source_width,
            source_height: facts.source_height,
            target_width: facts.target_width,
            target_height: facts.target_height,
            active: true,
            visible: self.policy.visible,
            powered: self.policy.effective_powered().unwrap_or(false),
        };
        Ok(self.commit(generation, Some(mode), PublicationReason::Committed))
    }

    pub fn publish_empty_commit(
        &mut self,
        generation: u64,
        source_id: u32,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        validate_source_identity(source_id, self.policy.source_id)?;
        Ok(self.commit_absent(generation, PublicationReason::Committed))
    }

    pub fn transition_visibility(
        &mut self,
        generation: u64,
        source_id: u32,
        visible: bool,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        validate_source_identity(source_id, self.policy.source_id)?;
        self.policy.visible = visible;
        let mode = self.current.map(|mut mode| {
            mode.generation = generation;
            mode.visible = visible;
            mode
        });
        Ok(self.commit(generation, mode, PublicationReason::VisibilityTransition))
    }

    pub fn transition_power(
        &mut self,
        generation: u64,
        subject: PowerSubject,
        powered: bool,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        match subject {
            PowerSubject::Adapter => self.policy.adapter_powered = powered,
            PowerSubject::Target { target_id } => {
                validate_target_identity(target_id, self.policy.target_id)?;
                self.policy.target_powered = powered;
            }
        }
        let effective_powered = self.policy.effective_powered();
        let mode = self.current.map(|mut mode| {
            mode.generation = generation;
            mode.powered = effective_powered.unwrap_or(false);
            mode
        });
        Ok(self.commit(generation, mode, PublicationReason::PowerTransition))
    }

    pub fn invalidate(
        &mut self,
        generation: u64,
        source_id: u32,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        validate_source_identity(source_id, self.policy.source_id)?;
        Ok(self.commit_absent(generation, PublicationReason::Invalidated))
    }

    pub fn reset(&mut self, generation: u64) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        Ok(self.commit_absent(generation, PublicationReason::Reset))
    }

    pub fn remove(&mut self, generation: u64) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        let publication = self.commit_absent(generation, PublicationReason::Removed);
        self.removed = true;
        Ok(publication)
    }

    fn validate_generation(&self, found: u64) -> Result<(), Refusal> {
        if self.removed {
            return Err(Refusal::Removed);
        }
        if self.high_water == u64::MAX {
            return Err(Refusal::GenerationExhausted {
                high_water: self.high_water,
            });
        }
        if found == 0 {
            return Err(Refusal::GenerationZero);
        }
        if found == self.high_water {
            return Err(Refusal::GenerationReused { value: found });
        }
        if found < self.high_water {
            return Err(Refusal::GenerationWentBackward {
                high_water: self.high_water,
                found,
            });
        }
        Ok(())
    }

    fn commit_absent(
        &mut self,
        generation: u64,
        reason: PublicationReason,
    ) -> CommittedModePublication {
        self.policy.path_powered = None;
        self.commit(generation, None, reason)
    }

    fn commit(
        &mut self,
        generation: u64,
        mode: Option<CommittedMode>,
        reason: PublicationReason,
    ) -> CommittedModePublication {
        self.high_water = generation;
        self.current = mode;
        CommittedModePublication {
            generation,
            mode,
            reason,
            policy: self.policy,
        }
    }
}

fn validate_bound_identity(source_id: u32, target_id: u32) -> Result<(), Refusal> {
    if source_id == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::SourceUninitialized);
    }
    if target_id == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::TargetUninitialized);
    }
    Ok(())
}

fn validate_source_identity(found: u32, expected: u32) -> Result<(), Refusal> {
    if found == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::SourceUninitialized);
    }
    if found != expected {
        return Err(Refusal::SourceIdentityMismatch { expected, found });
    }
    Ok(())
}

fn validate_target_identity(found: u32, expected: u32) -> Result<(), Refusal> {
    if found == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::TargetUninitialized);
    }
    if found != expected {
        return Err(Refusal::TargetIdentityMismatch { expected, found });
    }
    Ok(())
}

fn validate_commit_facts(
    facts: ModeCommitFacts,
    policy: ModePolicySnapshot,
) -> Result<(), Refusal> {
    validate_source_identity(facts.source_id, policy.source_id)?;
    validate_target_identity(facts.target_id, policy.target_id)?;
    validate_extents(
        facts.source_width,
        facts.source_height,
        facts.target_width,
        facts.target_height,
    )
}

fn validate_extents(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<(), Refusal> {
    if source_width == 0 || source_height == 0 {
        return Err(Refusal::SourceExtentZero);
    }
    if target_width == 0 || target_height == 0 {
        return Err(Refusal::TargetExtentZero);
    }
    Ok(())
}

fn validate_restored_mode(
    high_water: u64,
    mode: CommittedMode,
    policy: ModePolicySnapshot,
) -> Result<(), Refusal> {
    if mode.generation == 0 {
        return Err(Refusal::GenerationZero);
    }
    if mode.generation != high_water {
        return Err(Refusal::CurrentGenerationDoesNotMatchHighWater {
            high_water,
            current: mode.generation,
        });
    }
    if !mode.active {
        return Err(Refusal::CurrentModeInactive);
    }
    validate_source_identity(mode.source_id, policy.source_id)?;
    validate_target_identity(mode.target_id, policy.target_id)?;
    validate_extents(
        mode.source_width,
        mode.source_height,
        mode.target_width,
        mode.target_height,
    )?;
    if mode.visible != policy.visible {
        return Err(Refusal::CurrentModeVisibilityDoesNotMatchPolicy {
            expected: policy.visible,
            found: mode.visible,
        });
    }
    let Some(expected_powered) = policy.effective_powered() else {
        return Err(Refusal::PresentModeMissingPathPower);
    };
    if mode.powered != expected_powered {
        return Err(Refusal::CurrentModePowerDoesNotMatchPolicy {
            expected: expected_powered,
            found: mode.powered,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_ID: u32 = 3;
    const TARGET_ID: u32 = 7;

    fn state() -> CommittedModeState {
        CommittedModeState::new(SOURCE_ID, TARGET_ID).unwrap()
    }

    fn facts(path_powered: bool) -> ModeCommitFacts {
        ModeCommitFacts {
            source_id: SOURCE_ID,
            target_id: TARGET_ID,
            source_width: 1920,
            source_height: 1080,
            target_width: 1920,
            target_height: 1080,
            path_powered,
        }
    }

    #[test]
    fn binding_starts_hidden_and_power_unknown_fail_closed() {
        let state = state();
        assert_eq!(state.high_water(), 0);
        assert_eq!(state.current(), None);
        assert!(!state.is_removed());
        assert_eq!(
            state.policy_snapshot(),
            ModePolicySnapshot::from_stored(SOURCE_ID, TARGET_ID, false, false, false, None)
                .unwrap()
        );
    }

    #[test]
    fn active_commit_uses_exact_geometry_and_owned_policy() {
        let mut state = state();
        let committed = state.publish_active(10, facts(true)).unwrap();
        assert_eq!(committed.reason(), PublicationReason::Committed);
        assert_eq!(committed.generation(), 10);
        assert_eq!(committed.mode(), state.current());
        let mode = committed.mode().unwrap();
        assert!(mode.active);
        assert!(!mode.visible);
        assert!(!mode.powered);
        assert_eq!((mode.source_id, mode.target_id), (SOURCE_ID, TARGET_ID));
        assert_eq!((mode.source_width, mode.source_height), (1920, 1080));
        assert_eq!((mode.target_width, mode.target_height), (1920, 1080));
        assert_eq!(committed.policy_snapshot().path_powered(), Some(true));
    }

    #[test]
    fn empty_commit_advances_and_clears_only_path_power() {
        let mut state = state();
        state.transition_visibility(1, SOURCE_ID, true).unwrap();
        state
            .transition_power(
                2,
                PowerSubject::Target {
                    target_id: TARGET_ID,
                },
                true,
            )
            .unwrap();
        state.publish_active(3, facts(true)).unwrap();

        let empty = state.publish_empty_commit(4, SOURCE_ID).unwrap();
        assert_eq!(empty.reason(), PublicationReason::Committed);
        assert_eq!(empty.generation(), 4);
        assert_eq!(empty.mode(), None);
        assert!(empty.policy_snapshot().visible());
        assert!(!empty.policy_snapshot().adapter_powered());
        assert!(empty.policy_snapshot().target_powered());
        assert_eq!(empty.policy_snapshot().path_powered(), None);
    }

    #[test]
    fn path_powered_off_is_still_a_present_active_mode() {
        let mut state = state();
        state.transition_visibility(1, SOURCE_ID, true).unwrap();
        let publication = state.publish_active(2, facts(false)).unwrap();
        let mode = publication.mode().unwrap();
        assert!(mode.active);
        assert!(mode.visible);
        assert!(!mode.powered);
        assert_eq!(publication.policy_snapshot().path_powered(), Some(false));
    }

    #[test]
    fn visibility_transitions_while_absent_and_recomputes_later_commit() {
        let mut state = state();
        let visible = state.transition_visibility(1, SOURCE_ID, true).unwrap();
        assert_eq!(visible.mode(), None);
        assert!(visible.policy_snapshot().visible());
        assert_eq!(visible.reason(), PublicationReason::VisibilityTransition);

        let committed = state.publish_active(2, facts(true)).unwrap();
        assert!(committed.mode().unwrap().visible);

        let hidden = state.transition_visibility(3, SOURCE_ID, false).unwrap();
        assert!(!hidden.mode().unwrap().visible);
        assert_eq!(hidden.mode().unwrap().generation, 3);
    }

    #[test]
    fn adapter_and_target_power_persist_while_absent() {
        let mut state = state();
        let adapter_off = state
            .transition_power(1, PowerSubject::Adapter, false)
            .unwrap();
        assert_eq!(adapter_off.mode(), None);
        assert!(!adapter_off.policy_snapshot().adapter_powered());

        let target_off = state
            .transition_power(
                2,
                PowerSubject::Target {
                    target_id: TARGET_ID,
                },
                false,
            )
            .unwrap();
        assert_eq!(target_off.mode(), None);
        assert!(!target_off.policy_snapshot().target_powered());

        let committed = state.publish_active(3, facts(true)).unwrap();
        assert!(!committed.mode().unwrap().powered);
        state
            .transition_power(4, PowerSubject::Adapter, true)
            .unwrap();
        let target_on = state
            .transition_power(
                5,
                PowerSubject::Target {
                    target_id: TARGET_ID,
                },
                true,
            )
            .unwrap();
        assert!(target_on.mode().unwrap().powered);
    }

    #[test]
    fn adapter_and_target_d0_observations_are_required_in_both_orders() {
        fn exercise(first: PowerSubject, second: PowerSubject) -> CommittedModePublication {
            let mut state = state();
            assert!(
                !state
                    .publish_active(1, facts(true))
                    .unwrap()
                    .mode()
                    .unwrap()
                    .powered
            );
            let one_sided = state.transition_power(2, first, true).unwrap();
            assert!(!one_sided.mode().unwrap().powered);
            state.transition_power(3, second, true).unwrap()
        }

        let adapter_then_target = exercise(
            PowerSubject::Adapter,
            PowerSubject::Target {
                target_id: TARGET_ID,
            },
        );
        let target_then_adapter = exercise(
            PowerSubject::Target {
                target_id: TARGET_ID,
            },
            PowerSubject::Adapter,
        );
        assert!(adapter_then_target.mode().unwrap().powered);
        assert!(target_then_adapter.mode().unwrap().powered);
        assert_eq!(
            adapter_then_target.policy_snapshot(),
            target_then_adapter.policy_snapshot()
        );
        assert!(adapter_then_target.policy_snapshot().adapter_powered());
        assert!(adapter_then_target.policy_snapshot().target_powered());
    }

    #[test]
    fn effective_power_is_the_conjunction_of_all_three_facts() {
        for path_powered in [false, true] {
            for adapter_powered in [false, true] {
                for target_powered in [false, true] {
                    let mut state = state();
                    state
                        .transition_power(1, PowerSubject::Adapter, adapter_powered)
                        .unwrap();
                    state
                        .transition_power(
                            2,
                            PowerSubject::Target {
                                target_id: TARGET_ID,
                            },
                            target_powered,
                        )
                        .unwrap();
                    let mode = state
                        .publish_active(3, facts(path_powered))
                        .unwrap()
                        .mode()
                        .unwrap();
                    assert_eq!(
                        mode.powered,
                        path_powered && adapter_powered && target_powered
                    );
                }
            }
        }
    }

    #[test]
    fn absent_tombstones_advance_and_preserve_policy() {
        let mut state = state();
        state.transition_visibility(1, SOURCE_ID, true).unwrap();
        state
            .transition_power(
                2,
                PowerSubject::Target {
                    target_id: TARGET_ID,
                },
                false,
            )
            .unwrap();
        state.publish_active(3, facts(true)).unwrap();

        let invalidated = state.invalidate(4, SOURCE_ID).unwrap();
        assert_eq!(invalidated.reason(), PublicationReason::Invalidated);
        assert_eq!(invalidated.mode(), None);
        assert!(invalidated.policy_snapshot().visible());
        assert!(!invalidated.policy_snapshot().target_powered());
        assert_eq!(invalidated.policy_snapshot().path_powered(), None);

        let invalidated_again = state.invalidate(5, SOURCE_ID).unwrap();
        assert_eq!(invalidated_again.mode(), None);
        let reset = state.reset(6).unwrap();
        assert_eq!(reset.reason(), PublicationReason::Reset);
        assert_eq!(reset.mode(), None);
        let removed = state.remove(7).unwrap();
        assert_eq!(removed.reason(), PublicationReason::Removed);
        assert_eq!(removed.policy_snapshot(), invalidated.policy_snapshot());
        assert!(state.is_removed());
        assert_eq!(state.invalidate(8, SOURCE_ID), Err(Refusal::Removed));
    }

    #[test]
    fn restored_publication_retains_every_policy_factor() {
        let mut original = state();
        original.transition_visibility(40, SOURCE_ID, true).unwrap();
        original
            .transition_power(41, PowerSubject::Adapter, true)
            .unwrap();
        original
            .transition_power(
                42,
                PowerSubject::Target {
                    target_id: TARGET_ID,
                },
                false,
            )
            .unwrap();
        let publication = original.publish_active(43, facts(true)).unwrap();

        let mut restored = CommittedModeState::restore(
            publication.generation(),
            publication.mode(),
            false,
            publication.policy_snapshot(),
        )
        .unwrap();
        assert_eq!(restored.policy_snapshot(), original.policy_snapshot());
        assert_eq!(restored.next_generation(), Ok(44));
        let powered = restored
            .transition_power(
                44,
                PowerSubject::Target {
                    target_id: TARGET_ID,
                },
                true,
            )
            .unwrap();
        assert!(powered.mode().unwrap().powered);

        let empty = original.publish_empty_commit(44, SOURCE_ID).unwrap();
        let empty_restored = CommittedModeState::restore(
            empty.generation(),
            empty.mode(),
            false,
            empty.policy_snapshot(),
        )
        .unwrap();
        assert_eq!(empty_restored.policy_snapshot(), empty.policy_snapshot());
    }

    #[test]
    fn valid_committed_scaling_is_preserved_for_admission_to_refuse() {
        let mut state = state();
        let mut scaled = facts(true);
        scaled.target_width = 1280;
        scaled.target_height = 720;
        let mode = state.publish_active(1, scaled).unwrap().mode().unwrap();
        assert_eq!((mode.source_width, mode.source_height), (1920, 1080));
        assert_eq!((mode.target_width, mode.target_height), (1280, 720));
    }

    #[test]
    fn strict_generation_rules_hold_for_empty_and_present_publications() {
        let mut state = state();
        assert_eq!(
            state.publish_empty_commit(0, SOURCE_ID),
            Err(Refusal::GenerationZero)
        );
        state.publish_empty_commit(5, SOURCE_ID).unwrap();
        assert_eq!(
            state.transition_visibility(5, SOURCE_ID, true),
            Err(Refusal::GenerationReused { value: 5 })
        );
        assert_eq!(
            state.transition_power(4, PowerSubject::Adapter, false),
            Err(Refusal::GenerationWentBackward {
                high_water: 5,
                found: 4,
            })
        );
        assert_eq!(state.next_generation(), Ok(6));
    }

    fn refusal_kind(refusal: Refusal) -> usize {
        match refusal {
            Refusal::Removed => 0,
            Refusal::GenerationZero => 1,
            Refusal::GenerationReused { .. } => 2,
            Refusal::GenerationWentBackward { .. } => 3,
            Refusal::GenerationExhausted { .. } => 4,
            Refusal::SourceUninitialized => 5,
            Refusal::TargetUninitialized => 6,
            Refusal::SourceIdentityMismatch { .. } => 7,
            Refusal::TargetIdentityMismatch { .. } => 8,
            Refusal::SourceExtentZero => 9,
            Refusal::TargetExtentZero => 10,
            Refusal::RemovedStateHasMode => 11,
            Refusal::RemovedStateGenerationZero => 12,
            Refusal::CurrentGenerationDoesNotMatchHighWater { .. } => 13,
            Refusal::CurrentModeInactive => 14,
            Refusal::PresentModeMissingPathPower => 15,
            Refusal::AbsentModeHasPathPower => 16,
            Refusal::CurrentModeVisibilityDoesNotMatchPolicy { .. } => 17,
            Refusal::CurrentModePowerDoesNotMatchPolicy { .. } => 18,
        }
    }

    struct RefusalCase {
        name: &'static str,
        exercise: fn(&mut CommittedModeState) -> Result<CommittedModePublication, Refusal>,
        expected: Refusal,
    }

    fn removed(state: &mut CommittedModeState) -> Result<CommittedModePublication, Refusal> {
        state.remove(1).unwrap();
        state.publish_empty_commit(2, SOURCE_ID)
    }

    fn generation_zero(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_active(0, facts(true))
    }

    fn generation_reused(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_empty_commit(5, SOURCE_ID).unwrap();
        state.transition_visibility(5, SOURCE_ID, false)
    }

    fn generation_backward(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_empty_commit(5, SOURCE_ID).unwrap();
        state.transition_visibility(4, SOURCE_ID, false)
    }

    fn generation_exhausted(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_empty_commit(u64::MAX, SOURCE_ID).unwrap();
        state.transition_power(0, PowerSubject::Adapter, false)
    }

    fn source_uninitialized(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        match CommittedModeState::new(D3DDDI_ID_UNINITIALIZED, TARGET_ID) {
            Err(refusal) => Err(refusal),
            Ok(_) => panic!("uninitialized source binding was accepted"),
        }
    }

    fn target_uninitialized(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        match CommittedModeState::new(SOURCE_ID, D3DDDI_ID_UNINITIALIZED) {
            Err(refusal) => Err(refusal),
            Ok(_) => panic!("uninitialized target binding was accepted"),
        }
    }

    fn source_identity_mismatch(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_empty_commit(1, SOURCE_ID + 1)
    }

    fn target_identity_mismatch(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.transition_power(
            1,
            PowerSubject::Target {
                target_id: TARGET_ID + 1,
            },
            false,
        )
    }

    fn source_extent_zero(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let mut invalid = facts(true);
        invalid.source_width = 0;
        state.publish_active(1, invalid)
    }

    fn target_extent_zero(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let mut invalid = facts(true);
        invalid.target_height = 0;
        state.publish_active(1, invalid)
    }

    fn restored_mode() -> (CommittedMode, ModePolicySnapshot) {
        let mut state = state();
        let publication = state.publish_active(1, facts(true)).unwrap();
        (publication.mode().unwrap(), publication.policy_snapshot())
    }

    fn restore_error(
        result: Result<CommittedModeState, Refusal>,
    ) -> Result<CommittedModePublication, Refusal> {
        match result {
            Err(refusal) => Err(refusal),
            Ok(_) => panic!("invalid restored state was accepted"),
        }
    }

    fn removed_state_has_mode(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let (mode, policy) = restored_mode();
        restore_error(CommittedModeState::restore(1, Some(mode), true, policy))
    }

    fn removed_state_generation_zero(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let policy = state().policy_snapshot();
        restore_error(CommittedModeState::restore(0, None, true, policy))
    }

    fn current_generation_does_not_match_high_water(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let (mode, policy) = restored_mode();
        restore_error(CommittedModeState::restore(2, Some(mode), false, policy))
    }

    fn current_mode_inactive(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let (mut mode, policy) = restored_mode();
        mode.active = false;
        restore_error(CommittedModeState::restore(1, Some(mode), false, policy))
    }

    fn present_mode_missing_path_power(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let (mode, mut policy) = restored_mode();
        policy.path_powered = None;
        restore_error(CommittedModeState::restore(1, Some(mode), false, policy))
    }

    fn absent_mode_has_path_power(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let (_, policy) = restored_mode();
        restore_error(CommittedModeState::restore(1, None, false, policy))
    }

    fn current_mode_visibility_does_not_match_policy(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let (mut mode, policy) = restored_mode();
        mode.visible = !policy.visible;
        restore_error(CommittedModeState::restore(1, Some(mode), false, policy))
    }

    fn current_mode_power_does_not_match_policy(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let (mut mode, policy) = restored_mode();
        mode.powered = !mode.powered;
        restore_error(CommittedModeState::restore(1, Some(mode), false, policy))
    }

    const REFUSAL_CASES: &[RefusalCase] = &[
        RefusalCase {
            name: "removed",
            exercise: removed,
            expected: Refusal::Removed,
        },
        RefusalCase {
            name: "generation_zero",
            exercise: generation_zero,
            expected: Refusal::GenerationZero,
        },
        RefusalCase {
            name: "generation_reused",
            exercise: generation_reused,
            expected: Refusal::GenerationReused { value: 5 },
        },
        RefusalCase {
            name: "generation_backward",
            exercise: generation_backward,
            expected: Refusal::GenerationWentBackward {
                high_water: 5,
                found: 4,
            },
        },
        RefusalCase {
            name: "generation_exhausted",
            exercise: generation_exhausted,
            expected: Refusal::GenerationExhausted {
                high_water: u64::MAX,
            },
        },
        RefusalCase {
            name: "source_uninitialized",
            exercise: source_uninitialized,
            expected: Refusal::SourceUninitialized,
        },
        RefusalCase {
            name: "target_uninitialized",
            exercise: target_uninitialized,
            expected: Refusal::TargetUninitialized,
        },
        RefusalCase {
            name: "source_identity_mismatch",
            exercise: source_identity_mismatch,
            expected: Refusal::SourceIdentityMismatch {
                expected: SOURCE_ID,
                found: SOURCE_ID + 1,
            },
        },
        RefusalCase {
            name: "target_identity_mismatch",
            exercise: target_identity_mismatch,
            expected: Refusal::TargetIdentityMismatch {
                expected: TARGET_ID,
                found: TARGET_ID + 1,
            },
        },
        RefusalCase {
            name: "source_extent_zero",
            exercise: source_extent_zero,
            expected: Refusal::SourceExtentZero,
        },
        RefusalCase {
            name: "target_extent_zero",
            exercise: target_extent_zero,
            expected: Refusal::TargetExtentZero,
        },
        RefusalCase {
            name: "removed_state_has_mode",
            exercise: removed_state_has_mode,
            expected: Refusal::RemovedStateHasMode,
        },
        RefusalCase {
            name: "removed_state_generation_zero",
            exercise: removed_state_generation_zero,
            expected: Refusal::RemovedStateGenerationZero,
        },
        RefusalCase {
            name: "current_generation_does_not_match_high_water",
            exercise: current_generation_does_not_match_high_water,
            expected: Refusal::CurrentGenerationDoesNotMatchHighWater {
                high_water: 2,
                current: 1,
            },
        },
        RefusalCase {
            name: "current_mode_inactive",
            exercise: current_mode_inactive,
            expected: Refusal::CurrentModeInactive,
        },
        RefusalCase {
            name: "present_mode_missing_path_power",
            exercise: present_mode_missing_path_power,
            expected: Refusal::PresentModeMissingPathPower,
        },
        RefusalCase {
            name: "absent_mode_has_path_power",
            exercise: absent_mode_has_path_power,
            expected: Refusal::AbsentModeHasPathPower,
        },
        RefusalCase {
            name: "current_mode_visibility_does_not_match_policy",
            exercise: current_mode_visibility_does_not_match_policy,
            expected: Refusal::CurrentModeVisibilityDoesNotMatchPolicy {
                expected: false,
                found: true,
            },
        },
        RefusalCase {
            name: "current_mode_power_does_not_match_policy",
            exercise: current_mode_power_does_not_match_policy,
            expected: Refusal::CurrentModePowerDoesNotMatchPolicy {
                expected: false,
                found: true,
            },
        },
    ];

    #[test]
    fn named_refusal_matrix_is_exhaustive() {
        const REFUSAL_COUNT: usize = 19;
        let mut seen = [false; REFUSAL_COUNT];

        for case in REFUSAL_CASES {
            let mut state = state();
            assert_eq!(
                (case.exercise)(&mut state),
                Err(case.expected),
                "{}",
                case.name
            );
            let kind = refusal_kind(case.expected);
            assert!(!seen[kind], "duplicate refusal kind for {}", case.name);
            seen[kind] = true;
        }

        assert_eq!(REFUSAL_CASES.len(), REFUSAL_COUNT);
        assert!(seen.into_iter().all(|present| present));
    }

    #[test]
    fn refused_identity_or_geometry_does_not_mutate_policy_or_generation() {
        let mut state = state();
        let before = state;
        assert_eq!(
            state.transition_visibility(9, SOURCE_ID + 1, true),
            Err(Refusal::SourceIdentityMismatch {
                expected: SOURCE_ID,
                found: SOURCE_ID + 1,
            })
        );
        assert_eq!(state, before);

        assert_eq!(
            state.publish_empty_commit(9, SOURCE_ID + 1),
            Err(Refusal::SourceIdentityMismatch {
                expected: SOURCE_ID,
                found: SOURCE_ID + 1,
            })
        );
        assert_eq!(state, before);

        assert_eq!(
            state.invalidate(9, SOURCE_ID + 1),
            Err(Refusal::SourceIdentityMismatch {
                expected: SOURCE_ID,
                found: SOURCE_ID + 1,
            })
        );
        assert_eq!(state, before);

        assert_eq!(
            state.transition_power(
                9,
                PowerSubject::Target {
                    target_id: TARGET_ID + 1,
                },
                false,
            ),
            Err(Refusal::TargetIdentityMismatch {
                expected: TARGET_ID,
                found: TARGET_ID + 1,
            })
        );
        assert_eq!(state, before);

        let mut invalid = facts(true);
        invalid.source_height = 0;
        assert_eq!(
            state.publish_active(9, invalid),
            Err(Refusal::SourceExtentZero)
        );
        assert_eq!(state, before);
        assert_eq!(
            state.publish_active(9, facts(true)).unwrap().generation(),
            9
        );
    }
}
