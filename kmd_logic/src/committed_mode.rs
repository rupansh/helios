//! Pure committed-mode publication model. Every accepted transition consumes a
//! strictly newer generation and returns a complete by-value record for a
//! platform-owned atomic publisher; this state owns no display object.

use crate::direct_scanout_admission::CommittedMode;
use helios_protocol::D3DDDI_ID_UNINITIALIZED;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveModeFacts {
    pub source_id: u32,
    pub target_id: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub target_width: u32,
    pub target_height: u32,
    pub visible: bool,
    pub powered: bool,
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
pub struct CommittedModePublication {
    generation: u64,
    mode: Option<CommittedMode>,
    reason: PublicationReason,
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
    SourceExtentZero,
    TargetExtentZero,
    NoCommittedMode,
    RemovedStateHasMode,
    RemovedStateGenerationZero,
    CurrentGenerationDoesNotMatchHighWater { high_water: u64, current: u64 },
    CurrentModeInactive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedModeState {
    high_water: u64,
    current: Option<CommittedMode>,
    removed: bool,
}

impl CommittedModeState {
    pub const fn new() -> Self {
        Self {
            high_water: 0,
            current: None,
            removed: false,
        }
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

    pub fn restore(
        high_water: u64,
        current: Option<CommittedMode>,
        removed: bool,
    ) -> Result<Self, Refusal> {
        if removed && current.is_some() {
            return Err(Refusal::RemovedStateHasMode);
        }
        if removed && high_water == 0 {
            return Err(Refusal::RemovedStateGenerationZero);
        }
        if let Some(mode) = current {
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
            validate_active_facts(ActiveModeFacts {
                source_id: mode.source_id,
                target_id: mode.target_id,
                source_width: mode.source_width,
                source_height: mode.source_height,
                target_width: mode.target_width,
                target_height: mode.target_height,
                visible: mode.visible,
                powered: mode.powered,
            })?;
        }
        Ok(Self {
            high_water,
            current,
            removed,
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
        facts: ActiveModeFacts,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        validate_active_facts(facts)?;
        let mode = CommittedMode {
            generation,
            source_id: facts.source_id,
            target_id: facts.target_id,
            source_width: facts.source_width,
            source_height: facts.source_height,
            target_width: facts.target_width,
            target_height: facts.target_height,
            active: true,
            visible: facts.visible,
            powered: facts.powered,
        };
        Ok(self.commit(generation, Some(mode), PublicationReason::Committed))
    }

    pub fn transition_visibility(
        &mut self,
        generation: u64,
        visible: bool,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        let Some(mut mode) = self.current else {
            return Err(Refusal::NoCommittedMode);
        };
        mode.generation = generation;
        mode.visible = visible;
        Ok(self.commit(
            generation,
            Some(mode),
            PublicationReason::VisibilityTransition,
        ))
    }

    pub fn transition_power(
        &mut self,
        generation: u64,
        powered: bool,
    ) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        let Some(mut mode) = self.current else {
            return Err(Refusal::NoCommittedMode);
        };
        mode.generation = generation;
        mode.powered = powered;
        Ok(self.commit(generation, Some(mode), PublicationReason::PowerTransition))
    }

    pub fn invalidate(&mut self, generation: u64) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        if self.current.is_none() {
            return Err(Refusal::NoCommittedMode);
        }
        Ok(self.commit(generation, None, PublicationReason::Invalidated))
    }

    pub fn reset(&mut self, generation: u64) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        Ok(self.commit(generation, None, PublicationReason::Reset))
    }

    pub fn remove(&mut self, generation: u64) -> Result<CommittedModePublication, Refusal> {
        self.validate_generation(generation)?;
        let publication = self.commit(generation, None, PublicationReason::Removed);
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
        }
    }
}

impl Default for CommittedModeState {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_active_facts(facts: ActiveModeFacts) -> Result<(), Refusal> {
    if facts.source_id == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::SourceUninitialized);
    }
    if facts.target_id == D3DDDI_ID_UNINITIALIZED {
        return Err(Refusal::TargetUninitialized);
    }
    if facts.source_width == 0 || facts.source_height == 0 {
        return Err(Refusal::SourceExtentZero);
    }
    if facts.target_width == 0 || facts.target_height == 0 {
        return Err(Refusal::TargetExtentZero);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ActiveModeFacts {
        ActiveModeFacts {
            source_id: 3,
            target_id: 7,
            source_width: 1920,
            source_height: 1080,
            target_width: 1920,
            target_height: 1080,
            visible: true,
            powered: true,
        }
    }

    #[test]
    fn complete_snapshots_and_state_transitions_advance_generation() {
        let mut state = CommittedModeState::new();
        let committed = state.publish_active(10, facts()).unwrap();
        assert_eq!(committed.reason, PublicationReason::Committed);
        assert_eq!(committed.generation, 10);
        assert_eq!(committed.mode, state.current());
        let mode = committed.mode.unwrap();
        assert!(mode.active);
        assert_eq!(mode.source_id, 3);
        assert_eq!(mode.target_id, 7);
        assert_eq!((mode.source_width, mode.source_height), (1920, 1080));
        assert_eq!((mode.target_width, mode.target_height), (1920, 1080));

        let visible = state.transition_visibility(11, true).unwrap();
        assert_eq!(visible.reason, PublicationReason::VisibilityTransition);
        assert_eq!(visible.mode.unwrap().generation, 11);
        assert_eq!(visible.mode.unwrap().source_width, 1920);

        let hidden = state.transition_visibility(12, false).unwrap();
        assert!(!hidden.mode.unwrap().visible);
        assert_eq!(hidden.mode.unwrap().generation, 12);

        let powered = state.transition_power(13, true).unwrap();
        assert_eq!(powered.reason, PublicationReason::PowerTransition);
        assert_eq!(powered.mode.unwrap().generation, 13);

        let off = state.transition_power(14, false).unwrap();
        assert!(!off.mode.unwrap().powered);
        assert_eq!(off.mode.unwrap().generation, 14);
        assert_eq!(state.high_water(), 14);
    }

    #[test]
    fn invalidate_reset_and_remove_publish_distinct_empty_states() {
        let mut state = CommittedModeState::new();
        state.publish_active(1, facts()).unwrap();

        let invalidated = state.invalidate(2).unwrap();
        assert_eq!(invalidated.reason, PublicationReason::Invalidated);
        assert_eq!(invalidated.mode, None);
        assert_eq!(state.current(), None);
        assert_eq!(
            state.transition_visibility(3, false),
            Err(Refusal::NoCommittedMode)
        );
        assert_eq!(state.high_water(), 2);

        state.publish_active(3, facts()).unwrap();
        let reset = state.reset(4).unwrap();
        assert_eq!(reset.reason, PublicationReason::Reset);
        assert_eq!(reset.mode, None);
        assert_eq!(state.reset(5).unwrap().generation, 5);

        state.publish_active(6, facts()).unwrap();
        let removed = state.remove(7).unwrap();
        assert_eq!(removed.reason, PublicationReason::Removed);
        assert_eq!(removed.mode, None);
        assert!(state.is_removed());
        assert_eq!(state.publish_active(8, facts()), Err(Refusal::Removed));
        assert_eq!(state.high_water(), 7);
    }

    #[test]
    fn valid_committed_scaling_is_preserved_for_admission_to_refuse() {
        let mut state = CommittedModeState::new();
        let mut scaled = facts();
        scaled.target_width = 1280;
        scaled.target_height = 720;
        let mode = state.publish_active(1, scaled).unwrap().mode.unwrap();
        assert_eq!((mode.source_width, mode.source_height), (1920, 1080));
        assert_eq!((mode.target_width, mode.target_height), (1280, 720));
    }

    #[test]
    fn restored_snapshots_continue_from_the_exact_generation() {
        let mut original = CommittedModeState::new();
        let publication = original.publish_active(41, facts()).unwrap();
        assert_eq!(publication.generation(), 41);
        assert_eq!(publication.reason(), PublicationReason::Committed);

        let mut active = CommittedModeState::restore(41, publication.mode(), false).unwrap();
        assert_eq!(active.next_generation(), Ok(42));
        assert_eq!(
            active
                .transition_visibility(42, false)
                .unwrap()
                .generation(),
            42
        );

        let mut empty = CommittedModeState::restore(42, None, false).unwrap();
        assert_eq!(empty.next_generation(), Ok(43));
        assert_eq!(empty.reset(43).unwrap().mode(), None);

        let mut zero = publication.mode().unwrap();
        zero.generation = 0;
        assert_eq!(
            CommittedModeState::restore(0, Some(zero), false),
            Err(Refusal::GenerationZero)
        );

        let terminal = CommittedModeState::restore(43, None, true).unwrap();
        assert_eq!(terminal.next_generation(), Err(Refusal::Removed));
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
            Refusal::SourceExtentZero => 7,
            Refusal::TargetExtentZero => 8,
            Refusal::NoCommittedMode => 9,
            Refusal::RemovedStateHasMode => 10,
            Refusal::RemovedStateGenerationZero => 11,
            Refusal::CurrentGenerationDoesNotMatchHighWater { .. } => 12,
            Refusal::CurrentModeInactive => 13,
        }
    }

    struct RefusalCase {
        name: &'static str,
        exercise: fn(&mut CommittedModeState) -> Result<CommittedModePublication, Refusal>,
        expected: Refusal,
    }

    fn removed(state: &mut CommittedModeState) -> Result<CommittedModePublication, Refusal> {
        state.remove(1).unwrap();
        state.publish_active(2, facts())
    }

    fn generation_zero(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_active(0, facts())
    }

    fn generation_reused(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_active(5, facts()).unwrap();
        state.transition_visibility(5, false)
    }

    fn generation_backward(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_active(5, facts()).unwrap();
        state.transition_visibility(4, false)
    }

    fn generation_exhausted(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.publish_active(u64::MAX, facts()).unwrap();
        state.transition_power(0, false)
    }

    fn source_uninitialized(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let mut invalid = facts();
        invalid.source_id = D3DDDI_ID_UNINITIALIZED;
        state.publish_active(1, invalid)
    }

    fn target_uninitialized(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let mut invalid = facts();
        invalid.target_id = D3DDDI_ID_UNINITIALIZED;
        state.publish_active(1, invalid)
    }

    fn source_extent_zero(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let mut invalid = facts();
        invalid.source_width = 0;
        state.publish_active(1, invalid)
    }

    fn target_extent_zero(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let mut invalid = facts();
        invalid.target_height = 0;
        state.publish_active(1, invalid)
    }

    fn no_committed_mode(
        state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        state.transition_power(1, false)
    }

    fn restored_mode(generation: u64) -> CommittedMode {
        let mut state = CommittedModeState::new();
        state
            .publish_active(generation, facts())
            .unwrap()
            .mode()
            .unwrap()
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
        restore_error(CommittedModeState::restore(1, Some(restored_mode(1)), true))
    }

    fn removed_state_generation_zero(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        restore_error(CommittedModeState::restore(0, None, true))
    }

    fn current_generation_does_not_match_high_water(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        restore_error(CommittedModeState::restore(
            2,
            Some(restored_mode(1)),
            false,
        ))
    }

    fn current_mode_inactive(
        _state: &mut CommittedModeState,
    ) -> Result<CommittedModePublication, Refusal> {
        let mut mode = restored_mode(1);
        mode.active = false;
        restore_error(CommittedModeState::restore(1, Some(mode), false))
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
            name: "no_committed_mode",
            exercise: no_committed_mode,
            expected: Refusal::NoCommittedMode,
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
    ];

    #[test]
    fn named_refusal_matrix_is_exhaustive() {
        const REFUSAL_COUNT: usize = 14;
        let mut seen = [false; REFUSAL_COUNT];

        for case in REFUSAL_CASES {
            let mut state = CommittedModeState::new();
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
    fn refused_snapshot_does_not_consume_the_generation() {
        let mut state = CommittedModeState::new();
        let mut invalid = facts();
        invalid.source_height = 0;
        assert_eq!(
            state.publish_active(9, invalid),
            Err(Refusal::SourceExtentZero)
        );
        assert_eq!(state.high_water(), 0);
        assert_eq!(state.current(), None);
        assert_eq!(state.publish_active(9, facts()).unwrap().generation, 9);
    }
}
