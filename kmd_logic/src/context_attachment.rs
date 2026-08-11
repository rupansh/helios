use core::mem::ManuallyDrop;
use core::num::NonZeroU64;

use crate::context_lifecycle::{
    ContextLease, LeasedAttachmentReservation, ReleasedAttachmentLease,
    ReleasedAttachmentReservation,
};
use crate::control_ownership::{
    AbandonReason, ClassifiedControl, ControlOutcome, ControlSubject, ControlVerb, HostRejection,
    PreparedControl, TransportAttachment, TransportDomainId, TransportEpoch, TransportReset,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentPhase {
    AttachPending,
    Attached,
    RetirementRequired,
    DetachPending,
    Released,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentOperation {
    FinishAttach,
    BeginDetach,
    FinishDetach,
    TransportReset,
    ConsumeReleased,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentRefusal {
    DomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    EpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    ResourceMismatch {
        expected: u32,
        found: u32,
    },
    ContextMismatch {
        expected: u32,
        found: u32,
    },
    AttachmentInstanceMismatch {
        expected: u64,
        found: u64,
    },
    ResetDomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    ResetEpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    WrongPhase {
        operation: AttachmentOperation,
        phase: AttachmentPhase,
    },
    MissingPendingControl,
    ControlAlreadyPending,
    ControlVerbMismatch {
        expected: ControlVerb,
        found: ControlVerb,
    },
    ControlSubjectMismatch,
    ControlSequenceMismatch {
        expected: u64,
        found: u64,
    },
    ControlSequenceExhausted,
    ContextLeaseAttachmentMismatch,
    AlreadyReleased,
    ReleaseAuthorityMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentBeginEffect {
    DetachStarted,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct AttachmentBegin {
    effect: AttachmentBeginEffect,
    request: PreparedControl,
}

impl AttachmentBegin {
    pub const fn effect(&self) -> AttachmentBeginEffect {
        self.effect
    }

    pub const fn request(&self) -> &PreparedControl {
        &self.request
    }

    pub fn into_request(self) -> PreparedControl {
        self.request
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum AttachmentFinishEffect<E> {
    AttachDefiniteNotEnqueued(E),
    AttachCompleted,
    AttachHostRejected(HostRejection),
    AttachAmbiguous(AbandonReason),
    DetachDefiniteNotEnqueued(E),
    DetachCompleted,
    DetachHostRejected(HostRejection),
    DetachAmbiguous(AbandonReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentReleaseAuthorityKind {
    AttachDefiniteNotEnqueued,
    AttachHostRejected(HostRejection),
    DetachCompleted,
    TransportReset(TransportEpoch),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleaseAttachment {
    attachment: TransportAttachment,
    authority: AttachmentReleaseAuthorityKind,
}

impl ReleaseAttachment {
    pub const fn attachment(&self) -> TransportAttachment {
        self.attachment
    }

    pub const fn authority(&self) -> AttachmentReleaseAuthorityKind {
        self.authority
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct AttachmentFinish<E> {
    effect: AttachmentFinishEffect<E>,
    release: Option<ReleaseAttachment>,
}

impl<E> AttachmentFinish<E> {
    pub const fn effect(&self) -> &AttachmentFinishEffect<E> {
        &self.effect
    }

    pub const fn release_authority(&self) -> Option<&ReleaseAttachment> {
        self.release.as_ref()
    }

    pub fn into_parts(self) -> (AttachmentFinishEffect<E>, Option<ReleaseAttachment>) {
        (self.effect, self.release)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedAttachmentOutcome<T, E> {
    reason: AttachmentRefusal,
    completion: ClassifiedControl<T, E>,
}

impl<T, E> RefusedAttachmentOutcome<T, E> {
    pub const fn reason(&self) -> AttachmentRefusal {
        self.reason
    }

    pub fn into_completion(self) -> ClassifiedControl<T, E> {
        self.completion
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct AttachmentAdmission<R> {
    lifecycle: ContextAttachmentLifecycle<R>,
    request: PreparedControl,
}

impl<R> AttachmentAdmission<R> {
    pub const fn lifecycle(&self) -> &ContextAttachmentLifecycle<R> {
        &self.lifecycle
    }

    pub const fn request(&self) -> &PreparedControl {
        &self.request
    }

    pub fn into_parts(self) -> (ContextAttachmentLifecycle<R>, PreparedControl) {
        (self.lifecycle, self.request)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedAttachmentAdmission<R> {
    reason: AttachmentRefusal,
    leased: LeasedAttachmentReservation,
    association_lease: R,
}

impl<R> RefusedAttachmentAdmission<R> {
    pub const fn reason(&self) -> AttachmentRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (LeasedAttachmentReservation, R) {
        (self.leased, self.association_lease)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingControl {
    verb: ControlVerb,
    sequence: NonZeroU64,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ContextAttachmentLifecycle<R> {
    leased: ManuallyDrop<LeasedAttachmentReservation>,
    phase: AttachmentPhase,
    uncertain: bool,
    control_high_water: u64,
    pending: Option<PendingControl>,
    release_authority: Option<AttachmentReleaseAuthorityKind>,
    association_lease: ManuallyDrop<R>,
}

impl<R> ContextAttachmentLifecycle<R> {
    pub fn reserve(
        leased: LeasedAttachmentReservation,
        association_lease: R,
    ) -> Result<AttachmentAdmission<R>, RefusedAttachmentAdmission<R>> {
        if !leased.is_exact() {
            return Err(RefusedAttachmentAdmission {
                reason: AttachmentRefusal::ContextLeaseAttachmentMismatch,
                leased,
                association_lease,
            });
        }
        let attachment = leased.attachment();
        let pending = PendingControl {
            verb: ControlVerb::Attach,
            sequence: NonZeroU64::MIN,
        };
        Ok(AttachmentAdmission {
            lifecycle: Self {
                leased: ManuallyDrop::new(leased),
                phase: AttachmentPhase::AttachPending,
                uncertain: false,
                control_high_water: 1,
                pending: Some(pending),
                release_authority: None,
                association_lease: ManuallyDrop::new(association_lease),
            },
            request: PreparedControl::from_parts(
                pending.verb,
                ControlSubject::Attachment(attachment),
                pending.sequence,
            ),
        })
    }

    pub fn attachment(&self) -> TransportAttachment {
        self.leased.attachment()
    }

    pub const fn phase(&self) -> AttachmentPhase {
        self.phase
    }

    pub const fn is_uncertain(&self) -> bool {
        self.uncertain
    }

    pub const fn control_high_water(&self) -> u64 {
        self.control_high_water
    }

    pub fn finish_attach<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<AttachmentFinish<E>, RefusedAttachmentOutcome<(), E>> {
        if let Err(reason) = self.check_finish(
            AttachmentOperation::FinishAttach,
            AttachmentPhase::AttachPending,
            ControlVerb::Attach,
            &completion,
        ) {
            return Err(RefusedAttachmentOutcome { reason, completion });
        }
        self.pending = None;

        let (effect, authority) = match completion.into_outcome() {
            ControlOutcome::DefiniteNotEnqueued(error) => (
                AttachmentFinishEffect::AttachDefiniteNotEnqueued(error),
                AttachmentReleaseAuthorityKind::AttachDefiniteNotEnqueued,
            ),
            ControlOutcome::Completed(Ok(())) => {
                self.phase = AttachmentPhase::Attached;
                return Ok(AttachmentFinish {
                    effect: AttachmentFinishEffect::AttachCompleted,
                    release: None,
                });
            }
            ControlOutcome::Completed(Err(status)) => (
                AttachmentFinishEffect::AttachHostRejected(status),
                AttachmentReleaseAuthorityKind::AttachHostRejected(status),
            ),
            ControlOutcome::Ambiguous(abandoned) => {
                self.phase = AttachmentPhase::RetirementRequired;
                self.uncertain = true;
                return Ok(AttachmentFinish {
                    effect: AttachmentFinishEffect::AttachAmbiguous(abandoned.reason()),
                    release: None,
                });
            }
        };
        self.release(authority);
        Ok(AttachmentFinish {
            effect,
            release: Some(self.release_token(authority)),
        })
    }

    pub fn begin_detach(
        &mut self,
        attachment: TransportAttachment,
    ) -> Result<AttachmentBegin, AttachmentRefusal> {
        self.check_attachment(attachment)?;
        match self.phase {
            AttachmentPhase::Attached | AttachmentPhase::RetirementRequired => {}
            AttachmentPhase::Released => return Err(AttachmentRefusal::AlreadyReleased),
            phase => {
                return Err(AttachmentRefusal::WrongPhase {
                    operation: AttachmentOperation::BeginDetach,
                    phase,
                });
            }
        }
        let request = self.mint_control(ControlVerb::Detach)?;
        self.phase = AttachmentPhase::DetachPending;
        Ok(AttachmentBegin {
            effect: AttachmentBeginEffect::DetachStarted,
            request,
        })
    }

    pub fn finish_detach<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<AttachmentFinish<E>, RefusedAttachmentOutcome<(), E>> {
        if let Err(reason) = self.check_finish(
            AttachmentOperation::FinishDetach,
            AttachmentPhase::DetachPending,
            ControlVerb::Detach,
            &completion,
        ) {
            return Err(RefusedAttachmentOutcome { reason, completion });
        }
        self.pending = None;

        let effect = match completion.into_outcome() {
            ControlOutcome::DefiniteNotEnqueued(error) => {
                AttachmentFinishEffect::DetachDefiniteNotEnqueued(error)
            }
            ControlOutcome::Completed(Ok(())) => {
                let authority = AttachmentReleaseAuthorityKind::DetachCompleted;
                self.release(authority);
                return Ok(AttachmentFinish {
                    effect: AttachmentFinishEffect::DetachCompleted,
                    release: Some(self.release_token(authority)),
                });
            }
            ControlOutcome::Completed(Err(status)) => {
                AttachmentFinishEffect::DetachHostRejected(status)
            }
            ControlOutcome::Ambiguous(abandoned) => {
                self.uncertain = true;
                AttachmentFinishEffect::DetachAmbiguous(abandoned.reason())
            }
        };
        self.phase = AttachmentPhase::RetirementRequired;
        Ok(AttachmentFinish {
            effect,
            release: None,
        })
    }

    pub fn transport_reset(
        &mut self,
        attachment: TransportAttachment,
        reset: &TransportReset,
    ) -> Result<ReleaseAttachment, AttachmentRefusal> {
        self.check_attachment(attachment)?;
        if reset.retired_epoch().domain() != self.attachment().epoch().domain() {
            return Err(AttachmentRefusal::ResetDomainMismatch {
                expected: self.attachment().epoch().domain(),
                found: reset.retired_epoch().domain(),
            });
        }
        if reset.retired_epoch() != self.attachment().epoch() {
            return Err(AttachmentRefusal::ResetEpochMismatch {
                expected: self.attachment().epoch(),
                found: reset.retired_epoch(),
            });
        }
        if self.phase == AttachmentPhase::Released {
            return Err(AttachmentRefusal::AlreadyReleased);
        }
        let authority = AttachmentReleaseAuthorityKind::TransportReset(reset.retired_epoch());
        self.release(authority);
        Ok(self.release_token(authority))
    }

    pub fn consume_released(
        self,
        authority: ReleaseAttachment,
    ) -> Result<ReleasedAttachment<R>, RefusedReleaseAttachment<R>> {
        let reason = self
            .check_attachment(authority.attachment)
            .and_then(|()| {
                if self.phase != AttachmentPhase::Released {
                    Err(AttachmentRefusal::WrongPhase {
                        operation: AttachmentOperation::ConsumeReleased,
                        phase: self.phase,
                    })
                } else if self.release_authority != Some(authority.authority) {
                    Err(AttachmentRefusal::ReleaseAuthorityMismatch)
                } else {
                    Ok(())
                }
            })
            .err();
        if let Some(reason) = reason {
            return Err(RefusedReleaseAttachment {
                reason,
                lifecycle: self,
                authority,
            });
        }
        // SAFETY: the matching release authority above proves this row terminal.
        let attachment_lease = unsafe { ManuallyDrop::into_inner(self.leased).into_released() };
        Ok(ReleasedAttachment {
            attachment_lease,
            association_lease: ManuallyDrop::into_inner(self.association_lease),
        })
    }

    fn check_finish<T, E>(
        &self,
        operation: AttachmentOperation,
        expected_phase: AttachmentPhase,
        expected_verb: ControlVerb,
        completion: &ClassifiedControl<T, E>,
    ) -> Result<(), AttachmentRefusal> {
        if self.phase == AttachmentPhase::Released {
            return Err(AttachmentRefusal::AlreadyReleased);
        }
        if self.phase != expected_phase {
            return Err(AttachmentRefusal::WrongPhase {
                operation,
                phase: self.phase,
            });
        }
        self.check_control(expected_verb, completion)
    }

    fn check_control<T, E>(
        &self,
        expected_verb: ControlVerb,
        completion: &ClassifiedControl<T, E>,
    ) -> Result<(), AttachmentRefusal> {
        let Some(pending) = self.pending else {
            return Err(AttachmentRefusal::MissingPendingControl);
        };
        if pending.verb != expected_verb {
            return Err(AttachmentRefusal::ControlVerbMismatch {
                expected: expected_verb,
                found: pending.verb,
            });
        }
        let ControlSubject::Attachment(found) = completion.subject() else {
            return Err(AttachmentRefusal::ControlSubjectMismatch);
        };
        self.check_attachment(found)?;
        if completion.verb() != pending.verb {
            return Err(AttachmentRefusal::ControlVerbMismatch {
                expected: pending.verb,
                found: completion.verb(),
            });
        }
        if completion.sequence() != pending.sequence.get() {
            return Err(AttachmentRefusal::ControlSequenceMismatch {
                expected: pending.sequence.get(),
                found: completion.sequence(),
            });
        }
        Ok(())
    }

    fn check_attachment(&self, found: TransportAttachment) -> Result<(), AttachmentRefusal> {
        let expected = self.attachment();
        if found.epoch().domain() != expected.epoch().domain() {
            return Err(AttachmentRefusal::DomainMismatch {
                expected: expected.epoch().domain(),
                found: found.epoch().domain(),
            });
        }
        if found.epoch() != expected.epoch() {
            return Err(AttachmentRefusal::EpochMismatch {
                expected: expected.epoch(),
                found: found.epoch(),
            });
        }
        if found.resource().id() != expected.resource().id() {
            return Err(AttachmentRefusal::ResourceMismatch {
                expected: expected.resource().id(),
                found: found.resource().id(),
            });
        }
        if found.context().id() != expected.context().id() {
            return Err(AttachmentRefusal::ContextMismatch {
                expected: expected.context().id(),
                found: found.context().id(),
            });
        }
        if found.instance() != expected.instance() {
            return Err(AttachmentRefusal::AttachmentInstanceMismatch {
                expected: expected.instance(),
                found: found.instance(),
            });
        }
        Ok(())
    }

    fn mint_control(&mut self, verb: ControlVerb) -> Result<PreparedControl, AttachmentRefusal> {
        if self.pending.is_some() {
            return Err(AttachmentRefusal::ControlAlreadyPending);
        }
        let Some(raw) = self.control_high_water.checked_add(1) else {
            return Err(AttachmentRefusal::ControlSequenceExhausted);
        };
        let Some(sequence) = NonZeroU64::new(raw) else {
            return Err(AttachmentRefusal::ControlSequenceExhausted);
        };
        let pending = PendingControl { verb, sequence };
        self.control_high_water = raw;
        self.pending = Some(pending);
        Ok(PreparedControl::from_parts(
            pending.verb,
            ControlSubject::Attachment(self.attachment()),
            pending.sequence,
        ))
    }

    fn release(&mut self, authority: AttachmentReleaseAuthorityKind) {
        self.phase = AttachmentPhase::Released;
        self.pending = None;
        self.release_authority = Some(authority);
    }

    fn release_token(&self, authority: AttachmentReleaseAuthorityKind) -> ReleaseAttachment {
        ReleaseAttachment {
            attachment: self.attachment(),
            authority,
        }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasedAttachment<R> {
    attachment_lease: ReleasedAttachmentLease,
    association_lease: R,
}

impl<R> ReleasedAttachment<R> {
    pub const fn reservation(&self) -> &ReleasedAttachmentReservation {
        self.attachment_lease.reservation()
    }

    pub const fn context_lease(&self) -> &ContextLease {
        self.attachment_lease.context_lease()
    }

    pub const fn association_lease(&self) -> &R {
        &self.association_lease
    }

    pub fn into_parts(self) -> (ReleasedAttachmentLease, R) {
        (self.attachment_lease, self.association_lease)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedReleaseAttachment<R> {
    reason: AttachmentRefusal,
    lifecycle: ContextAttachmentLifecycle<R>,
    authority: ReleaseAttachment,
}

impl<R> RefusedReleaseAttachment<R> {
    pub const fn reason(&self) -> AttachmentRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (ContextAttachmentLifecycle<R>, ReleaseAttachment) {
        (self.lifecycle, self.authority)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::cell::Cell;
    use self::std::rc::Rc;
    use core::num::NonZeroU64;

    use super::*;
    use crate::context_lifecycle::{
        ContextFinishEffect, ContextRefusal, TransportContextLifecycle,
    };
    use crate::control_ownership::{
        AttachmentReservation, AttachmentsClosed, ContextReservation, ResourceFinishEffect,
        ResourceLifecycle, TransportContext, TransportDomainId, TransportDomainRoot,
        TransportEpoch, TransportGeneration, TransportResource,
    };
    use helios_protocol::virtio_gpu::VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID;

    const DEFINITE_ERROR: u8 = 0xa5;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum OutcomeCase {
        DefiniteNotEnqueued,
        Completed,
        HostRejected,
        Timeout,
        NotOurs,
        TransportAborted,
        MalformedResponse,
    }

    const OUTCOMES: [OutcomeCase; 7] = [
        OutcomeCase::DefiniteNotEnqueued,
        OutcomeCase::Completed,
        OutcomeCase::HostRejected,
        OutcomeCase::Timeout,
        OutcomeCase::NotOurs,
        OutcomeCase::TransportAborted,
        OutcomeCase::MalformedResponse,
    ];

    #[derive(Debug)]
    struct DropToken {
        drops: Rc<Cell<usize>>,
    }

    impl DropToken {
        fn new(drops: &Rc<Cell<usize>>) -> Self {
            Self {
                drops: Rc::clone(drops),
            }
        }
    }

    impl Drop for DropToken {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    fn epoch(domain: u64, generation: u64) -> TransportEpoch {
        let domain = TransportDomainId::test_from_raw(domain).unwrap();
        TransportEpoch::test_from_raw(domain, generation).unwrap()
    }

    fn attachment(
        domain: u64,
        generation: u64,
        resource_id: u32,
        context_id: u32,
    ) -> TransportAttachment {
        attachment_with_instance(domain, generation, resource_id, context_id, 1)
    }

    fn attachment_with_instance(
        domain: u64,
        generation: u64,
        resource_id: u32,
        context_id: u32,
        instance: u64,
    ) -> TransportAttachment {
        let epoch = epoch(domain, generation);
        let resource = TransportResource::from_raw(epoch, resource_id).unwrap();
        let context = TransportContext::from_raw(epoch, context_id).unwrap();
        TransportAttachment::from_raw(resource, context, NonZeroU64::new(instance).unwrap())
    }

    fn reset(domain: u64, generation: u64) -> TransportReset {
        TransportReset::test_for_epoch(epoch(domain, generation))
    }

    fn host_error() -> HostRejection {
        HostRejection::from_response_type(VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID).unwrap()
    }

    fn admission<R>(
        attachment: TransportAttachment,
        lease: R,
    ) -> (ContextAttachmentLifecycle<R>, PreparedControl) {
        let Ok(admission) =
            ContextAttachmentLifecycle::reserve(leased_attachment(attachment, 1), lease)
        else {
            panic!("matching test lease must admit");
        };
        admission.into_parts()
    }

    fn leased_attachment(
        attachment: TransportAttachment,
        lease_id: u64,
    ) -> LeasedAttachmentReservation {
        let reservation = AttachmentReservation::test_for_attachment(attachment);
        let context_lease =
            ContextLease::test_for_attachment(attachment, NonZeroU64::new(lease_id).unwrap());
        LeasedAttachmentReservation::test_for_parts(reservation, context_lease)
    }

    fn consume_unit(
        lifecycle: ContextAttachmentLifecycle<()>,
        authority: ReleaseAttachment,
    ) -> ReleasedAttachmentLease {
        let released = lifecycle.consume_released(authority).unwrap();
        released.into_parts().0
    }

    fn attached<R>(attachment: TransportAttachment, lease: R) -> ContextAttachmentLifecycle<R> {
        let (mut lifecycle, request) = admission(attachment, lease);
        let finish = lifecycle
            .finish_attach(unsafe { request.completed_ok::<u8>() })
            .unwrap();
        assert_eq!(finish.effect(), &AttachmentFinishEffect::AttachCompleted);
        lifecycle
    }

    fn outcome(
        case: OutcomeCase,
        request: PreparedControl,
        epoch: TransportEpoch,
    ) -> ClassifiedControl<(), u8> {
        match case {
            OutcomeCase::DefiniteNotEnqueued => unsafe {
                request.definite_not_enqueued(DEFINITE_ERROR)
            },
            OutcomeCase::Completed => unsafe { request.completed_ok() },
            OutcomeCase::HostRejected => unsafe { request.completed_rejection(host_error()) },
            OutcomeCase::Timeout => request.ambiguous(epoch, AbandonReason::Timeout).unwrap(),
            OutcomeCase::NotOurs => request.ambiguous(epoch, AbandonReason::NotOurs).unwrap(),
            OutcomeCase::TransportAborted => request
                .ambiguous(epoch, AbandonReason::TransportAborted)
                .unwrap(),
            OutcomeCase::MalformedResponse => request
                .ambiguous(epoch, AbandonReason::MalformedResponse)
                .unwrap(),
        }
    }

    fn abandon_reason(case: OutcomeCase) -> Option<AbandonReason> {
        match case {
            OutcomeCase::Timeout => Some(AbandonReason::Timeout),
            OutcomeCase::NotOurs => Some(AbandonReason::NotOurs),
            OutcomeCase::TransportAborted => Some(AbandonReason::TransportAborted),
            OutcomeCase::MalformedResponse => Some(AbandonReason::MalformedResponse),
            _ => None,
        }
    }

    fn completed(
        verb: ControlVerb,
        subject: ControlSubject,
        sequence: u64,
    ) -> ClassifiedControl<(), u8> {
        let request =
            PreparedControl::from_parts(verb, subject, NonZeroU64::new(sequence).unwrap());
        unsafe { request.completed_ok() }
    }

    #[test]
    fn admission_refuses_mismatched_context_lease_and_recovers_both_inputs() {
        let reserved = attachment_with_instance(1, 6, 9, 11, 1);
        let leased_for = attachment_with_instance(1, 6, 9, 11, 2);
        let leased = LeasedAttachmentReservation::test_for_parts(
            AttachmentReservation::test_for_attachment(reserved),
            ContextLease::test_for_attachment(leased_for, NonZeroU64::MIN),
        );
        let drops = Rc::new(Cell::new(0));
        let refusal =
            ContextAttachmentLifecycle::reserve(leased, DropToken::new(&drops)).unwrap_err();
        assert_eq!(
            refusal.reason(),
            AttachmentRefusal::ContextLeaseAttachmentMismatch
        );
        let (leased, association_lease) = refusal.into_parts();
        assert_eq!(leased.attachment(), reserved);
        assert_eq!(leased.context_lease().attachment(), leased_for);
        assert_eq!(drops.get(), 0);
        drop(association_lease);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn attach_releases_only_when_exact_absence_is_proven() {
        let attachment = attachment(1, 7, 11, 13);
        for case in OUTCOMES {
            let (mut lifecycle, request) = admission(attachment, ());
            assert_eq!(request.verb(), ControlVerb::Attach);
            assert_eq!(request.subject(), ControlSubject::Attachment(attachment));
            assert_eq!(request.sequence(), 1);
            let finish = lifecycle
                .finish_attach(outcome(case, request, attachment.epoch()))
                .unwrap();
            match case {
                OutcomeCase::DefiniteNotEnqueued => {
                    assert_eq!(
                        finish.effect(),
                        &AttachmentFinishEffect::AttachDefiniteNotEnqueued(DEFINITE_ERROR)
                    );
                    let (_, Some(authority)) = finish.into_parts() else {
                        panic!("definite non-enqueue must release the lease");
                    };
                    assert_eq!(
                        authority.authority(),
                        AttachmentReleaseAuthorityKind::AttachDefiniteNotEnqueued
                    );
                    drop(consume_unit(lifecycle, authority));
                }
                OutcomeCase::HostRejected => {
                    assert_eq!(
                        finish.effect(),
                        &AttachmentFinishEffect::AttachHostRejected(host_error())
                    );
                    let (_, Some(authority)) = finish.into_parts() else {
                        panic!("an exact documented rejection must release the lease");
                    };
                    assert_eq!(
                        authority.authority(),
                        AttachmentReleaseAuthorityKind::AttachHostRejected(host_error())
                    );
                    drop(consume_unit(lifecycle, authority));
                }
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &AttachmentFinishEffect::AttachCompleted);
                    assert!(finish.release_authority().is_none());
                    assert_eq!(lifecycle.phase(), AttachmentPhase::Attached);
                    assert!(!lifecycle.is_uncertain());
                }
                _ => {
                    assert_eq!(
                        finish.effect(),
                        &AttachmentFinishEffect::AttachAmbiguous(abandon_reason(case).unwrap())
                    );
                    assert!(finish.release_authority().is_none());
                    assert_eq!(lifecycle.phase(), AttachmentPhase::RetirementRequired);
                    assert!(lifecycle.is_uncertain());
                    let request = lifecycle.begin_detach(attachment).unwrap().into_request();
                    let finish = lifecycle
                        .finish_detach(unsafe { request.completed_ok::<u8>() })
                        .unwrap();
                    let (_, Some(authority)) = finish.into_parts() else {
                        panic!("exact DETACH must release an ambiguous attachment");
                    };
                    drop(consume_unit(lifecycle, authority));
                }
            }
        }
    }

    #[test]
    fn detach_releases_only_on_exact_success_and_every_failure_retries() {
        let attachment = attachment(2, 9, 3, 4);
        for case in OUTCOMES {
            let mut lifecycle = attached(attachment, ());
            let request = lifecycle.begin_detach(attachment).unwrap().into_request();
            assert_eq!(request.sequence(), 2);
            let finish = lifecycle
                .finish_detach(outcome(case, request, attachment.epoch()))
                .unwrap();
            match case {
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &AttachmentFinishEffect::DetachCompleted);
                    let (_, Some(authority)) = finish.into_parts() else {
                        panic!("exact DETACH must release the lease");
                    };
                    drop(consume_unit(lifecycle, authority));
                    continue;
                }
                OutcomeCase::DefiniteNotEnqueued => assert_eq!(
                    finish.effect(),
                    &AttachmentFinishEffect::DetachDefiniteNotEnqueued(DEFINITE_ERROR)
                ),
                OutcomeCase::HostRejected => assert_eq!(
                    finish.effect(),
                    &AttachmentFinishEffect::DetachHostRejected(host_error())
                ),
                _ => assert_eq!(
                    finish.effect(),
                    &AttachmentFinishEffect::DetachAmbiguous(abandon_reason(case).unwrap())
                ),
            }
            if case != OutcomeCase::Completed {
                assert!(finish.release_authority().is_none());
                assert_eq!(lifecycle.phase(), AttachmentPhase::RetirementRequired);
                assert_eq!(lifecycle.is_uncertain(), abandon_reason(case).is_some());
                let retry = lifecycle.begin_detach(attachment).unwrap().into_request();
                assert_eq!(retry.sequence(), 3);
                let finish = lifecycle
                    .finish_detach(unsafe { retry.completed_ok::<u8>() })
                    .unwrap();
                let (_, Some(authority)) = finish.into_parts() else {
                    panic!("a successful retry must release the lease");
                };
                drop(consume_unit(lifecycle, authority));
            }
        }
    }

    #[derive(Clone, Copy)]
    enum ResetPhase {
        AttachPending,
        Attached,
        RetirementRequired,
        DetachPending,
    }

    fn lifecycle_at_phase(
        attachment: TransportAttachment,
        phase: ResetPhase,
    ) -> ContextAttachmentLifecycle<()> {
        match phase {
            ResetPhase::AttachPending => {
                let (lifecycle, request) = admission(attachment, ());
                drop(request);
                lifecycle
            }
            ResetPhase::Attached => attached(attachment, ()),
            ResetPhase::RetirementRequired => {
                let (mut lifecycle, request) = admission(attachment, ());
                let _ = lifecycle
                    .finish_attach(
                        request
                            .ambiguous::<u8>(attachment.epoch(), AbandonReason::Timeout)
                            .unwrap(),
                    )
                    .unwrap();
                lifecycle
            }
            ResetPhase::DetachPending => {
                let mut lifecycle = attached(attachment, ());
                let request = lifecycle.begin_detach(attachment).unwrap().into_request();
                drop(request);
                lifecycle
            }
        }
    }

    #[test]
    fn exact_transport_reset_releases_every_live_phase() {
        let attachment = attachment(3, 15, 6, 7);
        let reset = reset(3, 15);
        for phase in [
            ResetPhase::AttachPending,
            ResetPhase::Attached,
            ResetPhase::RetirementRequired,
            ResetPhase::DetachPending,
        ] {
            let mut lifecycle = lifecycle_at_phase(attachment, phase);
            let authority = lifecycle.transport_reset(attachment, &reset).unwrap();
            assert_eq!(lifecycle.phase(), AttachmentPhase::Released);
            assert_eq!(
                authority.authority(),
                AttachmentReleaseAuthorityKind::TransportReset(attachment.epoch())
            );
            drop(consume_unit(lifecycle, authority));
        }
    }

    #[test]
    fn foreign_identity_completion_and_reset_refusals_are_inert_and_recoverable() {
        let other_domain = attachment(5, 20, 8, 9);
        let other_epoch = attachment(4, 21, 8, 9);
        let other_resource = attachment(4, 20, 10, 9);
        let other_context = attachment(4, 20, 8, 11);
        let other_instance = attachment_with_instance(4, 20, 8, 9, 2);
        let attachment = attachment(4, 20, 8, 9);
        let (mut lifecycle, request) = admission(attachment, ());

        for (completion, expected) in [
            (
                completed(
                    ControlVerb::Attach,
                    ControlSubject::Attachment(other_domain),
                    request.sequence(),
                ),
                AttachmentRefusal::DomainMismatch {
                    expected: attachment.epoch().domain(),
                    found: other_domain.epoch().domain(),
                },
            ),
            (
                completed(
                    ControlVerb::Attach,
                    ControlSubject::Attachment(other_epoch),
                    request.sequence(),
                ),
                AttachmentRefusal::EpochMismatch {
                    expected: attachment.epoch(),
                    found: other_epoch.epoch(),
                },
            ),
            (
                completed(
                    ControlVerb::Attach,
                    ControlSubject::Attachment(other_resource),
                    request.sequence(),
                ),
                AttachmentRefusal::ResourceMismatch {
                    expected: attachment.resource().id(),
                    found: other_resource.resource().id(),
                },
            ),
            (
                completed(
                    ControlVerb::Attach,
                    ControlSubject::Attachment(other_context),
                    request.sequence(),
                ),
                AttachmentRefusal::ContextMismatch {
                    expected: attachment.context().id(),
                    found: other_context.context().id(),
                },
            ),
            (
                completed(
                    ControlVerb::Attach,
                    ControlSubject::Attachment(other_instance),
                    request.sequence(),
                ),
                AttachmentRefusal::AttachmentInstanceMismatch {
                    expected: attachment.instance(),
                    found: other_instance.instance(),
                },
            ),
            (
                completed(
                    ControlVerb::Attach,
                    ControlSubject::Resource(attachment.resource()),
                    request.sequence(),
                ),
                AttachmentRefusal::ControlSubjectMismatch,
            ),
            (
                completed(
                    ControlVerb::Detach,
                    ControlSubject::Attachment(attachment),
                    request.sequence(),
                ),
                AttachmentRefusal::ControlVerbMismatch {
                    expected: ControlVerb::Attach,
                    found: ControlVerb::Detach,
                },
            ),
            (
                completed(
                    ControlVerb::Attach,
                    ControlSubject::Attachment(attachment),
                    request.sequence() + 1,
                ),
                AttachmentRefusal::ControlSequenceMismatch {
                    expected: request.sequence(),
                    found: request.sequence() + 1,
                },
            ),
        ] {
            let refusal = lifecycle.finish_attach(completion).unwrap_err();
            assert_eq!(refusal.reason(), expected);
            let recovered = refusal.into_completion();
            assert!(matches!(
                recovered.into_outcome(),
                ControlOutcome::Completed(Ok(()))
            ));
            assert_eq!(lifecycle.phase(), AttachmentPhase::AttachPending);
        }

        let _ = lifecycle
            .finish_attach(unsafe { request.completed_ok::<u8>() })
            .unwrap();
        assert_eq!(
            lifecycle.begin_detach(other_context),
            Err(AttachmentRefusal::ContextMismatch {
                expected: attachment.context().id(),
                found: other_context.context().id(),
            })
        );
        assert_eq!(lifecycle.phase(), AttachmentPhase::Attached);
        assert_eq!(
            lifecycle.transport_reset(attachment, &reset(5, 20)),
            Err(AttachmentRefusal::ResetDomainMismatch {
                expected: attachment.epoch().domain(),
                found: other_domain.epoch().domain(),
            })
        );
        assert_eq!(
            lifecycle.transport_reset(attachment, &reset(4, 21)),
            Err(AttachmentRefusal::ResetEpochMismatch {
                expected: attachment.epoch(),
                found: other_epoch.epoch(),
            })
        );
        assert_eq!(lifecycle.phase(), AttachmentPhase::Attached);
        let authority = lifecycle
            .transport_reset(attachment, &reset(4, 20))
            .unwrap();
        drop(consume_unit(lifecycle, authority));
    }

    #[test]
    fn late_and_replayed_detach_completions_cannot_cross_a_retry() {
        let attachment = attachment(6, 30, 12, 14);
        let mut lifecycle = attached(attachment, ());
        let first = lifecycle.begin_detach(attachment).unwrap().into_request();
        let first_sequence = first.sequence();
        let _ = lifecycle
            .finish_detach(
                first
                    .ambiguous::<u8>(attachment.epoch(), AbandonReason::MalformedResponse)
                    .unwrap(),
            )
            .unwrap();
        let retry = lifecycle.begin_detach(attachment).unwrap().into_request();
        let retry_sequence = retry.sequence();
        let late = completed(
            ControlVerb::Detach,
            ControlSubject::Attachment(attachment),
            first_sequence,
        );
        let refusal = lifecycle.finish_detach(late).unwrap_err();
        assert_eq!(
            refusal.reason(),
            AttachmentRefusal::ControlSequenceMismatch {
                expected: retry_sequence,
                found: first_sequence,
            }
        );
        drop(refusal.into_completion());
        assert_eq!(lifecycle.phase(), AttachmentPhase::DetachPending);
        let finish = lifecycle
            .finish_detach(unsafe { retry.completed_ok::<u8>() })
            .unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("exact retry must release");
        };
        let replay = completed(
            ControlVerb::Detach,
            ControlSubject::Attachment(attachment),
            retry_sequence,
        );
        let refusal = lifecycle.finish_detach(replay).unwrap_err();
        assert_eq!(refusal.reason(), AttachmentRefusal::AlreadyReleased);
        drop(refusal.into_completion());
        drop(consume_unit(lifecycle, authority));
    }

    #[test]
    fn released_secondary_row_closes_resource_and_unref_retry_keeps_witness() {
        // SAFETY: this test domain is unique until every descendant below is gone.
        let root = unsafe { TransportDomainRoot::new(0xcaa0_0001).unwrap() };
        let generation = TransportGeneration::bootstrap(root);
        let allocation = generation.allocate_resource().unwrap();
        let resource = allocation.resource();
        let (generation, resource_reservation) = allocation.into_parts();
        let mut resource_lifecycle = ResourceLifecycle::new(resource_reservation, ());
        let create = resource_lifecycle
            .begin_create(resource)
            .unwrap()
            .into_request();
        let finish = resource_lifecycle
            .finish_create(unsafe { create.completed_ok::<u8>() })
            .unwrap();
        assert_eq!(finish.effect(), &ResourceFinishEffect::CreateCompleted);

        let allocation = generation.allocate_context().unwrap();
        let context = allocation.context();
        let (generation, context_reservation) = allocation.into_parts();
        let mut context_lifecycle = TransportContextLifecycle::new(context_reservation, ());
        let create = context_lifecycle
            .begin_create(context)
            .unwrap()
            .into_request();
        let finish = context_lifecycle
            .finish_create(unsafe { create.completed_nodata::<u8>() })
            .unwrap();
        assert_eq!(finish.effect(), &ContextFinishEffect::CreateCompleted);
        // SAFETY: the empty test table retains this exact context and resource
        // custody, and keeps the pair canonical until the terminal DETACH below.
        let allocation = unsafe { generation.allocate_attachment(resource, context) }.unwrap();
        let attachment = allocation.attachment();
        let (_generation, reservation) = allocation.into_parts();
        let leased = context_lifecycle.lease_attachment(reservation).unwrap();
        let (mut attachment_lifecycle, attach) = ContextAttachmentLifecycle::reserve(leased, ())
            .unwrap()
            .into_parts();
        let finish = attachment_lifecycle
            .finish_attach(unsafe { attach.completed_ok::<u8>() })
            .unwrap();
        assert_eq!(finish.effect(), &AttachmentFinishEffect::AttachCompleted);
        let detach = attachment_lifecycle
            .begin_detach(attachment)
            .unwrap()
            .into_request();
        let finish = attachment_lifecycle
            .finish_detach(unsafe { detach.completed_ok::<u8>() })
            .unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("exact secondary DETACH must release");
        };
        let released = attachment_lifecycle.consume_released(authority).unwrap();
        assert_eq!(released.reservation().attachment(), attachment);
        let (attachment_release, association_lease) = released.into_parts();
        let _ = association_lease;
        assert_eq!(context_lifecycle.lease_census(), 1);
        let canonical_row = context_lifecycle
            .return_attachment(attachment_release)
            .unwrap();
        assert_eq!(canonical_row.attachment(), attachment);

        // SAFETY: admission is now closed and the returned canonical row was removed.
        let witness = unsafe { AttachmentsClosed::new(resource) };
        resource_lifecycle
            .install_attachments_closed(witness)
            .unwrap();
        let unref = resource_lifecycle
            .begin_unref(resource)
            .unwrap()
            .into_request();
        let finish = resource_lifecycle
            .finish_unref(
                unref
                    .ambiguous::<u8>(resource.epoch(), AbandonReason::Timeout)
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            finish.effect(),
            &ResourceFinishEffect::UnrefAmbiguous(AbandonReason::Timeout)
        );
        assert!(resource_lifecycle.attachments_are_closed());

        let retry = resource_lifecycle
            .begin_unref(resource)
            .unwrap()
            .into_request();
        let finish = resource_lifecycle
            .finish_unref(unsafe { retry.completed_ok::<u8>() })
            .unwrap();
        let (_, Some(authority), _) = finish.into_parts() else {
            panic!("exact UNREF retry must authorize destruction");
        };
        assert_eq!(resource_lifecycle.consume_terminal(authority), Ok(()));

        let destroy = context_lifecycle
            .begin_destroy(context)
            .unwrap()
            .into_request();
        let finish = context_lifecycle
            .finish_destroy(unsafe { destroy.completed_nodata::<u8>() })
            .unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("exact context destroy must authorize release");
        };
        let released = context_lifecycle.consume_terminal(authority).unwrap();
        let (context_row, ()) = released.into_parts();
        assert_eq!(context_row.context(), context);
    }

    #[test]
    fn secondary_reset_returns_the_combined_row_before_context_reset() {
        let attachment = attachment(7, 36, 18, 20);
        let context = attachment.context();
        let mut context_lifecycle =
            TransportContextLifecycle::new(ContextReservation::test_for_context(context), ());
        let create = context_lifecycle
            .begin_create(context)
            .unwrap()
            .into_request();
        let _ = context_lifecycle
            .finish_create(unsafe { create.completed_nodata::<u8>() })
            .unwrap();
        let leased = context_lifecycle
            .lease_attachment(AttachmentReservation::test_for_attachment(attachment))
            .unwrap();
        let (mut attachment_lifecycle, attach) = ContextAttachmentLifecycle::reserve(leased, ())
            .unwrap()
            .into_parts();
        let _ = attachment_lifecycle
            .finish_attach(unsafe { attach.completed_ok::<u8>() })
            .unwrap();

        let reset = reset(7, 36);
        let authority = attachment_lifecycle
            .transport_reset(attachment, &reset)
            .unwrap();
        let released = attachment_lifecycle.consume_released(authority).unwrap();
        let (attachment_release, ()) = released.into_parts();
        assert_eq!(context_lifecycle.lease_census(), 1);
        assert_eq!(
            context_lifecycle.transport_reset(context, &reset),
            Err(ContextRefusal::AttachmentsOutstanding { count: 1 })
        );
        let tombstone = context_lifecycle
            .return_attachment(attachment_release)
            .unwrap();
        assert_eq!(tombstone.attachment(), attachment);
        assert_eq!(context_lifecycle.lease_census(), 0);
        let authority = context_lifecycle.transport_reset(context, &reset).unwrap();
        drop(context_lifecycle.consume_terminal(authority).unwrap());
    }

    #[test]
    fn forged_same_pair_instances_have_noninterchangeable_provenance() {
        let first_attachment = attachment_with_instance(9, 45, 24, 26, 1);
        let second_attachment = attachment_with_instance(9, 45, 24, 26, 2);
        let (mut first, first_request) =
            ContextAttachmentLifecycle::reserve(leased_attachment(first_attachment, 1), ())
                .unwrap()
                .into_parts();
        let (mut second, second_request) =
            ContextAttachmentLifecycle::reserve(leased_attachment(second_attachment, 2), ())
                .unwrap()
                .into_parts();
        assert_eq!(first_request.sequence(), second_request.sequence());
        assert_eq!(first_attachment.resource(), second_attachment.resource());
        assert_eq!(first_attachment.context(), second_attachment.context());

        let refusal = second
            .finish_attach(unsafe { first_request.completed_ok::<u8>() })
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            AttachmentRefusal::AttachmentInstanceMismatch {
                expected: second_attachment.instance(),
                found: first_attachment.instance(),
            }
        );
        let first_completion = refusal.into_completion();
        let _ = first.finish_attach(first_completion).unwrap();
        assert_eq!(first.phase(), AttachmentPhase::Attached);
        assert_eq!(second.phase(), AttachmentPhase::AttachPending);
        drop(second_request);
        drop(second);
    }

    #[test]
    fn sequence_exhaustion_is_fail_closed_and_release_authority_is_exact() {
        let attachment = attachment(7, 35, 16, 18);
        let (mut lifecycle, request) = admission(attachment, ());
        let _ = lifecycle
            .finish_attach(
                request
                    .ambiguous::<u8>(attachment.epoch(), AbandonReason::Timeout)
                    .unwrap(),
            )
            .unwrap();
        lifecycle.control_high_water = u64::MAX;
        assert_eq!(
            lifecycle.begin_detach(attachment),
            Err(AttachmentRefusal::ControlSequenceExhausted)
        );
        assert_eq!(lifecycle.phase(), AttachmentPhase::RetirementRequired);
        assert!(lifecycle.pending.is_none());

        let authority = lifecycle
            .transport_reset(attachment, &reset(7, 35))
            .unwrap();
        let wrong = ReleaseAttachment {
            attachment,
            authority: AttachmentReleaseAuthorityKind::DetachCompleted,
        };
        let refusal = lifecycle.consume_released(wrong).unwrap_err();
        assert_eq!(
            refusal.reason(),
            AttachmentRefusal::ReleaseAuthorityMismatch
        );
        let (lifecycle, wrong) = refusal.into_parts();
        drop(wrong);
        drop(consume_unit(lifecycle, authority));
    }

    #[test]
    fn dropping_pending_or_ambiguous_state_quarantines_the_strong_lease() {
        let attachment = attachment(8, 40, 20, 22);

        let drops = Rc::new(Cell::new(0));
        let (lifecycle, request) = admission(attachment, DropToken::new(&drops));
        drop(request);
        drop(lifecycle);
        assert_eq!(drops.get(), 0);

        let drops = Rc::new(Cell::new(0));
        let (mut lifecycle, request) = admission(attachment, DropToken::new(&drops));
        let _ = lifecycle
            .finish_attach(
                request
                    .ambiguous::<u8>(attachment.epoch(), AbandonReason::TransportAborted)
                    .unwrap(),
            )
            .unwrap();
        drop(lifecycle);
        assert_eq!(drops.get(), 0);

        let drops = Rc::new(Cell::new(0));
        let mut lifecycle = attached(attachment, DropToken::new(&drops));
        let authority = lifecycle
            .transport_reset(attachment, &reset(8, 40))
            .unwrap();
        let released = lifecycle.consume_released(authority).unwrap();
        assert_eq!(released.reservation().attachment(), attachment);
        let (attachment_release, association_lease) = released.into_parts();
        assert_eq!(drops.get(), 0);
        drop(attachment_release);
        drop(association_lease);
        assert_eq!(drops.get(), 1);
    }
}
