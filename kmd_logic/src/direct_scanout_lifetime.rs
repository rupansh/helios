//! Fixed-storage lifetime model for one direct-scanout source/plane.
//! A fenced nonzero replacement is the only ordinary release boundary; SET(0)
//! never releases the KMD-owned parking binding retained by this state.

use helios_protocol::diagnostics::{
    HeliosEtwEventId, HELIOS_ETW_SUBKIND_DEVICE_REMOVAL, HELIOS_ETW_SUBKIND_DEVICE_RESET,
    HELIOS_ETW_SUBKIND_PLANE_CANCEL, HELIOS_ETW_SUBKIND_PLANE_LATCH,
    HELIOS_ETW_SUBKIND_PLANE_READER_RELEASE, HELIOS_ETW_SUBKIND_PLANE_UNBIND,
    HELIOS_ETW_SUBKIND_SOLE,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrainReason {
    ExplicitUnbind,
    SourceInvisible,
    ModeChange,
    PowerTransition,
    DwmRestart,
    AdapterStop,
    AllocationDestroyed,
}

/// A stable 1-based code for each drain reason, so a KMD counter can name why
/// the plane stopped. Same shape as [`refusal_code`] and for the same reason:
/// the plane drained ~10 s into every boot and published only "not active".
pub fn drain_reason_code(reason: DrainReason) -> u32 {
    use DrainReason as D;
    match reason {
        D::ExplicitUnbind => 1,
        D::SourceInvisible => 2,
        D::ModeChange => 3,
        D::PowerTransition => 4,
        D::DwmRestart => 5,
        D::AdapterStop => 6,
        D::AllocationDestroyed => 7,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Active,
    Draining(DrainReason),
    Quiescent(DrainReason),
    ResetQuiescent,
    Poisoned,
    Removed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompletionKey {
    pub transport_epoch: u64,
    pub fence_id: u64,
    pub binding_sequence: u64,
}

impl CompletionKey {
    pub const fn new(transport_epoch: u64, fence_id: u64, binding_sequence: u64) -> Self {
        Self {
            transport_epoch,
            fence_id,
            binding_sequence,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Binding<T> {
    token: T,
    object_generation: u64,
}

impl<T> Binding<T> {
    pub const fn new(token: T, object_generation: u64) -> Self {
        Self {
            token,
            object_generation,
        }
    }

    pub const fn token(&self) -> &T {
        &self.token
    }

    pub const fn object_generation(&self) -> u64 {
        self.object_generation
    }

    pub fn into_token(self) -> T {
        self.token
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum BackendBinding<T> {
    Real(Binding<T>),
    Parking(Binding<T>),
}

impl<T> BackendBinding<T> {
    pub const fn is_real(&self) -> bool {
        matches!(self, Self::Real(_))
    }

    pub const fn is_parking(&self) -> bool {
        matches!(self, Self::Parking(_))
    }

    pub const fn binding(&self) -> &Binding<T> {
        match self {
            Self::Real(binding) | Self::Parking(binding) => binding,
        }
    }

    pub const fn object_generation(&self) -> u64 {
        self.binding().object_generation()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct HeldBackend<T> {
    pub binding: BackendBinding<T>,
    pub binding_sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingPhase {
    None,
    RealCandidate,
    ParkingCandidate,
    RealReplacement,
    ParkingReplacement,
    DisableZero,
}

#[derive(Debug, Eq, PartialEq)]
enum Pending<T> {
    None,
    Candidate {
        target: BackendBinding<T>,
        binding_sequence: u64,
    },
    ReplacementInFlight {
        target: BackendBinding<T>,
        key: CompletionKey,
    },
    DisableZeroInFlight {
        key: CompletionKey,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    PlaneCandidate {
        object_generation: u64,
        plane_generation: u64,
        binding_sequence: u64,
    },
    PlaneLatch {
        object_generation: u64,
        plane_generation: u64,
        binding_sequence: u64,
    },
    PlaneCancel {
        object_generation: u64,
        plane_generation: u64,
        binding_sequence: u64,
    },
    PlaneReaderRelease {
        object_generation: u64,
        plane_generation: u64,
        binding_sequence: u64,
    },
    PlaneUnbind {
        object_generation: u64,
        plane_generation: u64,
        binding_sequence: u64,
    },
    DeviceReset,
    DeviceRemoval,
}

impl Event {
    pub const fn etw_event_id(self) -> HeliosEtwEventId {
        match self {
            Self::PlaneCandidate { .. } => HeliosEtwEventId::PlaneCandidate,
            Self::PlaneLatch { .. } | Self::PlaneCancel { .. } => {
                HeliosEtwEventId::PlaneLatchOrCancel
            }
            Self::PlaneReaderRelease { .. } | Self::PlaneUnbind { .. } => {
                HeliosEtwEventId::PlaneReaderReleaseOrUnbind
            }
            Self::DeviceReset | Self::DeviceRemoval => HeliosEtwEventId::DeviceResetOrRemoval,
        }
    }

    pub const fn etw_subkind(self) -> u32 {
        match self {
            Self::PlaneCandidate { .. } => HELIOS_ETW_SUBKIND_SOLE,
            Self::PlaneLatch { .. } => HELIOS_ETW_SUBKIND_PLANE_LATCH,
            Self::PlaneCancel { .. } => HELIOS_ETW_SUBKIND_PLANE_CANCEL,
            Self::PlaneReaderRelease { .. } => HELIOS_ETW_SUBKIND_PLANE_READER_RELEASE,
            Self::PlaneUnbind { .. } => HELIOS_ETW_SUBKIND_PLANE_UNBIND,
            Self::DeviceReset => HELIOS_ETW_SUBKIND_DEVICE_RESET,
            Self::DeviceRemoval => HELIOS_ETW_SUBKIND_DEVICE_REMOVAL,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    NoChange,
    CandidateRetained,
    CandidateSubmitted,
    CandidateCancelled,
    ReplacementLatched,
    ReplacementFailed,
    NeedParkingReplacement,
    ParkingLatched,
    ParkingReplacementFailed,
    DrainPending,
    DrainComplete,
    DisableZeroSubmitted,
    DisableZeroCompleted,
    DisableZeroFailed,
    Resumed,
    ResetComplete,
    ResetPoisoned,
    RemovalComplete,
    Refused,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    ZeroPlaneGeneration,
    ZeroTransportEpoch,
    ZeroObjectGeneration,
    ZeroBindingSequence,
    ZeroFenceId,
    BindingSequenceReused { value: u64 },
    BindingSequenceWentBackward { high_water: u64, found: u64 },
    BindingSequenceExhausted,
    FenceIdReused { value: u64 },
    FenceIdWentBackward { high_water: u64, found: u64 },
    FenceIdExhausted,
    TransportEpochMismatch { expected: u64, found: u64 },
    BindingSequenceMismatch { expected: u64, found: u64 },
    PendingBusy,
    NoCandidate,
    WrongPendingKind,
    NotActive,
    NotDraining,
    NotQuiescent,
    NoBackendToPark,
    BackendNotParking,
    NoCompletionPending,
    StaleCompletion,
    PlaneGenerationExhausted,
    TransportEpochExhausted,
    Poisoned,
    Removed,
    EventCapacityExceeded,
}

/// A stable 1-based code for each refusal, so a KMD counter can name which
/// transition was refused. The plane poisons on a refused transition and
/// published only "poisoned" until 22.22.346.0, which cost a deploy cycle
/// guessing between five `submit_candidate` arms.
pub fn refusal_code(refusal: Refusal) -> u32 {
    use Refusal as R;
    match refusal {
        R::ZeroPlaneGeneration => 1,
        R::ZeroTransportEpoch => 2,
        R::ZeroObjectGeneration => 3,
        R::ZeroBindingSequence => 4,
        R::ZeroFenceId => 5,
        R::BindingSequenceReused { .. } => 6,
        R::BindingSequenceWentBackward { .. } => 7,
        R::BindingSequenceExhausted => 8,
        R::FenceIdReused { .. } => 9,
        R::FenceIdWentBackward { .. } => 10,
        R::FenceIdExhausted => 11,
        R::TransportEpochMismatch { .. } => 12,
        R::BindingSequenceMismatch { .. } => 13,
        R::PendingBusy => 14,
        R::NoCandidate => 15,
        R::WrongPendingKind => 16,
        R::NotActive => 17,
        R::NotDraining => 18,
        R::NotQuiescent => 19,
        R::NoBackendToPark => 20,
        R::BackendNotParking => 21,
        R::NoCompletionPending => 22,
        R::StaleCompletion => 23,
        R::PlaneGenerationExhausted => 24,
        R::TransportEpochExhausted => 25,
        R::Poisoned => 26,
        R::Removed => 27,
        R::EventCapacityExceeded => 28,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvariantViolation {
    ZeroPlaneGeneration,
    ZeroTransportEpoch,
    ZeroObjectGeneration,
    ZeroBindingSequence,
    ZeroFenceId,
    FenceWithoutBindingSequence,
    BackendSequenceBeyondHighWater,
    PendingSequenceNotHighWater,
    PendingFenceNotHighWater,
    PendingEpochMismatch,
    RealCandidateOutsideActive,
    ParkingCandidateOutsideDrain,
    RealBackendWhileQuiescent,
    ResetQuiescentOwnsBackend,
    ResetQuiescentOwnsHistory,
    DisableZeroOutsideQuiescent,
    DisableZeroWithoutParking,
    TerminalOwnsPending,
    RemovedOwnsBackend,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Transition<T> {
    pub effect: Effect,
    pub refusal: Option<Refusal>,
    pub release_candidate: Option<BackendBinding<T>>,
    pub release_backend: Option<BackendBinding<T>>,
    pub events: [Option<Event>; 3],
}

impl<T> Transition<T> {
    fn new(effect: Effect) -> Self {
        Self {
            effect,
            refusal: None,
            release_candidate: None,
            release_backend: None,
            events: [None; 3],
        }
    }

    fn refused(reason: Refusal) -> Self {
        let mut transition = Self::new(Effect::Refused);
        transition.refusal = Some(reason);
        transition
    }

    fn refused_with_candidate(reason: Refusal, candidate: BackendBinding<T>) -> Self {
        let mut transition = Self::refused(reason);
        transition.release_candidate = Some(candidate);
        transition
    }

    fn push_event(&mut self, event: Event) {
        for slot in &mut self.events {
            if slot.is_none() {
                *slot = Some(event);
                return;
            }
        }
        self.refusal = Some(Refusal::EventCapacityExceeded);
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct PlaneState<T> {
    plane_generation: u64,
    transport_epoch: u64,
    binding_sequence_high_water: u64,
    fence_id_high_water: u64,
    lifecycle: Lifecycle,
    backend: Option<HeldBackend<T>>,
    pending: Pending<T>,
}

impl<T> PlaneState<T> {
    pub const fn new(plane_generation: u64, transport_epoch: u64) -> Result<Self, Refusal> {
        if plane_generation == 0 {
            return Err(Refusal::ZeroPlaneGeneration);
        }
        if transport_epoch == 0 {
            return Err(Refusal::ZeroTransportEpoch);
        }
        Ok(Self {
            plane_generation,
            transport_epoch,
            binding_sequence_high_water: 0,
            fence_id_high_water: 0,
            lifecycle: Lifecycle::Active,
            backend: None,
            pending: Pending::None,
        })
    }

    pub const fn plane_generation(&self) -> u64 {
        self.plane_generation
    }

    pub const fn transport_epoch(&self) -> u64 {
        self.transport_epoch
    }

    pub const fn binding_sequence_high_water(&self) -> u64 {
        self.binding_sequence_high_water
    }

    pub const fn fence_id_high_water(&self) -> u64 {
        self.fence_id_high_water
    }

    pub const fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }

    pub const fn backend(&self) -> Option<&HeldBackend<T>> {
        self.backend.as_ref()
    }

    pub const fn pending_phase(&self) -> PendingPhase {
        match &self.pending {
            Pending::None => PendingPhase::None,
            Pending::Candidate { target, .. } if target.is_real() => PendingPhase::RealCandidate,
            Pending::Candidate { .. } => PendingPhase::ParkingCandidate,
            Pending::ReplacementInFlight { target, .. } if target.is_real() => {
                PendingPhase::RealReplacement
            }
            Pending::ReplacementInFlight { .. } => PendingPhase::ParkingReplacement,
            Pending::DisableZeroInFlight { .. } => PendingPhase::DisableZero,
        }
    }

    pub const fn pending_binding(&self) -> Option<&BackendBinding<T>> {
        match &self.pending {
            Pending::Candidate { target, .. } | Pending::ReplacementInFlight { target, .. } => {
                Some(target)
            }
            Pending::None | Pending::DisableZeroInFlight { .. } => None,
        }
    }

    pub const fn held_reference_count(&self) -> usize {
        let backend = if self.backend.is_some() { 1 } else { 0 };
        let candidate = if matches!(
            self.pending,
            Pending::Candidate { .. } | Pending::ReplacementInFlight { .. }
        ) {
            1
        } else {
            0
        };
        backend + candidate
    }

    pub fn retain_real_candidate(
        &mut self,
        binding: Binding<T>,
        binding_sequence: u64,
    ) -> Transition<T> {
        let candidate = BackendBinding::Real(binding);
        if candidate.object_generation() == 0 {
            return Transition::refused_with_candidate(Refusal::ZeroObjectGeneration, candidate);
        }
        if binding_sequence == 0 {
            return Transition::refused_with_candidate(Refusal::ZeroBindingSequence, candidate);
        }
        if let Some(reason) = self.admission_refusal() {
            return Transition::refused_with_candidate(reason, candidate);
        }
        if !matches!(self.pending, Pending::None) {
            return Transition::refused_with_candidate(Refusal::PendingBusy, candidate);
        }
        if let Err(reason) = self.validate_new_binding_sequence(binding_sequence) {
            return Transition::refused_with_candidate(reason, candidate);
        }

        let object_generation = candidate.object_generation();
        self.binding_sequence_high_water = binding_sequence;
        self.pending = Pending::Candidate {
            target: candidate,
            binding_sequence,
        };
        let mut transition = Transition::new(Effect::CandidateRetained);
        transition.push_event(Event::PlaneCandidate {
            object_generation,
            plane_generation: self.plane_generation,
            binding_sequence,
        });
        transition
    }

    pub fn retain_parking_candidate(
        &mut self,
        binding: Binding<T>,
        binding_sequence: u64,
    ) -> Transition<T> {
        let candidate = BackendBinding::Parking(binding);
        if candidate.object_generation() == 0 {
            return Transition::refused_with_candidate(Refusal::ZeroObjectGeneration, candidate);
        }
        if binding_sequence == 0 {
            return Transition::refused_with_candidate(Refusal::ZeroBindingSequence, candidate);
        }
        match self.lifecycle {
            Lifecycle::Draining(_) => {}
            Lifecycle::Poisoned => {
                return Transition::refused_with_candidate(Refusal::Poisoned, candidate);
            }
            Lifecycle::Removed => {
                return Transition::refused_with_candidate(Refusal::Removed, candidate);
            }
            _ => return Transition::refused_with_candidate(Refusal::NotDraining, candidate),
        }
        if self.backend.is_none() {
            return Transition::refused_with_candidate(Refusal::NoBackendToPark, candidate);
        }
        if !matches!(self.pending, Pending::None) {
            return Transition::refused_with_candidate(Refusal::PendingBusy, candidate);
        }
        if let Err(reason) = self.validate_new_binding_sequence(binding_sequence) {
            return Transition::refused_with_candidate(reason, candidate);
        }
        self.binding_sequence_high_water = binding_sequence;
        self.pending = Pending::Candidate {
            target: candidate,
            binding_sequence,
        };
        Transition::new(Effect::CandidateRetained)
    }

    pub fn submit_candidate(&mut self, key: CompletionKey) -> Transition<T> {
        if let Err(reason) = self.validate_key(key) {
            return Transition::refused(reason);
        }
        let binding_sequence = match &self.pending {
            Pending::Candidate {
                binding_sequence, ..
            } => *binding_sequence,
            Pending::None => return Transition::refused(Refusal::NoCandidate),
            _ => return Transition::refused(Refusal::WrongPendingKind),
        };
        if key.binding_sequence != binding_sequence {
            return Transition::refused(Refusal::BindingSequenceMismatch {
                expected: binding_sequence,
                found: key.binding_sequence,
            });
        }
        if let Err(reason) = self.validate_new_fence_id(key.fence_id) {
            return Transition::refused(reason);
        }

        let pending = core::mem::replace(&mut self.pending, Pending::None);
        let target = match pending {
            Pending::Candidate { target, .. } => target,
            other => {
                self.pending = other;
                return Transition::refused(Refusal::WrongPendingKind);
            }
        };
        self.fence_id_high_water = key.fence_id;
        self.pending = Pending::ReplacementInFlight { target, key };
        Transition::new(Effect::CandidateSubmitted)
    }

    pub fn cancel_candidate(&mut self) -> Transition<T> {
        let pending = core::mem::replace(&mut self.pending, Pending::None);
        let Pending::Candidate {
            target,
            binding_sequence,
        } = pending
        else {
            self.pending = pending;
            return Transition::refused(if matches!(self.pending, Pending::None) {
                Refusal::NoCandidate
            } else {
                Refusal::WrongPendingKind
            });
        };

        let target_is_real = target.is_real();
        let object_generation = target.object_generation();
        let mut transition = Transition::new(if target_is_real {
            Effect::CandidateCancelled
        } else {
            Effect::ParkingReplacementFailed
        });
        if target_is_real {
            transition.push_event(Event::PlaneCancel {
                object_generation,
                plane_generation: self.plane_generation,
                binding_sequence,
            });
        }
        transition.release_candidate = Some(target);
        transition
    }

    pub fn begin_drain(&mut self, reason: DrainReason) -> Transition<T> {
        match self.lifecycle {
            Lifecycle::Removed => return Transition::refused(Refusal::Removed),
            Lifecycle::Poisoned => return Transition::refused(Refusal::Poisoned),
            Lifecycle::Draining(_) | Lifecycle::Quiescent(_) | Lifecycle::ResetQuiescent => {
                return Transition::new(Effect::NoChange);
            }
            Lifecycle::Active => self.lifecycle = Lifecycle::Draining(reason),
        }

        let pending = core::mem::replace(&mut self.pending, Pending::None);
        match pending {
            Pending::None => self.finish_drain_without_pending(reason),
            Pending::Candidate {
                target,
                binding_sequence,
            } => {
                let target_is_real = target.is_real();
                let object_generation = target.object_generation();
                let mut transition = self.finish_drain_without_pending(reason);
                transition.release_candidate = Some(target);
                if target_is_real {
                    transition.push_event(Event::PlaneCancel {
                        object_generation,
                        plane_generation: self.plane_generation,
                        binding_sequence,
                    });
                }
                transition
            }
            Pending::ReplacementInFlight { target, key } => {
                self.pending = Pending::ReplacementInFlight { target, key };
                Transition::new(Effect::DrainPending)
            }
            Pending::DisableZeroInFlight { key } => {
                self.pending = Pending::DisableZeroInFlight { key };
                self.lifecycle = Lifecycle::Quiescent(reason);
                Transition::new(Effect::DrainPending)
            }
        }
    }

    pub fn complete_replacement(&mut self, key: CompletionKey, success: bool) -> Transition<T> {
        if let Err(reason) = self.validate_key(key) {
            return Transition::refused(reason);
        }
        let pending = core::mem::replace(&mut self.pending, Pending::None);
        let Pending::ReplacementInFlight {
            target,
            key: expected,
        } = pending
        else {
            self.pending = pending;
            return Transition::refused(if matches!(self.pending, Pending::None) {
                Refusal::NoCompletionPending
            } else {
                Refusal::WrongPendingKind
            });
        };
        if key != expected {
            self.pending = Pending::ReplacementInFlight {
                target,
                key: expected,
            };
            return Transition::refused(Refusal::StaleCompletion);
        }

        let target_is_real = target.is_real();
        let target_generation = target.object_generation();
        if !success {
            let mut transition = Transition::new(if target_is_real {
                Effect::ReplacementFailed
            } else {
                Effect::ParkingReplacementFailed
            });
            transition.release_candidate = Some(target);
            if target_is_real {
                transition.push_event(Event::PlaneCancel {
                    object_generation: target_generation,
                    plane_generation: self.plane_generation,
                    binding_sequence: key.binding_sequence,
                });
                if let Lifecycle::Draining(reason) = self.lifecycle {
                    transition.effect = match self.backend.as_ref().map(|held| &held.binding) {
                        Some(BackendBinding::Real(_)) => Effect::NeedParkingReplacement,
                        Some(BackendBinding::Parking(_)) | None => {
                            self.lifecycle = Lifecycle::Quiescent(reason);
                            Effect::DrainComplete
                        }
                    };
                }
            }
            return transition;
        }

        let old = self.backend.replace(HeldBackend {
            binding: target,
            binding_sequence: key.binding_sequence,
        });
        let mut transition = Transition::new(Effect::ReplacementLatched);
        if target_is_real {
            transition.push_event(Event::PlaneLatch {
                object_generation: target_generation,
                plane_generation: self.plane_generation,
                binding_sequence: key.binding_sequence,
            });
            if let Some(HeldBackend {
                binding: BackendBinding::Real(old_binding),
                ..
            }) = old.as_ref()
            {
                transition.push_event(Event::PlaneReaderRelease {
                    object_generation: old_binding.object_generation(),
                    plane_generation: self.plane_generation,
                    binding_sequence: key.binding_sequence,
                });
            }
            if matches!(self.lifecycle, Lifecycle::Draining(_)) {
                transition.effect = Effect::NeedParkingReplacement;
            }
        } else {
            if let Some(HeldBackend {
                binding: BackendBinding::Real(old_binding),
                ..
            }) = old.as_ref()
            {
                transition.push_event(Event::PlaneUnbind {
                    object_generation: old_binding.object_generation(),
                    plane_generation: self.plane_generation,
                    binding_sequence: key.binding_sequence,
                });
            }
            if let Lifecycle::Draining(reason) = self.lifecycle {
                self.lifecycle = Lifecycle::Quiescent(reason);
            }
            transition.effect = Effect::ParkingLatched;
        }
        transition.release_backend = old.map(|held| held.binding);
        transition
    }

    pub fn submit_disable_zero(&mut self, key: CompletionKey) -> Transition<T> {
        if let Err(reason) = self.validate_key(key) {
            return Transition::refused(reason);
        }
        if !matches!(self.lifecycle, Lifecycle::Quiescent(_)) {
            return Transition::refused(Refusal::NotQuiescent);
        }
        if !self
            .backend
            .as_ref()
            .is_some_and(|held| held.binding.is_parking())
        {
            return Transition::refused(Refusal::BackendNotParking);
        }
        if !matches!(self.pending, Pending::None) {
            return Transition::refused(Refusal::PendingBusy);
        }
        if let Err(reason) = self.validate_new_binding_sequence(key.binding_sequence) {
            return Transition::refused(reason);
        }
        if let Err(reason) = self.validate_new_fence_id(key.fence_id) {
            return Transition::refused(reason);
        }
        self.binding_sequence_high_water = key.binding_sequence;
        self.fence_id_high_water = key.fence_id;
        self.pending = Pending::DisableZeroInFlight { key };
        Transition::new(Effect::DisableZeroSubmitted)
    }

    pub fn complete_disable_zero(&mut self, key: CompletionKey, success: bool) -> Transition<T> {
        if let Err(reason) = self.validate_key(key) {
            return Transition::refused(reason);
        }
        let Pending::DisableZeroInFlight { key: expected } = self.pending else {
            return Transition::refused(if matches!(self.pending, Pending::None) {
                Refusal::NoCompletionPending
            } else {
                Refusal::WrongPendingKind
            });
        };
        if key != expected {
            return Transition::refused(Refusal::StaleCompletion);
        }
        self.pending = Pending::None;
        Transition::new(if success {
            Effect::DisableZeroCompleted
        } else {
            Effect::DisableZeroFailed
        })
    }

    pub fn resume(&mut self) -> Transition<T> {
        if !matches!(self.pending, Pending::None) {
            return Transition::refused(Refusal::PendingBusy);
        }
        if self
            .backend
            .as_ref()
            .is_some_and(|held| held.binding.is_real())
        {
            return Transition::refused(Refusal::NotQuiescent);
        }
        match self.lifecycle {
            Lifecycle::Quiescent(_) => {
                let Some(next) = self.plane_generation.checked_add(1) else {
                    self.lifecycle = Lifecycle::Poisoned;
                    return Transition::refused(Refusal::PlaneGenerationExhausted);
                };
                self.plane_generation = next;
            }
            Lifecycle::ResetQuiescent => {}
            Lifecycle::Poisoned => return Transition::refused(Refusal::Poisoned),
            Lifecycle::Removed => return Transition::refused(Refusal::Removed),
            _ => return Transition::refused(Refusal::NotQuiescent),
        }
        self.lifecycle = Lifecycle::Active;
        Transition::new(Effect::Resumed)
    }

    pub fn complete_reset_barrier(&mut self) -> Transition<T> {
        if matches!(self.lifecycle, Lifecycle::Removed) {
            return Transition::refused(Refusal::Removed);
        }
        let mut transition = Transition::new(Effect::ResetComplete);
        self.release_all_for_barrier(&mut transition);
        transition.push_event(Event::DeviceReset);

        let next_plane = self.plane_generation.checked_add(1);
        let next_transport = self.transport_epoch.checked_add(1);
        match (next_plane, next_transport) {
            (Some(plane), Some(transport)) => {
                self.plane_generation = plane;
                self.transport_epoch = transport;
                self.binding_sequence_high_water = 0;
                self.fence_id_high_water = 0;
                self.lifecycle = Lifecycle::ResetQuiescent;
            }
            (None, _) => {
                self.lifecycle = Lifecycle::Poisoned;
                transition.effect = Effect::ResetPoisoned;
                transition.refusal = Some(Refusal::PlaneGenerationExhausted);
            }
            (_, None) => {
                self.lifecycle = Lifecycle::Poisoned;
                transition.effect = Effect::ResetPoisoned;
                transition.refusal = Some(Refusal::TransportEpochExhausted);
            }
        }
        transition
    }

    pub fn complete_removal_barrier(&mut self) -> Transition<T> {
        if matches!(self.lifecycle, Lifecycle::Removed) {
            return Transition::new(Effect::NoChange);
        }
        let mut transition = Transition::new(Effect::RemovalComplete);
        self.release_all_for_barrier(&mut transition);
        transition.push_event(Event::DeviceRemoval);
        self.lifecycle = Lifecycle::Removed;
        transition
    }

    pub fn check_invariants(&self) -> Result<(), InvariantViolation> {
        if self.plane_generation == 0 {
            return Err(InvariantViolation::ZeroPlaneGeneration);
        }
        if self.transport_epoch == 0 {
            return Err(InvariantViolation::ZeroTransportEpoch);
        }
        if self.fence_id_high_water != 0 && self.binding_sequence_high_water == 0 {
            return Err(InvariantViolation::FenceWithoutBindingSequence);
        }
        if let Some(backend) = &self.backend {
            if backend.binding.object_generation() == 0 {
                return Err(InvariantViolation::ZeroObjectGeneration);
            }
            if backend.binding_sequence == 0 {
                return Err(InvariantViolation::ZeroBindingSequence);
            }
            if backend.binding_sequence > self.binding_sequence_high_water {
                return Err(InvariantViolation::BackendSequenceBeyondHighWater);
            }
        }

        match &self.pending {
            Pending::None => {}
            Pending::Candidate {
                target,
                binding_sequence,
            } => {
                if target.object_generation() == 0 {
                    return Err(InvariantViolation::ZeroObjectGeneration);
                }
                if *binding_sequence == 0 {
                    return Err(InvariantViolation::ZeroBindingSequence);
                }
                if *binding_sequence != self.binding_sequence_high_water {
                    return Err(InvariantViolation::PendingSequenceNotHighWater);
                }
                if target.is_real() && !matches!(self.lifecycle, Lifecycle::Active) {
                    return Err(InvariantViolation::RealCandidateOutsideActive);
                }
                if target.is_parking() && !matches!(self.lifecycle, Lifecycle::Draining(_)) {
                    return Err(InvariantViolation::ParkingCandidateOutsideDrain);
                }
            }
            Pending::ReplacementInFlight { target, key } => {
                self.check_pending_key(*key)?;
                if key.binding_sequence != self.binding_sequence_high_water {
                    return Err(InvariantViolation::PendingSequenceNotHighWater);
                }
                if key.fence_id != self.fence_id_high_water {
                    return Err(InvariantViolation::PendingFenceNotHighWater);
                }
                if target.object_generation() == 0 {
                    return Err(InvariantViolation::ZeroObjectGeneration);
                }
                if target.is_real()
                    && !matches!(self.lifecycle, Lifecycle::Active | Lifecycle::Draining(_))
                {
                    return Err(InvariantViolation::RealCandidateOutsideActive);
                }
                if target.is_parking() && !matches!(self.lifecycle, Lifecycle::Draining(_)) {
                    return Err(InvariantViolation::ParkingCandidateOutsideDrain);
                }
            }
            Pending::DisableZeroInFlight { key } => {
                self.check_pending_key(*key)?;
                if key.binding_sequence != self.binding_sequence_high_water {
                    return Err(InvariantViolation::PendingSequenceNotHighWater);
                }
                if key.fence_id != self.fence_id_high_water {
                    return Err(InvariantViolation::PendingFenceNotHighWater);
                }
                if !matches!(self.lifecycle, Lifecycle::Quiescent(_)) {
                    return Err(InvariantViolation::DisableZeroOutsideQuiescent);
                }
                if !self
                    .backend
                    .as_ref()
                    .is_some_and(|held| held.binding.is_parking())
                {
                    return Err(InvariantViolation::DisableZeroWithoutParking);
                }
            }
        }

        match self.lifecycle {
            Lifecycle::Quiescent(_) => {
                if self
                    .backend
                    .as_ref()
                    .is_some_and(|held| held.binding.is_real())
                {
                    return Err(InvariantViolation::RealBackendWhileQuiescent);
                }
            }
            Lifecycle::ResetQuiescent => {
                if self.backend.is_some() {
                    return Err(InvariantViolation::ResetQuiescentOwnsBackend);
                }
                if self.binding_sequence_high_water != 0 || self.fence_id_high_water != 0 {
                    return Err(InvariantViolation::ResetQuiescentOwnsHistory);
                }
            }
            Lifecycle::Removed => {
                if !matches!(self.pending, Pending::None) {
                    return Err(InvariantViolation::TerminalOwnsPending);
                }
                if self.backend.is_some() {
                    return Err(InvariantViolation::RemovedOwnsBackend);
                }
            }
            Lifecycle::Poisoned => {
                if !matches!(self.pending, Pending::None) {
                    return Err(InvariantViolation::TerminalOwnsPending);
                }
                if self
                    .backend
                    .as_ref()
                    .is_some_and(|held| held.binding.is_real())
                {
                    return Err(InvariantViolation::RealBackendWhileQuiescent);
                }
            }
            Lifecycle::Active | Lifecycle::Draining(_) => {}
        }
        Ok(())
    }

    fn admission_refusal(&self) -> Option<Refusal> {
        match self.lifecycle {
            Lifecycle::Active => None,
            Lifecycle::Poisoned => Some(Refusal::Poisoned),
            Lifecycle::Removed => Some(Refusal::Removed),
            _ => Some(Refusal::NotActive),
        }
    }

    fn validate_key(&self, key: CompletionKey) -> Result<(), Refusal> {
        if key.transport_epoch == 0 {
            return Err(Refusal::ZeroTransportEpoch);
        }
        if key.fence_id == 0 {
            return Err(Refusal::ZeroFenceId);
        }
        if key.binding_sequence == 0 {
            return Err(Refusal::ZeroBindingSequence);
        }
        if key.transport_epoch != self.transport_epoch {
            return Err(Refusal::TransportEpochMismatch {
                expected: self.transport_epoch,
                found: key.transport_epoch,
            });
        }
        Ok(())
    }

    fn validate_new_binding_sequence(&self, found: u64) -> Result<(), Refusal> {
        if found == 0 {
            return Err(Refusal::ZeroBindingSequence);
        }
        if self.binding_sequence_high_water == u64::MAX {
            return Err(Refusal::BindingSequenceExhausted);
        }
        if found == self.binding_sequence_high_water {
            return Err(Refusal::BindingSequenceReused { value: found });
        }
        if found < self.binding_sequence_high_water {
            return Err(Refusal::BindingSequenceWentBackward {
                high_water: self.binding_sequence_high_water,
                found,
            });
        }
        Ok(())
    }

    fn validate_new_fence_id(&self, found: u64) -> Result<(), Refusal> {
        if found == 0 {
            return Err(Refusal::ZeroFenceId);
        }
        if self.fence_id_high_water == u64::MAX {
            return Err(Refusal::FenceIdExhausted);
        }
        if found == self.fence_id_high_water {
            return Err(Refusal::FenceIdReused { value: found });
        }
        if found < self.fence_id_high_water {
            return Err(Refusal::FenceIdWentBackward {
                high_water: self.fence_id_high_water,
                found,
            });
        }
        Ok(())
    }

    fn check_pending_key(&self, key: CompletionKey) -> Result<(), InvariantViolation> {
        if key.transport_epoch != self.transport_epoch {
            return Err(InvariantViolation::PendingEpochMismatch);
        }
        if key.fence_id == 0 {
            return Err(InvariantViolation::ZeroFenceId);
        }
        if key.binding_sequence == 0 {
            return Err(InvariantViolation::ZeroBindingSequence);
        }
        Ok(())
    }

    fn finish_drain_without_pending(&mut self, reason: DrainReason) -> Transition<T> {
        if self
            .backend
            .as_ref()
            .is_some_and(|held| held.binding.is_real())
        {
            Transition::new(Effect::NeedParkingReplacement)
        } else {
            self.lifecycle = Lifecycle::Quiescent(reason);
            Transition::new(Effect::DrainComplete)
        }
    }

    fn release_all_for_barrier(&mut self, transition: &mut Transition<T>) {
        let pending = core::mem::replace(&mut self.pending, Pending::None);
        match pending {
            Pending::Candidate {
                target,
                binding_sequence,
            } => {
                if target.is_real() {
                    transition.push_event(Event::PlaneCancel {
                        object_generation: target.object_generation(),
                        plane_generation: self.plane_generation,
                        binding_sequence,
                    });
                }
                transition.release_candidate = Some(target);
            }
            Pending::ReplacementInFlight { target, key } => {
                if target.is_real() {
                    transition.push_event(Event::PlaneCancel {
                        object_generation: target.object_generation(),
                        plane_generation: self.plane_generation,
                        binding_sequence: key.binding_sequence,
                    });
                }
                transition.release_candidate = Some(target);
            }
            Pending::None | Pending::DisableZeroInFlight { .. } => {}
        }

        if let Some(backend) = self.backend.take() {
            if let BackendBinding::Real(binding) = &backend.binding {
                transition.push_event(Event::PlaneUnbind {
                    object_generation: binding.object_generation(),
                    plane_generation: self.plane_generation,
                    binding_sequence: backend.binding_sequence,
                });
            }
            transition.release_backend = Some(backend.binding);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
    struct Token(u8);

    fn binding(token: u8) -> Binding<Token> {
        Binding::new(Token(token), u64::from(token) + 100)
    }

    fn key(fence_id: u64, binding_sequence: u64) -> CompletionKey {
        CompletionKey::new(1, fence_id, binding_sequence)
    }

    fn state() -> PlaneState<Token> {
        PlaneState::new(1, 1).expect("valid initial state")
    }

    fn install_real(state: &mut PlaneState<Token>, token: u8, sequence: u64) {
        assert_eq!(
            state.retain_real_candidate(binding(token), sequence).effect,
            Effect::CandidateRetained
        );
        assert_eq!(
            state.submit_candidate(key(sequence, sequence)).effect,
            Effect::CandidateSubmitted
        );
        assert_eq!(
            state
                .complete_replacement(key(sequence, sequence), true)
                .effect,
            Effect::ReplacementLatched
        );
        assert_eq!(state.check_invariants(), Ok(()));
    }

    fn token_of(binding: &BackendBinding<Token>) -> u8 {
        binding.binding().token().0
    }

    fn collect_releases(
        transition: Transition<Token>,
        released: &mut [u8; 5],
        released_len: &mut usize,
    ) {
        if let Some(binding) = transition.release_candidate {
            released[*released_len] = token_of(&binding);
            *released_len += 1;
        }
        if let Some(binding) = transition.release_backend {
            released[*released_len] = token_of(&binding);
            *released_len += 1;
        }
    }

    #[test]
    fn construction_and_candidate_refusals_are_loud_and_ownership_preserving() {
        assert_eq!(
            PlaneState::<Token>::new(0, 1).unwrap_err(),
            Refusal::ZeroPlaneGeneration
        );
        assert_eq!(
            PlaneState::<Token>::new(1, 0).unwrap_err(),
            Refusal::ZeroTransportEpoch
        );

        let mut state = state();
        let zero_generation = state.retain_real_candidate(Binding::new(Token(1), 0), 1);
        assert_eq!(zero_generation.refusal, Some(Refusal::ZeroObjectGeneration));
        assert_eq!(
            token_of(zero_generation.release_candidate.as_ref().unwrap()),
            1
        );
        let zero_sequence = state.retain_real_candidate(binding(2), 0);
        assert_eq!(zero_sequence.refusal, Some(Refusal::ZeroBindingSequence));
        assert_eq!(
            token_of(zero_sequence.release_candidate.as_ref().unwrap()),
            2
        );

        assert_eq!(
            state.retain_real_candidate(binding(3), 3).effect,
            Effect::CandidateRetained
        );
        let busy = state.retain_real_candidate(binding(4), 4);
        assert_eq!(busy.refusal, Some(Refusal::PendingBusy));
        assert_eq!(token_of(busy.release_candidate.as_ref().unwrap()), 4);
        assert_eq!(state.pending_phase(), PendingPhase::RealCandidate);
        assert_eq!(state.held_reference_count(), 1);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn prewire_cancel_releases_only_the_candidate_once() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);

        let cancelled = state.cancel_candidate();
        assert_eq!(cancelled.effect, Effect::CandidateCancelled);
        assert_eq!(token_of(cancelled.release_candidate.as_ref().unwrap()), 2);
        assert_eq!(token_of(&state.backend().unwrap().binding), 1);
        assert!(matches!(
            cancelled.events[0],
            Some(Event::PlaneCancel {
                object_generation: 102,
                binding_sequence: 2,
                ..
            })
        ));
        assert_eq!(state.cancel_candidate().refusal, Some(Refusal::NoCandidate));
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn exact_real_replacement_latches_then_releases_the_old_binding() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);
        state.submit_candidate(key(22, 2));

        let completed = state.complete_replacement(key(22, 2), true);
        assert_eq!(completed.effect, Effect::ReplacementLatched);
        assert_eq!(token_of(completed.release_backend.as_ref().unwrap()), 1);
        assert_eq!(token_of(&state.backend().unwrap().binding), 2);
        assert!(matches!(
            completed.events[0],
            Some(Event::PlaneLatch { .. })
        ));
        assert!(matches!(
            completed.events[1],
            Some(Event::PlaneReaderRelease {
                object_generation: 101,
                binding_sequence: 2,
                ..
            })
        ));
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn same_backing_replacement_still_releases_only_the_old_lease() {
        let mut state = state();
        install_real(&mut state, 7, 1);
        state.retain_real_candidate(binding(7), 2);
        state.submit_candidate(key(2, 2));
        let completed = state.complete_replacement(key(2, 2), true);

        assert_eq!(token_of(completed.release_backend.as_ref().unwrap()), 7);
        assert_eq!(token_of(&state.backend().unwrap().binding), 7);
        assert_eq!(state.held_reference_count(), 1);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn replacement_error_cancels_candidate_and_preserves_current() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);
        state.submit_candidate(key(20, 2));

        let failed = state.complete_replacement(key(20, 2), false);
        assert_eq!(failed.effect, Effect::ReplacementFailed);
        assert_eq!(token_of(failed.release_candidate.as_ref().unwrap()), 2);
        assert!(failed.release_backend.is_none());
        assert_eq!(token_of(&state.backend().unwrap().binding), 1);
        assert!(matches!(failed.events[0], Some(Event::PlaneCancel { .. })));
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn stale_and_duplicate_completions_are_inert() {
        let mut state = state();
        state.retain_real_candidate(binding(1), 1);
        state.submit_candidate(key(10, 1));

        let stale_fence = state.complete_replacement(key(11, 1), true);
        assert_eq!(stale_fence.refusal, Some(Refusal::StaleCompletion));
        let stale_sequence = state.complete_replacement(key(10, 2), true);
        assert_eq!(stale_sequence.refusal, Some(Refusal::StaleCompletion));
        let stale_epoch = state.complete_replacement(CompletionKey::new(2, 10, 1), true);
        assert_eq!(
            stale_epoch.refusal,
            Some(Refusal::TransportEpochMismatch {
                expected: 1,
                found: 2
            })
        );
        assert_eq!(state.pending_phase(), PendingPhase::RealReplacement);
        assert_eq!(state.held_reference_count(), 1);

        assert_eq!(
            state.complete_replacement(key(10, 1), true).effect,
            Effect::ReplacementLatched
        );
        let duplicate = state.complete_replacement(key(10, 1), true);
        assert_eq!(duplicate.refusal, Some(Refusal::NoCompletionPending));
        assert_eq!(token_of(&state.backend().unwrap().binding), 1);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn old_completion_keys_cannot_collide_with_a_newer_candidate() {
        let mut state = state();
        let old = key(10, 1);
        state.retain_real_candidate(binding(1), 1);
        state.submit_candidate(old);
        state.complete_replacement(old, true);

        state.retain_real_candidate(binding(2), 2);
        let old_sequence = state.submit_candidate(old);
        assert_eq!(
            old_sequence.refusal,
            Some(Refusal::BindingSequenceMismatch {
                expected: 2,
                found: 1,
            })
        );
        assert_eq!(state.pending_phase(), PendingPhase::RealCandidate);

        let reused_fence = state.submit_candidate(key(10, 2));
        assert_eq!(
            reused_fence.refusal,
            Some(Refusal::FenceIdReused { value: 10 })
        );
        assert_eq!(state.pending_phase(), PendingPhase::RealCandidate);

        let current = key(20, 2);
        assert_eq!(
            state.submit_candidate(current).effect,
            Effect::CandidateSubmitted
        );
        assert_eq!(
            state.complete_replacement(old, true).refusal,
            Some(Refusal::StaleCompletion)
        );
        assert_eq!(token_of(&state.backend().unwrap().binding), 1);
        assert_eq!(state.held_reference_count(), 2);

        let completed = state.complete_replacement(current, true);
        assert_eq!(token_of(completed.release_backend.as_ref().unwrap()), 1);
        assert_eq!(token_of(&state.backend().unwrap().binding), 2);
        assert_eq!(
            state.complete_replacement(current, true).refusal,
            Some(Refusal::NoCompletionPending)
        );
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn binding_sequence_reuse_backward_and_exhaustion_are_distinct() {
        let mut state = state();
        state.retain_real_candidate(binding(1), 5);
        assert_eq!(state.binding_sequence_high_water(), 5);
        state.cancel_candidate();

        let reused = state.retain_real_candidate(binding(2), 5);
        assert_eq!(
            reused.refusal,
            Some(Refusal::BindingSequenceReused { value: 5 })
        );
        assert_eq!(token_of(reused.release_candidate.as_ref().unwrap()), 2);
        let backward = state.retain_real_candidate(binding(3), 4);
        assert_eq!(
            backward.refusal,
            Some(Refusal::BindingSequenceWentBackward {
                high_water: 5,
                found: 4,
            })
        );

        state.begin_drain(DrainReason::ModeChange);
        state.resume();
        assert_eq!(state.binding_sequence_high_water(), 5);
        state.retain_real_candidate(binding(4), u64::MAX);
        state.cancel_candidate();
        let exhausted = state.retain_real_candidate(binding(5), u64::MAX);
        assert_eq!(exhausted.refusal, Some(Refusal::BindingSequenceExhausted));
        assert_eq!(token_of(exhausted.release_candidate.as_ref().unwrap()), 5);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn fence_reuse_backward_and_exhaustion_survive_terminal_errors() {
        let mut state = state();
        state.retain_real_candidate(binding(1), 1);
        state.submit_candidate(key(10, 1));
        state.complete_replacement(key(10, 1), false);
        assert_eq!(state.fence_id_high_water(), 10);

        state.retain_real_candidate(binding(2), 2);
        assert_eq!(
            state.submit_candidate(key(10, 2)).refusal,
            Some(Refusal::FenceIdReused { value: 10 })
        );
        assert_eq!(
            state.submit_candidate(key(9, 2)).refusal,
            Some(Refusal::FenceIdWentBackward {
                high_water: 10,
                found: 9,
            })
        );
        assert_eq!(state.pending_phase(), PendingPhase::RealCandidate);

        state.submit_candidate(key(u64::MAX, 2));
        state.complete_replacement(key(u64::MAX, 2), false);
        state.retain_real_candidate(binding(3), 3);
        assert_eq!(
            state.submit_candidate(key(u64::MAX, 3)).refusal,
            Some(Refusal::FenceIdExhausted)
        );
        assert_eq!(state.pending_phase(), PendingPhase::RealCandidate);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn wrong_submission_preserves_candidate_and_both_high_waters() {
        let mut state = state();
        state.retain_real_candidate(binding(1), 7);

        let wrong_epoch = state.submit_candidate(CompletionKey::new(2, 10, 7));
        assert_eq!(
            wrong_epoch.refusal,
            Some(Refusal::TransportEpochMismatch {
                expected: 1,
                found: 2,
            })
        );
        let wrong_sequence = state.submit_candidate(key(10, 8));
        assert_eq!(
            wrong_sequence.refusal,
            Some(Refusal::BindingSequenceMismatch {
                expected: 7,
                found: 8,
            })
        );
        assert_eq!(
            state.submit_candidate(key(0, 7)).refusal,
            Some(Refusal::ZeroFenceId)
        );
        assert_eq!(state.pending_phase(), PendingPhase::RealCandidate);
        assert_eq!(state.binding_sequence_high_water(), 7);
        assert_eq!(state.fence_id_high_water(), 0);
        assert_eq!(token_of(state.pending_binding().unwrap()), 1);

        assert_eq!(
            state.submit_candidate(key(10, 7)).effect,
            Effect::CandidateSubmitted
        );
        assert_eq!(state.fence_id_high_water(), 10);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn drain_replaces_real_with_nonzero_parking_before_release() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        assert_eq!(
            state.begin_drain(DrainReason::ModeChange).effect,
            Effect::NeedParkingReplacement
        );
        assert_eq!(token_of(&state.backend().unwrap().binding), 1);

        let retained = state.retain_parking_candidate(binding(9), 2);
        assert_eq!(retained.effect, Effect::CandidateRetained);
        assert_eq!(retained.events, [None; 3]);
        state.submit_candidate(key(20, 2));
        let parked = state.complete_replacement(key(20, 2), true);
        assert_eq!(parked.effect, Effect::ParkingLatched);
        assert_eq!(token_of(parked.release_backend.as_ref().unwrap()), 1);
        assert_eq!(token_of(&state.backend().unwrap().binding), 9);
        assert!(state.backend().unwrap().binding.is_parking());
        assert_eq!(
            state.lifecycle(),
            Lifecycle::Quiescent(DrainReason::ModeChange)
        );
        assert!(matches!(
            parked.events[0],
            Some(Event::PlaneUnbind {
                object_generation: 101,
                binding_sequence: 2,
                ..
            })
        ));
        assert!(parked.events[1].is_none());
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn parking_pre_wire_and_fenced_failures_have_no_real_plane_events() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.begin_drain(DrainReason::PowerTransition);

        let retained = state.retain_parking_candidate(binding(9), 2);
        assert_eq!(retained.events, [None; 3]);
        let cancelled = state.cancel_candidate();
        assert_eq!(cancelled.effect, Effect::ParkingReplacementFailed);
        assert_eq!(token_of(cancelled.release_candidate.as_ref().unwrap()), 9);
        assert_eq!(cancelled.events, [None; 3]);

        state.retain_parking_candidate(binding(9), 3);
        state.submit_candidate(key(30, 3));

        let failed = state.complete_replacement(key(30, 3), false);
        assert_eq!(failed.effect, Effect::ParkingReplacementFailed);
        assert_eq!(token_of(failed.release_candidate.as_ref().unwrap()), 9);
        assert!(failed.release_backend.is_none());
        assert_eq!(failed.events, [None; 3]);
        assert_eq!(token_of(&state.backend().unwrap().binding), 1);
        assert_eq!(
            state.lifecycle(),
            Lifecycle::Draining(DrainReason::PowerTransition)
        );
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn set_zero_success_failure_and_stale_response_never_release_parking() {
        for success in [false, true] {
            let mut state = state();
            install_real(&mut state, 1, 1);
            state.begin_drain(DrainReason::ExplicitUnbind);
            state.retain_parking_candidate(binding(9), 2);
            state.submit_candidate(key(20, 2));
            state.complete_replacement(key(20, 2), true);

            assert_eq!(
                state.submit_disable_zero(key(30, 3)).effect,
                Effect::DisableZeroSubmitted
            );
            assert_eq!(state.resume().refusal, Some(Refusal::PendingBusy));
            assert_eq!(
                state.complete_disable_zero(key(31, 3), success).refusal,
                Some(Refusal::StaleCompletion)
            );
            assert_eq!(token_of(&state.backend().unwrap().binding), 9);
            assert_eq!(state.held_reference_count(), 1);

            let completed = state.complete_disable_zero(key(30, 3), success);
            assert_eq!(
                completed.effect,
                if success {
                    Effect::DisableZeroCompleted
                } else {
                    Effect::DisableZeroFailed
                }
            );
            assert!(completed.release_backend.is_none());
            assert_eq!(token_of(&state.backend().unwrap().binding), 9);
            assert_eq!(state.check_invariants(), Ok(()));
        }
    }

    #[test]
    fn set_zero_atomically_consumes_a_new_sequence_and_fence() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.begin_drain(DrainReason::ExplicitUnbind);
        state.retain_parking_candidate(binding(9), 2);
        state.submit_candidate(key(20, 2));
        state.complete_replacement(key(20, 2), true);

        state.submit_disable_zero(key(30, 3));
        state.complete_disable_zero(key(30, 3), false);
        assert_eq!(state.binding_sequence_high_water(), 3);
        assert_eq!(state.fence_id_high_water(), 30);

        assert_eq!(
            state.submit_disable_zero(key(40, 3)).refusal,
            Some(Refusal::BindingSequenceReused { value: 3 })
        );
        assert_eq!(
            state.submit_disable_zero(key(30, 4)).refusal,
            Some(Refusal::FenceIdReused { value: 30 })
        );
        assert_eq!(state.binding_sequence_high_water(), 3);
        assert_eq!(state.fence_id_high_water(), 30);
        assert_eq!(state.pending_phase(), PendingPhase::None);

        assert_eq!(
            state.submit_disable_zero(key(40, 4)).effect,
            Effect::DisableZeroSubmitted
        );
        assert_eq!(state.binding_sequence_high_water(), 4);
        assert_eq!(state.fence_id_high_water(), 40);
        assert_eq!(token_of(&state.backend().unwrap().binding), 9);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn parking_is_released_only_by_the_next_successful_nonzero_replacement() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.begin_drain(DrainReason::SourceInvisible);
        state.retain_parking_candidate(binding(9), 2);
        state.submit_candidate(key(20, 2));
        state.complete_replacement(key(20, 2), true);
        assert_eq!(state.resume().effect, Effect::Resumed);

        state.retain_real_candidate(binding(2), 3);
        state.submit_candidate(key(30, 3));
        let failed = state.complete_replacement(key(30, 3), false);
        assert_eq!(token_of(failed.release_candidate.as_ref().unwrap()), 2);
        assert_eq!(token_of(&state.backend().unwrap().binding), 9);

        state.retain_real_candidate(binding(3), 4);
        state.submit_candidate(key(40, 4));
        let latched = state.complete_replacement(key(40, 4), true);
        assert_eq!(token_of(latched.release_backend.as_ref().unwrap()), 9);
        assert_eq!(token_of(&state.backend().unwrap().binding), 3);
        assert!(matches!(latched.events[0], Some(Event::PlaneLatch { .. })));
        assert!(latched.events[1].is_none());
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn drain_during_inflight_bind_resolves_it_before_parking() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);
        state.submit_candidate(key(20, 2));
        assert_eq!(
            state.begin_drain(DrainReason::DwmRestart).effect,
            Effect::DrainPending
        );

        let replacement = state.complete_replacement(key(20, 2), true);
        assert_eq!(replacement.effect, Effect::NeedParkingReplacement);
        assert_eq!(token_of(replacement.release_backend.as_ref().unwrap()), 1);
        assert_eq!(token_of(&state.backend().unwrap().binding), 2);
        assert_eq!(
            state.lifecycle(),
            Lifecycle::Draining(DrainReason::DwmRestart)
        );

        state.retain_parking_candidate(binding(9), 3);
        state.submit_candidate(key(30, 3));
        let parked = state.complete_replacement(key(30, 3), true);
        assert_eq!(token_of(parked.release_backend.as_ref().unwrap()), 2);
        assert_eq!(token_of(&state.backend().unwrap().binding), 9);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn failed_inflight_bind_during_drain_parks_the_surviving_current() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);
        state.submit_candidate(key(20, 2));
        state.begin_drain(DrainReason::AdapterStop);

        let failed = state.complete_replacement(key(20, 2), false);
        assert_eq!(failed.effect, Effect::NeedParkingReplacement);
        assert_eq!(token_of(failed.release_candidate.as_ref().unwrap()), 2);
        assert_eq!(token_of(&state.backend().unwrap().binding), 1);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn every_drain_reason_reaches_the_same_empty_quiescent_state() {
        let reasons = [
            DrainReason::ExplicitUnbind,
            DrainReason::SourceInvisible,
            DrainReason::ModeChange,
            DrainReason::PowerTransition,
            DrainReason::DwmRestart,
            DrainReason::AdapterStop,
            DrainReason::AllocationDestroyed,
        ];
        for reason in reasons {
            let mut state = state();
            assert_eq!(state.begin_drain(reason).effect, Effect::DrainComplete);
            assert_eq!(state.lifecycle(), Lifecycle::Quiescent(reason));
            assert_eq!(state.begin_drain(reason).effect, Effect::NoChange);
            assert_eq!(state.resume().effect, Effect::Resumed);
            assert_eq!(state.plane_generation(), 2);
            assert_eq!(state.check_invariants(), Ok(()));
        }
    }

    #[test]
    fn reset_barrier_releases_current_and_inflight_candidate_once() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);
        state.submit_candidate(key(20, 2));

        let reset = state.complete_reset_barrier();
        assert_eq!(reset.effect, Effect::ResetComplete);
        assert_eq!(token_of(reset.release_candidate.as_ref().unwrap()), 2);
        assert_eq!(token_of(reset.release_backend.as_ref().unwrap()), 1);
        assert!(matches!(reset.events[0], Some(Event::PlaneCancel { .. })));
        assert!(matches!(reset.events[1], Some(Event::PlaneUnbind { .. })));
        assert_eq!(reset.events[2], Some(Event::DeviceReset));
        assert_eq!(state.held_reference_count(), 0);
        assert_eq!(state.transport_epoch(), 2);
        assert_eq!(state.binding_sequence_high_water(), 0);
        assert_eq!(state.fence_id_high_water(), 0);
        assert_eq!(state.lifecycle(), Lifecycle::ResetQuiescent);

        let late = state.complete_replacement(key(20, 2), true);
        assert_eq!(
            late.refusal,
            Some(Refusal::TransportEpochMismatch {
                expected: 2,
                found: 1
            })
        );
        assert_eq!(state.resume().effect, Effect::Resumed);
        assert_eq!(
            state.retain_real_candidate(binding(3), 1).effect,
            Effect::CandidateRetained
        );
        assert_eq!(
            state.submit_candidate(CompletionKey::new(2, 1, 1)).effect,
            Effect::CandidateSubmitted
        );
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn parking_barriers_release_ownership_without_real_plane_events() {
        for reset in [false, true] {
            for submitted in [false, true] {
                let mut state = state();
                install_real(&mut state, 1, 1);
                state.begin_drain(DrainReason::ExplicitUnbind);
                let retained = state.retain_parking_candidate(binding(9), 2);
                assert_eq!(retained.events, [None; 3]);
                if submitted {
                    state.submit_candidate(key(20, 2));
                }

                let barrier = if reset {
                    state.complete_reset_barrier()
                } else {
                    state.complete_removal_barrier()
                };
                assert_eq!(token_of(barrier.release_candidate.as_ref().unwrap()), 9);
                assert_eq!(token_of(barrier.release_backend.as_ref().unwrap()), 1);
                assert!(matches!(
                    barrier.events[0],
                    Some(Event::PlaneUnbind {
                        object_generation: 101,
                        binding_sequence: 1,
                        ..
                    })
                ));
                assert_eq!(
                    barrier.events[1],
                    Some(if reset {
                        Event::DeviceReset
                    } else {
                        Event::DeviceRemoval
                    })
                );
                assert!(barrier.events[2].is_none());
                assert_eq!(state.held_reference_count(), 0);
                assert_eq!(state.check_invariants(), Ok(()));
            }
        }
    }

    #[test]
    fn reset_and_removal_barriers_clear_inflight_set_zero() {
        for reset in [false, true] {
            let mut state = state();
            install_real(&mut state, 1, 1);
            state.begin_drain(DrainReason::ExplicitUnbind);
            state.retain_parking_candidate(binding(9), 2);
            state.submit_candidate(key(20, 2));
            state.complete_replacement(key(20, 2), true);
            let disable = key(30, 3);
            state.submit_disable_zero(disable);

            let barrier = if reset {
                state.complete_reset_barrier()
            } else {
                state.complete_removal_barrier()
            };
            assert_eq!(token_of(barrier.release_backend.as_ref().unwrap()), 9);
            assert!(barrier.release_candidate.is_none());
            assert_eq!(state.pending_phase(), PendingPhase::None);
            assert_eq!(state.held_reference_count(), 0);

            if reset {
                assert_eq!(barrier.effect, Effect::ResetComplete);
                assert_eq!(state.transport_epoch(), 2);
                assert_eq!(state.binding_sequence_high_water(), 0);
                assert_eq!(state.fence_id_high_water(), 0);
                assert_eq!(
                    state.complete_disable_zero(disable, true).refusal,
                    Some(Refusal::TransportEpochMismatch {
                        expected: 2,
                        found: 1,
                    })
                );
            } else {
                assert_eq!(barrier.effect, Effect::RemovalComplete);
                assert_eq!(state.binding_sequence_high_water(), 3);
                assert_eq!(state.fence_id_high_water(), 30);
                assert_eq!(
                    state.complete_disable_zero(disable, true).refusal,
                    Some(Refusal::NoCompletionPending)
                );
            }
            assert_eq!(state.check_invariants(), Ok(()));
        }
    }

    #[test]
    fn reset_epoch_exhaustion_poisoned_state_never_reopens() {
        let mut state = PlaneState::<Token>::new(u64::MAX, u64::MAX).unwrap();
        let reset = state.complete_reset_barrier();
        assert_eq!(reset.effect, Effect::ResetPoisoned);
        assert_eq!(reset.refusal, Some(Refusal::PlaneGenerationExhausted));
        assert_eq!(state.lifecycle(), Lifecycle::Poisoned);
        let refused = state.retain_real_candidate(binding(1), 1);
        assert_eq!(refused.refusal, Some(Refusal::Poisoned));
        assert_eq!(token_of(refused.release_candidate.as_ref().unwrap()), 1);
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn removal_is_terminal_and_idempotent() {
        let mut state = state();
        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);
        state.submit_candidate(key(20, 2));
        let removed = state.complete_removal_barrier();
        assert_eq!(removed.effect, Effect::RemovalComplete);
        assert_eq!(token_of(removed.release_candidate.as_ref().unwrap()), 2);
        assert_eq!(token_of(removed.release_backend.as_ref().unwrap()), 1);
        assert!(matches!(removed.events[0], Some(Event::PlaneCancel { .. })));
        assert!(matches!(removed.events[1], Some(Event::PlaneUnbind { .. })));
        assert_eq!(removed.events[2], Some(Event::DeviceRemoval));
        assert_eq!(state.binding_sequence_high_water(), 2);
        assert_eq!(state.fence_id_high_water(), 20);
        assert_eq!(state.complete_removal_barrier().effect, Effect::NoChange);
        assert_eq!(state.resume().refusal, Some(Refusal::Removed));
        assert_eq!(state.check_invariants(), Ok(()));
    }

    #[test]
    fn event_mapping_is_exactly_the_protocol_plane_and_device_schema() {
        let events = [
            (
                Event::PlaneCandidate {
                    object_generation: 1,
                    plane_generation: 2,
                    binding_sequence: 3,
                },
                HeliosEtwEventId::PlaneCandidate,
                HELIOS_ETW_SUBKIND_SOLE,
            ),
            (
                Event::PlaneLatch {
                    object_generation: 1,
                    plane_generation: 2,
                    binding_sequence: 3,
                },
                HeliosEtwEventId::PlaneLatchOrCancel,
                HELIOS_ETW_SUBKIND_PLANE_LATCH,
            ),
            (
                Event::PlaneCancel {
                    object_generation: 1,
                    plane_generation: 2,
                    binding_sequence: 3,
                },
                HeliosEtwEventId::PlaneLatchOrCancel,
                HELIOS_ETW_SUBKIND_PLANE_CANCEL,
            ),
            (
                Event::PlaneReaderRelease {
                    object_generation: 1,
                    plane_generation: 2,
                    binding_sequence: 3,
                },
                HeliosEtwEventId::PlaneReaderReleaseOrUnbind,
                HELIOS_ETW_SUBKIND_PLANE_READER_RELEASE,
            ),
            (
                Event::PlaneUnbind {
                    object_generation: 1,
                    plane_generation: 2,
                    binding_sequence: 3,
                },
                HeliosEtwEventId::PlaneReaderReleaseOrUnbind,
                HELIOS_ETW_SUBKIND_PLANE_UNBIND,
            ),
            (
                Event::DeviceReset,
                HeliosEtwEventId::DeviceResetOrRemoval,
                HELIOS_ETW_SUBKIND_DEVICE_RESET,
            ),
            (
                Event::DeviceRemoval,
                HeliosEtwEventId::DeviceResetOrRemoval,
                HELIOS_ETW_SUBKIND_DEVICE_REMOVAL,
            ),
        ];
        for (event, id, subkind) in events {
            assert_eq!(event.etw_event_id(), id);
            assert_eq!(event.etw_subkind(), subkind);
        }
    }

    #[test]
    fn high_water_invariants_reject_impossible_history() {
        let mut fence_without_sequence = state();
        fence_without_sequence.fence_id_high_water = 1;
        assert_eq!(
            fence_without_sequence.check_invariants(),
            Err(InvariantViolation::FenceWithoutBindingSequence)
        );

        let mut backend_ahead = state();
        install_real(&mut backend_ahead, 1, 1);
        backend_ahead.binding_sequence_high_water = 0;
        backend_ahead.fence_id_high_water = 0;
        assert_eq!(
            backend_ahead.check_invariants(),
            Err(InvariantViolation::BackendSequenceBeyondHighWater)
        );

        let mut candidate_mismatch = state();
        candidate_mismatch.retain_real_candidate(binding(1), 2);
        candidate_mismatch.binding_sequence_high_water = 3;
        assert_eq!(
            candidate_mismatch.check_invariants(),
            Err(InvariantViolation::PendingSequenceNotHighWater)
        );

        let mut fence_mismatch = state();
        fence_mismatch.retain_real_candidate(binding(1), 1);
        fence_mismatch.submit_candidate(key(10, 1));
        fence_mismatch.fence_id_high_water = 11;
        assert_eq!(
            fence_mismatch.check_invariants(),
            Err(InvariantViolation::PendingFenceNotHighWater)
        );

        let mut reset_history = state();
        reset_history.complete_reset_barrier();
        reset_history.binding_sequence_high_water = 1;
        assert_eq!(
            reset_history.check_invariants(),
            Err(InvariantViolation::ResetQuiescentOwnsHistory)
        );
    }

    #[test]
    fn owned_reference_conservation_holds_across_mixed_lifetime() {
        let accepted = [1, 2, 3, 4, 9];
        let mut released = [0; 5];
        let mut released_len = 0;
        let mut state = state();

        install_real(&mut state, 1, 1);
        state.retain_real_candidate(binding(2), 2);
        state.submit_candidate(key(20, 2));
        collect_releases(
            state.complete_replacement(key(20, 2), false),
            &mut released,
            &mut released_len,
        );

        state.begin_drain(DrainReason::ModeChange);
        state.retain_parking_candidate(binding(9), 3);
        state.submit_candidate(key(30, 3));
        collect_releases(
            state.complete_replacement(key(30, 3), true),
            &mut released,
            &mut released_len,
        );
        state.submit_disable_zero(key(40, 4));
        collect_releases(
            state.complete_disable_zero(key(40, 4), true),
            &mut released,
            &mut released_len,
        );
        state.resume();

        state.retain_real_candidate(binding(3), 5);
        state.submit_candidate(key(50, 5));
        collect_releases(
            state.complete_replacement(key(50, 5), true),
            &mut released,
            &mut released_len,
        );
        state.retain_real_candidate(binding(4), 6);
        state.submit_candidate(key(60, 6));
        collect_releases(
            state.complete_reset_barrier(),
            &mut released,
            &mut released_len,
        );

        assert_eq!(released_len, released.len());
        released[..released_len].sort_unstable();
        assert_eq!(released, accepted);
        assert_eq!(state.held_reference_count(), 0);
        assert_eq!(state.check_invariants(), Ok(()));
    }
}

#[cfg(test)]
mod refusal_code_tests {
    use super::*;

    #[test]
    fn refusal_codes_are_distinct_and_dense() {
        const ALL: &[Refusal] = &[
            Refusal::ZeroPlaneGeneration,
            Refusal::ZeroTransportEpoch,
            Refusal::ZeroObjectGeneration,
            Refusal::ZeroBindingSequence,
            Refusal::ZeroFenceId,
            Refusal::BindingSequenceReused { value: 0 },
            Refusal::BindingSequenceWentBackward { high_water: 0, found: 0 },
            Refusal::BindingSequenceExhausted,
            Refusal::FenceIdReused { value: 0 },
            Refusal::FenceIdWentBackward { high_water: 0, found: 0 },
            Refusal::FenceIdExhausted,
            Refusal::TransportEpochMismatch { expected: 0, found: 0 },
            Refusal::BindingSequenceMismatch { expected: 0, found: 0 },
            Refusal::PendingBusy,
            Refusal::NoCandidate,
            Refusal::WrongPendingKind,
            Refusal::NotActive,
            Refusal::NotDraining,
            Refusal::NotQuiescent,
            Refusal::NoBackendToPark,
            Refusal::BackendNotParking,
            Refusal::NoCompletionPending,
            Refusal::StaleCompletion,
            Refusal::PlaneGenerationExhausted,
            Refusal::TransportEpochExhausted,
            Refusal::Poisoned,
            Refusal::Removed,
            Refusal::EventCapacityExceeded,
        ];
        assert_eq!(ALL.len(), COUNT, "add the new Refusal to ALL");
        let mut seen = [false; COUNT + 1];
        for r in ALL {
            let c = refusal_code(*r) as usize;
            assert!((1..=COUNT).contains(&c), "code {c} out of range");
            assert!(!seen[c], "duplicate code {c} for {r:?}");
            seen[c] = true;
        }
    }

    const COUNT: usize = 28;

    #[test]
    fn drain_reason_codes_are_dense_and_distinct() {
        const ALL: &[DrainReason] = &[
            DrainReason::ExplicitUnbind,
            DrainReason::SourceInvisible,
            DrainReason::ModeChange,
            DrainReason::PowerTransition,
            DrainReason::DwmRestart,
            DrainReason::AdapterStop,
            DrainReason::AllocationDestroyed,
        ];
        assert_eq!(ALL.len(), DRAIN_COUNT, "add the new DrainReason to ALL");
        let mut seen = [false; DRAIN_COUNT + 1];
        for r in ALL {
            let c = drain_reason_code(*r) as usize;
            assert!((1..=DRAIN_COUNT).contains(&c), "code {c} out of range");
            assert!(!seen[c], "duplicate code {c} for {r:?}");
            seen[c] = true;
        }
    }

    const DRAIN_COUNT: usize = 7;
}
