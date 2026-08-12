use core::mem::ManuallyDrop;
use core::num::NonZeroU64;

use crate::control_ownership::{
    AbandonReason, AttachmentReservation, ClassifiedControl, ContextReservation, ControlOutcome,
    ControlSubject, ControlVerb, ExactNoData, HostRejection, PreparedControl, TransportAttachment,
    TransportContext, TransportDomainId, TransportEpoch, TransportReset,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextPhase {
    Reserved,
    CreatePending,
    Live,
    RetirementRequired,
    DestroyPending,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextOperation {
    Create,
    FinishCreate,
    LeaseAttachment,
    CancelAttachment,
    ReturnAttachment,
    Destroy,
    FinishDestroy,
    CancelReservation,
    TransportReset,
    ConsumeTerminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextRefusal {
    DomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    EpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    ContextMismatch {
        expected: u32,
        found: u32,
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
        operation: ContextOperation,
        phase: ContextPhase,
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
    AdmissionClosed,
    AttachmentsOutstanding {
        count: u64,
    },
    LeaseIdExhausted,
    LeaseCensusExhausted,
    LeaseNotIssued {
        high_water: u64,
        found: u64,
    },
    LeaseCensusEmpty,
    AttachmentLeaseMismatch,
    AlreadyTerminal,
    TerminalAuthorityMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextBeginEffect {
    CreateStarted,
    DestroyStarted,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ContextBegin {
    effect: ContextBeginEffect,
    request: PreparedControl,
}

impl ContextBegin {
    pub const fn effect(&self) -> ContextBeginEffect {
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
pub enum ContextFinishEffect<E> {
    CreateDefiniteNotEnqueued(E),
    CreateCompleted,
    CreateHostRejected(HostRejection),
    CreateAmbiguous(AbandonReason),
    DestroyDefiniteNotEnqueued(E),
    DestroyCompleted,
    DestroyHostRejected(HostRejection),
    DestroyAmbiguous(AbandonReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextTerminalAuthorityKind {
    ReservationCancelled,
    CreateDefiniteNotEnqueued,
    CreateHostRejected(HostRejection),
    DestroyCompleted,
    TransportReset(TransportEpoch),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleaseContext {
    context: TransportContext,
    authority: ContextTerminalAuthorityKind,
}

impl ReleaseContext {
    pub const fn context(&self) -> TransportContext {
        self.context
    }

    pub const fn authority(&self) -> ContextTerminalAuthorityKind {
        self.authority
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ContextFinish<E> {
    effect: ContextFinishEffect<E>,
    release: Option<ReleaseContext>,
}

impl<E> ContextFinish<E> {
    pub const fn effect(&self) -> &ContextFinishEffect<E> {
        &self.effect
    }

    pub const fn release_authority(&self) -> Option<&ReleaseContext> {
        self.release.as_ref()
    }

    pub fn into_parts(self) -> (ContextFinishEffect<E>, Option<ReleaseContext>) {
        (self.effect, self.release)
    }

    pub(crate) const fn from_parts(
        effect: ContextFinishEffect<E>,
        release: Option<ReleaseContext>,
    ) -> Self {
        Self { effect, release }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedContextOutcome<T, E> {
    reason: ContextRefusal,
    completion: ClassifiedControl<T, E>,
}

impl<T, E> RefusedContextOutcome<T, E> {
    pub const fn reason(&self) -> ContextRefusal {
        self.reason
    }

    pub fn into_completion(self) -> ClassifiedControl<T, E> {
        self.completion
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ContextLease {
    attachment: TransportAttachment,
    id: NonZeroU64,
}

impl ContextLease {
    pub const fn attachment(&self) -> TransportAttachment {
        self.attachment
    }

    pub const fn id(&self) -> u64 {
        self.id.get()
    }

    #[cfg(test)]
    pub(crate) const fn test_for_attachment(
        attachment: TransportAttachment,
        id: NonZeroU64,
    ) -> Self {
        Self { attachment, id }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct LeasedAttachmentReservation {
    reservation: AttachmentReservation,
    context_lease: ContextLease,
}

impl LeasedAttachmentReservation {
    pub const fn attachment(&self) -> TransportAttachment {
        self.reservation.attachment()
    }

    pub const fn context_lease(&self) -> &ContextLease {
        &self.context_lease
    }

    pub fn is_exact(&self) -> bool {
        self.reservation.attachment() == self.context_lease.attachment
    }

    /// Safety: an exact attachment terminal authority has ended this canonical row.
    pub(crate) unsafe fn into_released(self) -> ReleasedAttachmentLease {
        ReleasedAttachmentLease {
            reservation: ReleasedAttachmentReservation {
                attachment: self.reservation.attachment(),
            },
            context_lease: self.context_lease,
        }
    }

    #[cfg(test)]
    pub(crate) const fn test_for_parts(
        reservation: AttachmentReservation,
        context_lease: ContextLease,
    ) -> Self {
        Self {
            reservation,
            context_lease,
        }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasedAttachmentReservation {
    attachment: TransportAttachment,
}

impl ReleasedAttachmentReservation {
    pub const fn attachment(&self) -> TransportAttachment {
        self.attachment
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasedAttachmentLease {
    reservation: ReleasedAttachmentReservation,
    context_lease: ContextLease,
}

impl ReleasedAttachmentLease {
    pub const fn reservation(&self) -> &ReleasedAttachmentReservation {
        &self.reservation
    }

    pub const fn context_lease(&self) -> &ContextLease {
        &self.context_lease
    }

    fn is_exact(&self) -> bool {
        self.reservation.attachment == self.context_lease.attachment
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedAttachmentLease {
    reason: ContextRefusal,
    reservation: AttachmentReservation,
}

impl RefusedAttachmentLease {
    pub const fn reason(&self) -> ContextRefusal {
        self.reason
    }

    pub fn into_reservation(self) -> AttachmentReservation {
        self.reservation
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedAttachmentCancellation {
    reason: ContextRefusal,
    leased: LeasedAttachmentReservation,
}

impl RefusedAttachmentCancellation {
    pub const fn reason(&self) -> ContextRefusal {
        self.reason
    }

    pub fn into_leased(self) -> LeasedAttachmentReservation {
        self.leased
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedAttachmentReturn {
    reason: ContextRefusal,
    released: ReleasedAttachmentLease,
}

impl RefusedAttachmentReturn {
    pub const fn reason(&self) -> ContextRefusal {
        self.reason
    }

    pub fn into_released(self) -> ReleasedAttachmentLease {
        self.released
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingControl {
    verb: ControlVerb,
    sequence: NonZeroU64,
}

#[derive(Debug, Eq, PartialEq)]
pub struct TransportContextLifecycle<B> {
    reservation: ManuallyDrop<ContextReservation>,
    phase: ContextPhase,
    admission_closed: bool,
    uncertain: bool,
    control_high_water: u64,
    pending: Option<PendingControl>,
    lease_high_water: u64,
    lease_census: u64,
    terminal_authority: Option<ContextTerminalAuthorityKind>,
    owner: ManuallyDrop<B>,
}

impl<B> TransportContextLifecycle<B> {
    pub const fn new(reservation: ContextReservation, owner: B) -> Self {
        Self {
            reservation: ManuallyDrop::new(reservation),
            phase: ContextPhase::Reserved,
            admission_closed: true,
            uncertain: false,
            control_high_water: 0,
            pending: None,
            lease_high_water: 0,
            lease_census: 0,
            terminal_authority: None,
            owner: ManuallyDrop::new(owner),
        }
    }

    pub fn context(&self) -> TransportContext {
        self.reservation.context()
    }

    pub const fn phase(&self) -> ContextPhase {
        self.phase
    }

    pub const fn admission_is_closed(&self) -> bool {
        self.admission_closed
    }

    pub const fn is_uncertain(&self) -> bool {
        self.uncertain
    }

    pub const fn control_high_water(&self) -> u64 {
        self.control_high_water
    }

    pub const fn lease_high_water(&self) -> u64 {
        self.lease_high_water
    }

    pub const fn lease_census(&self) -> u64 {
        self.lease_census
    }

    pub fn begin_create(
        &mut self,
        context: TransportContext,
    ) -> Result<ContextBegin, ContextRefusal> {
        self.check_begin(context, ContextOperation::Create, ContextPhase::Reserved)?;
        let request = self.mint_control(ControlVerb::ContextCreate)?;
        self.phase = ContextPhase::CreatePending;
        Ok(ContextBegin {
            effect: ContextBeginEffect::CreateStarted,
            request,
        })
    }

    pub fn finish_create<E>(
        &mut self,
        completion: ClassifiedControl<ExactNoData, E>,
    ) -> Result<ContextFinish<E>, RefusedContextOutcome<ExactNoData, E>> {
        if let Err(reason) = self.check_finish(
            ContextOperation::FinishCreate,
            ContextPhase::CreatePending,
            ControlVerb::ContextCreate,
            &completion,
        ) {
            return Err(RefusedContextOutcome { reason, completion });
        }
        self.pending = None;
        let (effect, authority) = match completion.into_outcome() {
            ControlOutcome::DefiniteNotEnqueued(error) => (
                ContextFinishEffect::CreateDefiniteNotEnqueued(error),
                ContextTerminalAuthorityKind::CreateDefiniteNotEnqueued,
            ),
            ControlOutcome::Completed(Ok(_)) => {
                self.phase = ContextPhase::Live;
                self.admission_closed = false;
                return Ok(ContextFinish {
                    effect: ContextFinishEffect::CreateCompleted,
                    release: None,
                });
            }
            ControlOutcome::Completed(Err(status)) => (
                ContextFinishEffect::CreateHostRejected(status),
                ContextTerminalAuthorityKind::CreateHostRejected(status),
            ),
            ControlOutcome::Ambiguous(abandoned) => {
                self.phase = ContextPhase::RetirementRequired;
                self.admission_closed = true;
                self.uncertain = true;
                return Ok(ContextFinish {
                    effect: ContextFinishEffect::CreateAmbiguous(abandoned.reason()),
                    release: None,
                });
            }
        };
        self.terminalize(authority);
        Ok(ContextFinish {
            effect,
            release: Some(self.release_token(authority)),
        })
    }

    pub fn lease_attachment(
        &mut self,
        reservation: AttachmentReservation,
    ) -> Result<LeasedAttachmentReservation, RefusedAttachmentLease> {
        let attachment = reservation.attachment();
        let reason = self
            .check_context(attachment.context())
            .and_then(|()| {
                if self.phase == ContextPhase::Terminal {
                    Err(ContextRefusal::AlreadyTerminal)
                } else if self.phase != ContextPhase::Live {
                    Err(ContextRefusal::WrongPhase {
                        operation: ContextOperation::LeaseAttachment,
                        phase: self.phase,
                    })
                } else if self.admission_closed {
                    Err(ContextRefusal::AdmissionClosed)
                } else {
                    Ok(())
                }
            })
            .err();
        if let Some(reason) = reason {
            return Err(RefusedAttachmentLease {
                reason,
                reservation,
            });
        }
        let Some(raw_id) = self.lease_high_water.checked_add(1) else {
            return Err(RefusedAttachmentLease {
                reason: ContextRefusal::LeaseIdExhausted,
                reservation,
            });
        };
        let Some(id) = NonZeroU64::new(raw_id) else {
            return Err(RefusedAttachmentLease {
                reason: ContextRefusal::LeaseIdExhausted,
                reservation,
            });
        };
        let Some(census) = self.lease_census.checked_add(1) else {
            return Err(RefusedAttachmentLease {
                reason: ContextRefusal::LeaseCensusExhausted,
                reservation,
            });
        };
        self.lease_high_water = raw_id;
        self.lease_census = census;
        Ok(LeasedAttachmentReservation {
            reservation,
            context_lease: ContextLease { attachment, id },
        })
    }

    pub fn cancel_attachment(
        &mut self,
        leased: LeasedAttachmentReservation,
    ) -> Result<ReleasedAttachmentReservation, RefusedAttachmentCancellation> {
        let reason = if !leased.is_exact() {
            Some(ContextRefusal::AttachmentLeaseMismatch)
        } else {
            self.check_attachment_return(leased.context_lease(), ContextOperation::CancelAttachment)
                .err()
        };
        if let Some(reason) = reason {
            return Err(RefusedAttachmentCancellation { reason, leased });
        }
        self.lease_census -= 1;
        Ok(ReleasedAttachmentReservation {
            attachment: leased.attachment(),
        })
    }

    pub(crate) fn can_cancel_attachment(&self, leased: &LeasedAttachmentReservation) -> bool {
        leased.is_exact()
            && self
                .check_attachment_return(leased.context_lease(), ContextOperation::CancelAttachment)
                .is_ok()
    }

    /// Safety: `can_cancel_attachment` passed under the sole owner borrow and
    /// the canonical pair row is being removed in this same transition.
    pub(crate) unsafe fn cancel_attachment_validated(
        &mut self,
        leased: LeasedAttachmentReservation,
    ) -> ReleasedAttachmentReservation {
        self.lease_census = self.lease_census.wrapping_sub(1);
        ReleasedAttachmentReservation {
            attachment: leased.attachment(),
        }
    }

    pub fn return_attachment(
        &mut self,
        released: ReleasedAttachmentLease,
    ) -> Result<ReleasedAttachmentReservation, RefusedAttachmentReturn> {
        let reason = if !released.is_exact() {
            Some(ContextRefusal::AttachmentLeaseMismatch)
        } else {
            self.check_attachment_return(
                released.context_lease(),
                ContextOperation::ReturnAttachment,
            )
            .err()
        };
        if let Some(reason) = reason {
            return Err(RefusedAttachmentReturn { reason, released });
        }
        self.lease_census -= 1;
        Ok(released.reservation)
    }

    pub(crate) fn can_return_attachment(&self, released: &ReleasedAttachmentLease) -> bool {
        released.is_exact()
            && self
                .check_attachment_return(
                    released.context_lease(),
                    ContextOperation::ReturnAttachment,
                )
                .is_ok()
    }

    /// Safety: `can_return_attachment` passed under the sole owner borrow and
    /// the canonical pair row is being removed in this same transition.
    pub(crate) unsafe fn return_attachment_validated(
        &mut self,
        released: ReleasedAttachmentLease,
    ) -> ReleasedAttachmentReservation {
        self.lease_census = self.lease_census.wrapping_sub(1);
        released.reservation
    }

    pub fn begin_destroy(
        &mut self,
        context: TransportContext,
    ) -> Result<ContextBegin, ContextRefusal> {
        self.check_context(context)?;
        if self.phase == ContextPhase::Terminal {
            return Err(ContextRefusal::AlreadyTerminal);
        }
        if !matches!(
            self.phase,
            ContextPhase::Live | ContextPhase::RetirementRequired
        ) {
            return Err(ContextRefusal::WrongPhase {
                operation: ContextOperation::Destroy,
                phase: self.phase,
            });
        }
        self.admission_closed = true;
        if self.lease_census != 0 {
            return Err(ContextRefusal::AttachmentsOutstanding {
                count: self.lease_census,
            });
        }
        let request = self.mint_control(ControlVerb::ContextDestroy)?;
        self.phase = ContextPhase::DestroyPending;
        Ok(ContextBegin {
            effect: ContextBeginEffect::DestroyStarted,
            request,
        })
    }

    pub fn finish_destroy<E>(
        &mut self,
        completion: ClassifiedControl<ExactNoData, E>,
    ) -> Result<ContextFinish<E>, RefusedContextOutcome<ExactNoData, E>> {
        if let Err(reason) = self.check_finish(
            ContextOperation::FinishDestroy,
            ContextPhase::DestroyPending,
            ControlVerb::ContextDestroy,
            &completion,
        ) {
            return Err(RefusedContextOutcome { reason, completion });
        }
        self.pending = None;
        let effect = match completion.into_outcome() {
            ControlOutcome::DefiniteNotEnqueued(error) => {
                ContextFinishEffect::DestroyDefiniteNotEnqueued(error)
            }
            ControlOutcome::Completed(Ok(_)) => {
                let authority = ContextTerminalAuthorityKind::DestroyCompleted;
                self.terminalize(authority);
                return Ok(ContextFinish {
                    effect: ContextFinishEffect::DestroyCompleted,
                    release: Some(self.release_token(authority)),
                });
            }
            ControlOutcome::Completed(Err(status)) => {
                ContextFinishEffect::DestroyHostRejected(status)
            }
            ControlOutcome::Ambiguous(abandoned) => {
                self.uncertain = true;
                ContextFinishEffect::DestroyAmbiguous(abandoned.reason())
            }
        };
        self.phase = ContextPhase::RetirementRequired;
        self.admission_closed = true;
        Ok(ContextFinish {
            effect,
            release: None,
        })
    }

    pub fn cancel_reservation(
        &mut self,
        context: TransportContext,
    ) -> Result<ReleaseContext, ContextRefusal> {
        self.check_begin(
            context,
            ContextOperation::CancelReservation,
            ContextPhase::Reserved,
        )?;
        let authority = ContextTerminalAuthorityKind::ReservationCancelled;
        self.terminalize(authority);
        Ok(self.release_token(authority))
    }

    pub fn transport_reset(
        &mut self,
        context: TransportContext,
        reset: &TransportReset,
    ) -> Result<ReleaseContext, ContextRefusal> {
        self.check_context(context)?;
        if reset.retired_epoch().domain() != self.context().epoch().domain() {
            return Err(ContextRefusal::ResetDomainMismatch {
                expected: self.context().epoch().domain(),
                found: reset.retired_epoch().domain(),
            });
        }
        if reset.retired_epoch() != self.context().epoch() {
            return Err(ContextRefusal::ResetEpochMismatch {
                expected: self.context().epoch(),
                found: reset.retired_epoch(),
            });
        }
        if self.phase == ContextPhase::Terminal {
            return Err(ContextRefusal::AlreadyTerminal);
        }
        self.admission_closed = true;
        if self.lease_census != 0 {
            return Err(ContextRefusal::AttachmentsOutstanding {
                count: self.lease_census,
            });
        }
        let authority = ContextTerminalAuthorityKind::TransportReset(reset.retired_epoch());
        self.terminalize(authority);
        Ok(self.release_token(authority))
    }

    pub(crate) fn can_reset(&self, context: TransportContext, reset: &TransportReset) -> bool {
        self.check_context(context).is_ok()
            && reset.retired_epoch() == self.context().epoch()
            && self.phase != ContextPhase::Terminal
            && self.lease_census == 0
    }

    pub(crate) fn reset_consume(
        mut self,
        context: TransportContext,
        reset: &TransportReset,
    ) -> Result<ReleasedContext<B>, Self> {
        let authority = match self.transport_reset(context, reset) {
            Ok(authority) => authority,
            Err(_) => return Err(self),
        };
        match self.consume_terminal(authority) {
            Ok(released) => Ok(released),
            Err(refused) => {
                let (lifecycle, _) = refused.into_parts();
                Err(lifecycle)
            }
        }
    }

    pub fn consume_terminal(
        self,
        authority: ReleaseContext,
    ) -> Result<ReleasedContext<B>, RefusedReleaseContext<B>> {
        let reason = self
            .check_context(authority.context)
            .and_then(|()| {
                if self.phase != ContextPhase::Terminal {
                    Err(ContextRefusal::WrongPhase {
                        operation: ContextOperation::ConsumeTerminal,
                        phase: self.phase,
                    })
                } else if self.terminal_authority != Some(authority.authority) {
                    Err(ContextRefusal::TerminalAuthorityMismatch)
                } else {
                    Ok(())
                }
            })
            .err();
        if let Some(reason) = reason {
            return Err(RefusedReleaseContext {
                reason,
                lifecycle: self,
                authority,
            });
        }
        let reservation = ManuallyDrop::into_inner(self.reservation);
        Ok(ReleasedContext {
            reservation: ReleasedContextReservation {
                context: reservation.context(),
            },
            owner: ManuallyDrop::into_inner(self.owner),
        })
    }

    fn check_begin(
        &self,
        context: TransportContext,
        operation: ContextOperation,
        expected: ContextPhase,
    ) -> Result<(), ContextRefusal> {
        self.check_context(context)?;
        if self.phase == ContextPhase::Terminal {
            return Err(ContextRefusal::AlreadyTerminal);
        }
        if self.phase != expected {
            return Err(ContextRefusal::WrongPhase {
                operation,
                phase: self.phase,
            });
        }
        Ok(())
    }

    fn check_finish<T, E>(
        &self,
        operation: ContextOperation,
        expected_phase: ContextPhase,
        expected_verb: ControlVerb,
        completion: &ClassifiedControl<T, E>,
    ) -> Result<(), ContextRefusal> {
        if self.phase == ContextPhase::Terminal {
            return Err(ContextRefusal::AlreadyTerminal);
        }
        if self.phase != expected_phase {
            return Err(ContextRefusal::WrongPhase {
                operation,
                phase: self.phase,
            });
        }
        let Some(pending) = self.pending else {
            return Err(ContextRefusal::MissingPendingControl);
        };
        if pending.verb != expected_verb {
            return Err(ContextRefusal::ControlVerbMismatch {
                expected: expected_verb,
                found: pending.verb,
            });
        }
        let ControlSubject::Context(found) = completion.subject() else {
            return Err(ContextRefusal::ControlSubjectMismatch);
        };
        self.check_context(found)?;
        if completion.verb() != pending.verb {
            return Err(ContextRefusal::ControlVerbMismatch {
                expected: pending.verb,
                found: completion.verb(),
            });
        }
        if completion.sequence() != pending.sequence.get() {
            return Err(ContextRefusal::ControlSequenceMismatch {
                expected: pending.sequence.get(),
                found: completion.sequence(),
            });
        }
        Ok(())
    }

    fn check_context(&self, found: TransportContext) -> Result<(), ContextRefusal> {
        let expected = self.context();
        if found.epoch().domain() != expected.epoch().domain() {
            return Err(ContextRefusal::DomainMismatch {
                expected: expected.epoch().domain(),
                found: found.epoch().domain(),
            });
        }
        if found.epoch() != expected.epoch() {
            return Err(ContextRefusal::EpochMismatch {
                expected: expected.epoch(),
                found: found.epoch(),
            });
        }
        if found.id() != expected.id() {
            return Err(ContextRefusal::ContextMismatch {
                expected: expected.id(),
                found: found.id(),
            });
        }
        Ok(())
    }

    fn check_attachment_return(
        &self,
        lease: &ContextLease,
        operation: ContextOperation,
    ) -> Result<(), ContextRefusal> {
        self.check_context(lease.attachment.context())?;
        if self.phase == ContextPhase::Terminal {
            Err(ContextRefusal::AlreadyTerminal)
        } else if self.phase != ContextPhase::Live {
            Err(ContextRefusal::WrongPhase {
                operation,
                phase: self.phase,
            })
        } else if lease.id.get() > self.lease_high_water {
            Err(ContextRefusal::LeaseNotIssued {
                high_water: self.lease_high_water,
                found: lease.id.get(),
            })
        } else if self.lease_census == 0 {
            Err(ContextRefusal::LeaseCensusEmpty)
        } else {
            Ok(())
        }
    }

    fn mint_control(&mut self, verb: ControlVerb) -> Result<PreparedControl, ContextRefusal> {
        if self.pending.is_some() {
            return Err(ContextRefusal::ControlAlreadyPending);
        }
        let Some(raw) = self.control_high_water.checked_add(1) else {
            return Err(ContextRefusal::ControlSequenceExhausted);
        };
        let Some(sequence) = NonZeroU64::new(raw) else {
            return Err(ContextRefusal::ControlSequenceExhausted);
        };
        let pending = PendingControl { verb, sequence };
        self.control_high_water = raw;
        self.pending = Some(pending);
        Ok(PreparedControl::from_parts(
            pending.verb,
            ControlSubject::Context(self.context()),
            pending.sequence,
        ))
    }

    fn terminalize(&mut self, authority: ContextTerminalAuthorityKind) {
        self.phase = ContextPhase::Terminal;
        self.admission_closed = true;
        self.pending = None;
        self.terminal_authority = Some(authority);
    }

    fn release_token(&self, authority: ContextTerminalAuthorityKind) -> ReleaseContext {
        ReleaseContext {
            context: self.context(),
            authority,
        }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasedContextReservation {
    context: TransportContext,
}

impl ReleasedContextReservation {
    pub const fn context(&self) -> TransportContext {
        self.context
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleasedContext<B> {
    reservation: ReleasedContextReservation,
    owner: B,
}

impl<B> ReleasedContext<B> {
    pub const fn reservation(&self) -> &ReleasedContextReservation {
        &self.reservation
    }

    pub const fn owner(&self) -> &B {
        &self.owner
    }

    pub fn into_parts(self) -> (ReleasedContextReservation, B) {
        (self.reservation, self.owner)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedReleaseContext<B> {
    reason: ContextRefusal,
    lifecycle: TransportContextLifecycle<B>,
    authority: ReleaseContext,
}

impl<B> RefusedReleaseContext<B> {
    pub const fn reason(&self) -> ContextRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (TransportContextLifecycle<B>, ReleaseContext) {
        (self.lifecycle, self.authority)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::cell::Cell;
    use self::std::rc::Rc;
    use super::*;
    use crate::control_ownership::{TransportDomainId, TransportResource};
    use helios_protocol::virtio_gpu::VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID;

    const DEFINITE_ERROR: u8 = 0x6d;

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

    fn context(domain: u64, generation: u64, id: u32) -> TransportContext {
        TransportContext::from_raw(epoch(domain, generation), id).unwrap()
    }

    fn attachment(
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

    fn lifecycle<B>(context: TransportContext, owner: B) -> TransportContextLifecycle<B> {
        TransportContextLifecycle::new(ContextReservation::test_for_context(context), owner)
    }

    fn host_error() -> HostRejection {
        HostRejection::from_response_type(VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID).unwrap()
    }

    fn reset(domain: u64, generation: u64) -> TransportReset {
        TransportReset::test_for_epoch(epoch(domain, generation))
    }

    fn outcome(
        case: OutcomeCase,
        request: PreparedControl,
        epoch: TransportEpoch,
    ) -> ClassifiedControl<ExactNoData, u8> {
        match case {
            OutcomeCase::DefiniteNotEnqueued => unsafe {
                request.nodata_definite_not_enqueued(DEFINITE_ERROR)
            },
            OutcomeCase::Completed => unsafe { request.completed_nodata() },
            OutcomeCase::HostRejected => unsafe {
                request.completed_nodata_rejection(host_error())
            },
            OutcomeCase::Timeout => request
                .ambiguous_nodata(epoch, AbandonReason::Timeout)
                .unwrap(),
            OutcomeCase::NotOurs => request
                .ambiguous_nodata(epoch, AbandonReason::NotOurs)
                .unwrap(),
            OutcomeCase::TransportAborted => request
                .ambiguous_nodata(epoch, AbandonReason::TransportAborted)
                .unwrap(),
            OutcomeCase::MalformedResponse => request
                .ambiguous_nodata(epoch, AbandonReason::MalformedResponse)
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

    fn live<B>(context: TransportContext, owner: B) -> TransportContextLifecycle<B> {
        let mut lifecycle = lifecycle(context, owner);
        let request = lifecycle.begin_create(context).unwrap().into_request();
        let finish = lifecycle
            .finish_create(unsafe { request.completed_nodata::<u8>() })
            .unwrap();
        assert_eq!(finish.effect(), &ContextFinishEffect::CreateCompleted);
        lifecycle
    }

    fn reservation(attachment: TransportAttachment) -> AttachmentReservation {
        AttachmentReservation::test_for_attachment(attachment)
    }

    fn released(attachment: TransportAttachment, lease_id: u64) -> ReleasedAttachmentLease {
        ReleasedAttachmentLease {
            reservation: ReleasedAttachmentReservation { attachment },
            context_lease: ContextLease::test_for_attachment(
                attachment,
                NonZeroU64::new(lease_id).unwrap(),
            ),
        }
    }

    fn exact_destroy<B>(
        lifecycle: &mut TransportContextLifecycle<B>,
        context: TransportContext,
    ) -> ReleaseContext {
        let request = lifecycle.begin_destroy(context).unwrap().into_request();
        let finish = lifecycle
            .finish_destroy(unsafe { request.completed_nodata::<u8>() })
            .unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("exact NODATA destroy must release");
        };
        authority
    }

    #[test]
    fn create_outcomes_have_the_exact_terminal_boundary() {
        let context = context(1, 7, 3);
        for case in OUTCOMES {
            let mut lifecycle = lifecycle(context, ());
            let request = lifecycle.begin_create(context).unwrap().into_request();
            assert_eq!(request.verb(), ControlVerb::ContextCreate);
            assert_eq!(request.subject(), ControlSubject::Context(context));
            assert_eq!(request.sequence(), 1);
            let finish = lifecycle
                .finish_create(outcome(case, request, context.epoch()))
                .unwrap();
            match case {
                OutcomeCase::DefiniteNotEnqueued => {
                    assert_eq!(
                        finish.effect(),
                        &ContextFinishEffect::CreateDefiniteNotEnqueued(DEFINITE_ERROR)
                    );
                    let (_, Some(authority)) = finish.into_parts() else {
                        panic!("definite non-enqueue must release");
                    };
                    assert_eq!(
                        authority.authority(),
                        ContextTerminalAuthorityKind::CreateDefiniteNotEnqueued
                    );
                    let released = lifecycle.consume_terminal(authority).unwrap();
                    assert_eq!(released.reservation().context(), context);
                }
                OutcomeCase::HostRejected => {
                    assert_eq!(
                        finish.effect(),
                        &ContextFinishEffect::CreateHostRejected(host_error())
                    );
                    let (_, Some(authority)) = finish.into_parts() else {
                        panic!("exact rejection must release");
                    };
                    assert_eq!(
                        authority.authority(),
                        ContextTerminalAuthorityKind::CreateHostRejected(host_error())
                    );
                    drop(lifecycle.consume_terminal(authority).unwrap());
                }
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &ContextFinishEffect::CreateCompleted);
                    assert!(finish.release_authority().is_none());
                    assert_eq!(lifecycle.phase(), ContextPhase::Live);
                    assert!(!lifecycle.admission_is_closed());
                    let authority = exact_destroy(&mut lifecycle, context);
                    drop(lifecycle.consume_terminal(authority).unwrap());
                }
                _ => {
                    assert_eq!(
                        finish.effect(),
                        &ContextFinishEffect::CreateAmbiguous(abandon_reason(case).unwrap())
                    );
                    assert!(finish.release_authority().is_none());
                    assert_eq!(lifecycle.phase(), ContextPhase::RetirementRequired);
                    assert!(lifecycle.admission_is_closed());
                    assert!(lifecycle.is_uncertain());
                    let authority = exact_destroy(&mut lifecycle, context);
                    drop(lifecycle.consume_terminal(authority).unwrap());
                }
            }
        }
    }

    #[test]
    fn destroy_outcomes_release_only_on_exact_nodata_and_every_other_case_retries() {
        let context = context(2, 9, 4);
        for case in OUTCOMES {
            let mut lifecycle = live(context, ());
            let request = lifecycle.begin_destroy(context).unwrap().into_request();
            assert!(lifecycle.admission_is_closed());
            assert_eq!(request.verb(), ControlVerb::ContextDestroy);
            assert_eq!(request.sequence(), 2);
            let finish = lifecycle
                .finish_destroy(outcome(case, request, context.epoch()))
                .unwrap();
            match case {
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &ContextFinishEffect::DestroyCompleted);
                    let (_, Some(authority)) = finish.into_parts() else {
                        panic!("exact NODATA must release");
                    };
                    drop(lifecycle.consume_terminal(authority).unwrap());
                    continue;
                }
                OutcomeCase::DefiniteNotEnqueued => assert_eq!(
                    finish.effect(),
                    &ContextFinishEffect::DestroyDefiniteNotEnqueued(DEFINITE_ERROR)
                ),
                OutcomeCase::HostRejected => assert_eq!(
                    finish.effect(),
                    &ContextFinishEffect::DestroyHostRejected(host_error())
                ),
                _ => assert_eq!(
                    finish.effect(),
                    &ContextFinishEffect::DestroyAmbiguous(abandon_reason(case).unwrap())
                ),
            }
            assert!(finish.release_authority().is_none());
            assert_eq!(lifecycle.phase(), ContextPhase::RetirementRequired);
            assert!(lifecycle.admission_is_closed());
            assert_eq!(lifecycle.is_uncertain(), abandon_reason(case).is_some());
            let retry = lifecycle.begin_destroy(context).unwrap().into_request();
            assert_eq!(retry.sequence(), 3);
            let finish = lifecycle
                .finish_destroy(unsafe { retry.completed_nodata::<u8>() })
                .unwrap();
            let (_, Some(authority)) = finish.into_parts() else {
                panic!("exact retry must release");
            };
            drop(lifecycle.consume_terminal(authority).unwrap());
        }
    }

    #[test]
    fn context_control_provenance_and_replay_are_inert_and_recoverable() {
        let other_domain = context(4, 20, 8);
        let other_epoch = context(3, 21, 8);
        let other_context = context(3, 20, 9);
        let context = context(3, 20, 8);
        let mut lifecycle = lifecycle(context, ());
        let request = lifecycle.begin_create(context).unwrap().into_request();

        for (completion, expected) in [
            (
                unsafe {
                    PreparedControl::from_parts(
                        ControlVerb::ContextCreate,
                        ControlSubject::Context(other_domain),
                        NonZeroU64::MIN,
                    )
                    .completed_nodata::<u8>()
                },
                ContextRefusal::DomainMismatch {
                    expected: context.epoch().domain(),
                    found: other_domain.epoch().domain(),
                },
            ),
            (
                unsafe {
                    PreparedControl::from_parts(
                        ControlVerb::ContextCreate,
                        ControlSubject::Context(other_epoch),
                        NonZeroU64::MIN,
                    )
                    .completed_nodata::<u8>()
                },
                ContextRefusal::EpochMismatch {
                    expected: context.epoch(),
                    found: other_epoch.epoch(),
                },
            ),
            (
                unsafe {
                    PreparedControl::from_parts(
                        ControlVerb::ContextCreate,
                        ControlSubject::Context(other_context),
                        NonZeroU64::MIN,
                    )
                    .completed_nodata::<u8>()
                },
                ContextRefusal::ContextMismatch {
                    expected: context.id(),
                    found: other_context.id(),
                },
            ),
            (
                unsafe {
                    PreparedControl::from_parts(
                        ControlVerb::ContextCreate,
                        ControlSubject::Resource(
                            TransportResource::from_raw(context.epoch(), 1).unwrap(),
                        ),
                        NonZeroU64::MIN,
                    )
                    .completed_nodata::<u8>()
                },
                ContextRefusal::ControlSubjectMismatch,
            ),
            (
                unsafe {
                    PreparedControl::from_parts(
                        ControlVerb::ContextDestroy,
                        ControlSubject::Context(context),
                        NonZeroU64::MIN,
                    )
                    .completed_nodata::<u8>()
                },
                ContextRefusal::ControlVerbMismatch {
                    expected: ControlVerb::ContextCreate,
                    found: ControlVerb::ContextDestroy,
                },
            ),
            (
                unsafe {
                    PreparedControl::from_parts(
                        ControlVerb::ContextCreate,
                        ControlSubject::Context(context),
                        NonZeroU64::new(2).unwrap(),
                    )
                    .completed_nodata::<u8>()
                },
                ContextRefusal::ControlSequenceMismatch {
                    expected: 1,
                    found: 2,
                },
            ),
        ] {
            let refusal = lifecycle.finish_create(completion).unwrap_err();
            assert_eq!(refusal.reason(), expected);
            drop(refusal.into_completion());
            assert_eq!(lifecycle.phase(), ContextPhase::CreatePending);
        }

        let _ = lifecycle
            .finish_create(unsafe { request.completed_nodata::<u8>() })
            .unwrap();
        let replay = unsafe {
            PreparedControl::from_parts(
                ControlVerb::ContextCreate,
                ControlSubject::Context(context),
                NonZeroU64::MIN,
            )
            .completed_nodata::<u8>()
        };
        let refusal = lifecycle.finish_create(replay).unwrap_err();
        assert_eq!(
            refusal.reason(),
            ContextRefusal::WrongPhase {
                operation: ContextOperation::FinishCreate,
                phase: ContextPhase::Live,
            }
        );
        drop(refusal.into_completion());

        let first = lifecycle.begin_destroy(context).unwrap().into_request();
        let first_sequence = first.sequence();
        let _ = lifecycle
            .finish_destroy(
                first
                    .ambiguous_nodata::<u8>(context.epoch(), AbandonReason::Timeout)
                    .unwrap(),
            )
            .unwrap();
        let retry = lifecycle.begin_destroy(context).unwrap().into_request();
        let retry_sequence = retry.sequence();
        let late = unsafe {
            PreparedControl::from_parts(
                ControlVerb::ContextDestroy,
                ControlSubject::Context(context),
                NonZeroU64::new(first_sequence).unwrap(),
            )
            .completed_nodata::<u8>()
        };
        let refusal = lifecycle.finish_destroy(late).unwrap_err();
        assert_eq!(
            refusal.reason(),
            ContextRefusal::ControlSequenceMismatch {
                expected: retry_sequence,
                found: first_sequence,
            }
        );
        drop(refusal.into_completion());
        let finish = lifecycle
            .finish_destroy(unsafe { retry.completed_nodata::<u8>() })
            .unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("exact retry must release");
        };
        drop(lifecycle.consume_terminal(authority).unwrap());
    }

    #[test]
    fn destroy_closes_admission_before_a_checked_attachment_drain() {
        let context = context(5, 25, 10);
        let first_attachment = attachment(5, 25, 1, 10, 1);
        let second_attachment = attachment(5, 25, 2, 10, 2);
        let mut lifecycle = live(context, ());
        let first = lifecycle
            .lease_attachment(reservation(first_attachment))
            .unwrap();
        assert_eq!(lifecycle.lease_census(), 1);

        assert_eq!(
            lifecycle.begin_destroy(context),
            Err(ContextRefusal::AttachmentsOutstanding { count: 1 })
        );
        assert!(lifecycle.admission_is_closed());
        let refusal = lifecycle
            .lease_attachment(reservation(second_attachment))
            .unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::AdmissionClosed);
        assert_eq!(refusal.into_reservation().attachment(), second_attachment);

        let tombstone = lifecycle.cancel_attachment(first).unwrap();
        assert_eq!(tombstone.attachment(), first_attachment);
        assert_eq!(lifecycle.lease_census(), 0);
        assert!(lifecycle.admission_is_closed());
        let authority = exact_destroy(&mut lifecycle, context);
        drop(lifecycle.consume_terminal(authority).unwrap());
    }

    #[test]
    fn prewire_cancel_is_exact_one_shot_and_recovers_the_full_lease_on_refusal() {
        let context = context(5, 26, 10);
        let exact_attachment = attachment(5, 26, 1, 10, 1);
        let foreign_attachment = attachment(5, 26, 1, 11, 2);
        let mut lifecycle = live(context, ());
        let exact = lifecycle
            .lease_attachment(reservation(exact_attachment))
            .unwrap();

        let foreign = LeasedAttachmentReservation::test_for_parts(
            reservation(foreign_attachment),
            ContextLease::test_for_attachment(foreign_attachment, NonZeroU64::MIN),
        );
        let refusal = lifecycle.cancel_attachment(foreign).unwrap_err();
        assert!(matches!(
            refusal.reason(),
            ContextRefusal::ContextMismatch { .. }
        ));
        let recovered = refusal.into_leased();
        assert_eq!(recovered.attachment(), foreign_attachment);
        assert_eq!(recovered.context_lease().attachment(), foreign_attachment);
        assert_eq!(lifecycle.lease_census(), 1);

        let mismatched = LeasedAttachmentReservation::test_for_parts(
            reservation(exact_attachment),
            ContextLease::test_for_attachment(foreign_attachment, NonZeroU64::MIN),
        );
        let refusal = lifecycle.cancel_attachment(mismatched).unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::AttachmentLeaseMismatch);
        let recovered = refusal.into_leased();
        assert_eq!(recovered.attachment(), exact_attachment);
        assert_eq!(recovered.context_lease().attachment(), foreign_attachment);
        assert_eq!(lifecycle.lease_census(), 1);

        let tombstone = lifecycle.cancel_attachment(exact).unwrap();
        assert_eq!(tombstone.attachment(), exact_attachment);
        assert_eq!(lifecycle.lease_census(), 0);
        let replay = LeasedAttachmentReservation::test_for_parts(
            reservation(exact_attachment),
            ContextLease::test_for_attachment(exact_attachment, NonZeroU64::MIN),
        );
        let refusal = lifecycle.cancel_attachment(replay).unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::LeaseCensusEmpty);
        assert_eq!(refusal.into_leased().attachment(), exact_attachment);
    }

    #[test]
    fn lease_identity_refusals_recover_tokens_and_census_never_underflows() {
        let context = context(6, 30, 12);
        let exact_attachment = attachment(6, 30, 1, 12, 1);
        let mut lifecycle = live(context, ());
        let leased = lifecycle
            .lease_attachment(reservation(exact_attachment))
            .unwrap();
        assert_eq!(leased.context_lease().attachment(), exact_attachment);
        assert_eq!(leased.context_lease().id(), 1);
        assert_eq!(lifecycle.lease_census(), 1);
        let exact_released = unsafe { leased.into_released() };

        for foreign in [
            attachment(7, 30, 1, 12, 1),
            attachment(6, 31, 1, 12, 1),
            attachment(6, 30, 1, 13, 1),
        ] {
            let forged = released(foreign, 1);
            let refusal = lifecycle.return_attachment(forged).unwrap_err();
            assert!(matches!(
                refusal.reason(),
                ContextRefusal::DomainMismatch { .. }
                    | ContextRefusal::EpochMismatch { .. }
                    | ContextRefusal::ContextMismatch { .. }
            ));
            let recovered = refusal.into_released();
            assert_eq!(recovered.reservation().attachment(), foreign);
            assert_eq!(recovered.context_lease().attachment(), foreign);
            assert_eq!(lifecycle.lease_census(), 1);
        }

        let mismatched = ReleasedAttachmentLease {
            reservation: ReleasedAttachmentReservation {
                attachment: exact_attachment,
            },
            context_lease: ContextLease::test_for_attachment(
                attachment(6, 30, 2, 12, 2),
                NonZeroU64::MIN,
            ),
        };
        let refusal = lifecycle.return_attachment(mismatched).unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::AttachmentLeaseMismatch);
        let recovered = refusal.into_released();
        assert_eq!(recovered.reservation().attachment(), exact_attachment);
        assert_eq!(recovered.context_lease().attachment().resource().id(), 2);
        assert_eq!(lifecycle.lease_census(), 1);

        let future = released(exact_attachment, 2);
        let refusal = lifecycle.return_attachment(future).unwrap_err();
        assert_eq!(
            refusal.reason(),
            ContextRefusal::LeaseNotIssued {
                high_water: 1,
                found: 2,
            }
        );
        let recovered = refusal.into_released();
        assert_eq!(recovered.context_lease().id(), 2);
        assert_eq!(lifecycle.lease_census(), 1);
        let tombstone = lifecycle.return_attachment(exact_released).unwrap();
        assert_eq!(tombstone.attachment(), exact_attachment);
        assert_eq!(lifecycle.lease_census(), 0);

        let replay = released(exact_attachment, 1);
        let refusal = lifecycle.return_attachment(replay).unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::LeaseCensusEmpty);
        assert_eq!(
            refusal.into_released().reservation().attachment(),
            exact_attachment
        );
    }

    #[test]
    fn lease_and_control_high_waters_exhaust_without_mutating_ownership() {
        let context = context(8, 35, 14);
        let attachment = attachment(8, 35, 1, 14, 1);
        let mut lifecycle = live(context, ());
        lifecycle.lease_high_water = u64::MAX;
        let refusal = lifecycle
            .lease_attachment(reservation(attachment))
            .unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::LeaseIdExhausted);
        assert_eq!(refusal.into_reservation().attachment(), attachment);
        assert_eq!(lifecycle.lease_census(), 0);

        lifecycle.lease_high_water = 1;
        lifecycle.lease_census = u64::MAX;
        let refusal = lifecycle
            .lease_attachment(reservation(attachment))
            .unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::LeaseCensusExhausted);
        drop(refusal.into_reservation());
        assert_eq!(lifecycle.lease_high_water(), 1);
        assert_eq!(lifecycle.lease_census(), u64::MAX);

        let mut lifecycle = live(context, ());
        lifecycle.control_high_water = u64::MAX;
        assert_eq!(
            lifecycle.begin_destroy(context),
            Err(ContextRefusal::ControlSequenceExhausted)
        );
        assert!(lifecycle.admission_is_closed());
        assert_eq!(lifecycle.phase(), ContextPhase::Live);
    }

    #[derive(Clone, Copy)]
    enum ResetPhase {
        Reserved,
        CreatePending,
        Live,
        RetirementRequired,
        DestroyPending,
    }

    fn lifecycle_at_phase(
        context: TransportContext,
        phase: ResetPhase,
    ) -> TransportContextLifecycle<()> {
        let mut lifecycle = lifecycle(context, ());
        match phase {
            ResetPhase::Reserved => {}
            ResetPhase::CreatePending => {
                let request = lifecycle.begin_create(context).unwrap().into_request();
                drop(request);
            }
            ResetPhase::Live => return live(context, ()),
            ResetPhase::RetirementRequired => {
                let request = lifecycle.begin_create(context).unwrap().into_request();
                let _ = lifecycle
                    .finish_create(
                        request
                            .ambiguous_nodata::<u8>(context.epoch(), AbandonReason::Timeout)
                            .unwrap(),
                    )
                    .unwrap();
            }
            ResetPhase::DestroyPending => {
                lifecycle = live(context, ());
                let request = lifecycle.begin_destroy(context).unwrap().into_request();
                drop(request);
            }
        }
        lifecycle
    }

    #[test]
    fn matching_reset_terminalizes_every_phase_only_after_leases_return() {
        let context = context(9, 40, 16);
        let reset = reset(9, 40);
        for phase in [
            ResetPhase::Reserved,
            ResetPhase::CreatePending,
            ResetPhase::Live,
            ResetPhase::RetirementRequired,
            ResetPhase::DestroyPending,
        ] {
            let mut lifecycle = lifecycle_at_phase(context, phase);
            let authority = lifecycle.transport_reset(context, &reset).unwrap();
            assert_eq!(lifecycle.phase(), ContextPhase::Terminal);
            assert_eq!(
                authority.authority(),
                ContextTerminalAuthorityKind::TransportReset(context.epoch())
            );
            drop(lifecycle.consume_terminal(authority).unwrap());
        }

        let exact_attachment = attachment(9, 40, 1, 16, 1);
        let mut lifecycle = live(context, ());
        let leased = lifecycle
            .lease_attachment(reservation(exact_attachment))
            .unwrap();
        assert_eq!(
            lifecycle.transport_reset(context, &reset),
            Err(ContextRefusal::AttachmentsOutstanding { count: 1 })
        );
        assert!(lifecycle.admission_is_closed());
        let refusal = lifecycle
            .lease_attachment(reservation(attachment(9, 40, 2, 16, 2)))
            .unwrap_err();
        assert_eq!(refusal.reason(), ContextRefusal::AdmissionClosed);
        drop(refusal.into_reservation());
        let tombstone = lifecycle.cancel_attachment(leased).unwrap();
        assert_eq!(tombstone.attachment(), exact_attachment);
        let authority = lifecycle.transport_reset(context, &reset).unwrap();
        drop(lifecycle.consume_terminal(authority).unwrap());
    }

    #[test]
    fn foreign_reset_and_context_identity_are_inert() {
        let exact_context = context(10, 45, 18);
        let other_domain = context(11, 45, 18);
        let other_epoch = context(10, 46, 18);
        let other_context = context(10, 45, 19);
        let mut lifecycle = live(exact_context, ());
        assert!(matches!(
            lifecycle.begin_destroy(other_domain),
            Err(ContextRefusal::DomainMismatch { .. })
        ));
        assert!(matches!(
            lifecycle.begin_destroy(other_epoch),
            Err(ContextRefusal::EpochMismatch { .. })
        ));
        assert!(matches!(
            lifecycle.begin_destroy(other_context),
            Err(ContextRefusal::ContextMismatch { .. })
        ));
        assert!(!lifecycle.admission_is_closed());
        assert_eq!(
            lifecycle.transport_reset(exact_context, &reset(11, 45)),
            Err(ContextRefusal::ResetDomainMismatch {
                expected: exact_context.epoch().domain(),
                found: other_domain.epoch().domain(),
            })
        );
        assert_eq!(
            lifecycle.transport_reset(exact_context, &reset(10, 46)),
            Err(ContextRefusal::ResetEpochMismatch {
                expected: exact_context.epoch(),
                found: other_epoch.epoch(),
            })
        );
        assert_eq!(lifecycle.phase(), ContextPhase::Live);
    }

    #[test]
    fn reserved_cancel_and_terminal_authority_are_exact_and_one_shot() {
        let context_a = context(12, 50, 20);
        let context_b = context(12, 50, 21);
        let mut a = lifecycle(context_a, 10u8);
        let mut b = lifecycle(context_b, 20u8);
        let authority_a = a.cancel_reservation(context_a).unwrap();
        let authority_b = b.cancel_reservation(context_b).unwrap();
        assert_eq!(
            a.cancel_reservation(context_a),
            Err(ContextRefusal::AlreadyTerminal)
        );
        let refusal = a.consume_terminal(authority_b).unwrap_err();
        assert!(matches!(
            refusal.reason(),
            ContextRefusal::ContextMismatch { .. }
        ));
        let (a, authority_b) = refusal.into_parts();
        let released_a = a.consume_terminal(authority_a).unwrap();
        let (row_a, owner_a) = released_a.into_parts();
        assert_eq!(row_a.context(), context_a);
        assert_eq!(owner_a, 10);
        let (_, owner_b) = b.consume_terminal(authority_b).unwrap().into_parts();
        assert_eq!(owner_b, 20);
    }

    #[test]
    fn dropping_ambiguous_or_leased_context_quarantines_the_owner() {
        let context = context(13, 55, 22);
        let drops = Rc::new(Cell::new(0));
        let mut lifecycle = lifecycle(context, DropToken::new(&drops));
        let request = lifecycle.begin_create(context).unwrap().into_request();
        let _ = lifecycle
            .finish_create(
                request
                    .ambiguous_nodata::<u8>(context.epoch(), AbandonReason::TransportAborted)
                    .unwrap(),
            )
            .unwrap();
        drop(lifecycle);
        assert_eq!(drops.get(), 0);

        let drops = Rc::new(Cell::new(0));
        let mut lifecycle = live(context, DropToken::new(&drops));
        let leased = lifecycle
            .lease_attachment(reservation(attachment(13, 55, 1, 22, 1)))
            .unwrap();
        drop(leased);
        drop(lifecycle);
        assert_eq!(drops.get(), 0);

        let drops = Rc::new(Cell::new(0));
        let mut lifecycle = live(context, DropToken::new(&drops));
        let authority = exact_destroy(&mut lifecycle, context);
        let released = lifecycle.consume_terminal(authority).unwrap();
        let (_, owner) = released.into_parts();
        assert_eq!(drops.get(), 0);
        drop(owner);
        assert_eq!(drops.get(), 1);
    }
}
