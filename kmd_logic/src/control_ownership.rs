//! Pure ownership rules for epoch-qualified host control operations.
//! Embedded tokens use `ManuallyDrop`: dropping uncertain state quarantines it;
//! unsafe seams expose, not prove, unique-domain/device facts and stay pointer-bound.

use core::mem::ManuallyDrop;
use core::num::{NonZeroU32, NonZeroU64};
use helios_protocol::virtio_gpu::{
    VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER,
    VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID, VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID,
    VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY, VIRTIO_GPU_RESP_ERR_UNSPEC,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpochRefusal {
    Zero,
    Exhausted,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TransportDomainId(NonZeroU64);

impl TransportDomainId {
    const fn from_raw(raw: u64) -> Result<Self, TransportDomainRefusal> {
        match NonZeroU64::new(raw) {
            Some(id) => Ok(Self(id)),
            None => Err(TransportDomainRefusal::Zero),
        }
    }

    #[cfg(test)]
    pub(crate) const fn test_from_raw(raw: u64) -> Result<Self, TransportDomainRefusal> {
        Self::from_raw(raw)
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportDomainRefusal {
    Zero,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct TransportDomainRoot {
    id: TransportDomainId,
}

impl TransportDomainRoot {
    /// Safety: `raw` is globally unique and is not reused until every request,
    /// completion, reset witness, and other descendant of this root is gone.
    pub const unsafe fn new(raw: u64) -> Result<Self, TransportDomainRefusal> {
        match TransportDomainId::from_raw(raw) {
            Ok(id) => Ok(Self { id }),
            Err(reason) => Err(reason),
        }
    }

    pub const fn id(&self) -> TransportDomainId {
        self.id
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TransportEpoch {
    domain: TransportDomainId,
    generation: NonZeroU64,
}

impl TransportEpoch {
    const fn initial(domain: TransportDomainId) -> Self {
        Self {
            domain,
            generation: NonZeroU64::MIN,
        }
    }

    const fn from_raw(domain: TransportDomainId, raw: u64) -> Result<Self, EpochRefusal> {
        match NonZeroU64::new(raw) {
            Some(generation) => Ok(Self { domain, generation }),
            None => Err(EpochRefusal::Zero),
        }
    }

    #[cfg(test)]
    pub(crate) const fn test_from_raw(
        domain: TransportDomainId,
        raw: u64,
    ) -> Result<Self, EpochRefusal> {
        Self::from_raw(domain, raw)
    }

    pub const fn domain(self) -> TransportDomainId {
        self.domain
    }

    pub const fn get(self) -> u64 {
        self.generation.get()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct TransportGeneration {
    epoch: TransportEpoch,
    resource_high_water: u32,
    context_high_water: u32,
    attachment_high_water: u64,
}

impl TransportGeneration {
    pub const fn bootstrap(root: TransportDomainRoot) -> Self {
        Self {
            epoch: TransportEpoch::initial(root.id),
            resource_high_water: 0,
            context_high_water: 0,
            attachment_high_water: 0,
        }
    }

    /// Safety: all high-water values cover every ID issued in this restored epoch,
    /// and no colliding reservation or lifecycle remains.
    pub const unsafe fn restore(
        root: TransportDomainRoot,
        epoch: u64,
        resource_high_water: u32,
        context_high_water: u32,
        attachment_high_water: u64,
    ) -> Result<Self, RefusedTransportRestore> {
        match TransportEpoch::from_raw(root.id, epoch) {
            Ok(epoch) => Ok(Self {
                epoch,
                resource_high_water,
                context_high_water,
                attachment_high_water,
            }),
            Err(reason) => Err(RefusedTransportRestore { reason, root }),
        }
    }

    pub const fn epoch(&self) -> TransportEpoch {
        self.epoch
    }

    pub const fn resource_high_water(&self) -> u32 {
        self.resource_high_water
    }

    pub const fn context_high_water(&self) -> u32 {
        self.context_high_water
    }

    pub const fn attachment_high_water(&self) -> u64 {
        self.attachment_high_water
    }

    pub fn allocate_resource(self) -> Result<ResourceAllocation, RefusedResourceAllocation> {
        let Some(raw) = self.resource_high_water.checked_add(1) else {
            return Err(RefusedResourceAllocation {
                reason: ResourceAllocationRefusal::Exhausted,
                generation: self,
            });
        };
        let Some(id) = NonZeroU32::new(raw) else {
            return Err(RefusedResourceAllocation {
                reason: ResourceAllocationRefusal::Exhausted,
                generation: self,
            });
        };
        let resource = TransportResource {
            epoch: self.epoch,
            id,
        };
        Ok(ResourceAllocation {
            generation: Self {
                epoch: self.epoch,
                resource_high_water: raw,
                context_high_water: self.context_high_water,
                attachment_high_water: self.attachment_high_water,
            },
            reservation: ResourceReservation { resource },
        })
    }

    pub fn allocate_context(self) -> Result<ContextAllocation, RefusedContextAllocation> {
        let Some(raw) = self.context_high_water.checked_add(1) else {
            return Err(RefusedContextAllocation {
                reason: ContextAllocationRefusal::Exhausted,
                generation: self,
            });
        };
        let Some(id) = NonZeroU32::new(raw) else {
            return Err(RefusedContextAllocation {
                reason: ContextAllocationRefusal::Exhausted,
                generation: self,
            });
        };
        let context = TransportContext {
            epoch: self.epoch,
            id,
        };
        Ok(ContextAllocation {
            generation: Self {
                epoch: self.epoch,
                resource_high_water: self.resource_high_water,
                context_high_water: raw,
                attachment_high_water: self.attachment_high_water,
            },
            reservation: ContextReservation { context },
        })
    }

    /// Safety: the caller's pointer-bound canonical table proves this pair absent
    /// and its exact context live, then retains that context, resource association
    /// custody, and pair uniqueness until the attachment reaches terminal.
    pub unsafe fn allocate_attachment(
        self,
        resource: TransportResource,
        context: TransportContext,
    ) -> Result<AttachmentAllocation, RefusedAttachmentAllocation> {
        if resource.epoch().domain() != self.epoch.domain() {
            return Err(RefusedAttachmentAllocation {
                reason: AttachmentAllocationRefusal::ResourceDomainMismatch {
                    expected: self.epoch.domain(),
                    found: resource.epoch().domain(),
                },
                generation: self,
            });
        }
        if context.epoch().domain() != self.epoch.domain() {
            return Err(RefusedAttachmentAllocation {
                reason: AttachmentAllocationRefusal::ContextDomainMismatch {
                    expected: self.epoch.domain(),
                    found: context.epoch().domain(),
                },
                generation: self,
            });
        }
        if resource.epoch() != self.epoch {
            return Err(RefusedAttachmentAllocation {
                reason: AttachmentAllocationRefusal::ResourceEpochMismatch {
                    expected: self.epoch,
                    found: resource.epoch(),
                },
                generation: self,
            });
        }
        if context.epoch() != self.epoch {
            return Err(RefusedAttachmentAllocation {
                reason: AttachmentAllocationRefusal::ContextEpochMismatch {
                    expected: self.epoch,
                    found: context.epoch(),
                },
                generation: self,
            });
        }
        let Some(raw) = self.attachment_high_water.checked_add(1) else {
            return Err(RefusedAttachmentAllocation {
                reason: AttachmentAllocationRefusal::Exhausted,
                generation: self,
            });
        };
        let Some(instance) = NonZeroU64::new(raw) else {
            return Err(RefusedAttachmentAllocation {
                reason: AttachmentAllocationRefusal::Exhausted,
                generation: self,
            });
        };
        let attachment = TransportAttachment {
            epoch: resource.epoch,
            resource_id: resource.id,
            context_id: context.id,
            instance,
        };
        Ok(AttachmentAllocation {
            generation: Self {
                epoch: self.epoch,
                resource_high_water: self.resource_high_water,
                context_high_water: self.context_high_water,
                attachment_high_water: raw,
            },
            reservation: AttachmentReservation { attachment },
        })
    }

    /// Safety: old-epoch submission admission is irreversibly closed and drained,
    /// no retained `PreparedControl` can enqueue after this reset is minted, and
    /// the device cannot DMA.
    pub unsafe fn advance(self) -> Result<TransportAdvance, RefusedTransportAdvance> {
        let Some(raw) = self.epoch.get().checked_add(1) else {
            return Err(RefusedTransportAdvance {
                reason: EpochRefusal::Exhausted,
                generation: self,
            });
        };
        let Some(epoch) = NonZeroU64::new(raw) else {
            return Err(RefusedTransportAdvance {
                reason: EpochRefusal::Exhausted,
                generation: self,
            });
        };
        Ok(TransportAdvance {
            generation: Self {
                epoch: TransportEpoch {
                    domain: self.epoch.domain(),
                    generation: epoch,
                },
                resource_high_water: 0,
                context_high_water: 0,
                attachment_high_water: 0,
            },
            reset: TransportReset {
                retired: self.epoch,
            },
        })
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedTransportRestore {
    reason: EpochRefusal,
    root: TransportDomainRoot,
}

impl RefusedTransportRestore {
    pub const fn reason(&self) -> EpochRefusal {
        self.reason
    }

    pub fn into_root(self) -> TransportDomainRoot {
        self.root
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct TransportAdvance {
    generation: TransportGeneration,
    reset: TransportReset,
}

impl TransportAdvance {
    pub const fn generation(&self) -> &TransportGeneration {
        &self.generation
    }

    pub const fn reset(&self) -> &TransportReset {
        &self.reset
    }

    pub fn into_parts(self) -> (TransportGeneration, TransportReset) {
        (self.generation, self.reset)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedTransportAdvance {
    reason: EpochRefusal,
    generation: TransportGeneration,
}

impl RefusedTransportAdvance {
    pub const fn reason(&self) -> EpochRefusal {
        self.reason
    }

    pub fn into_generation(self) -> TransportGeneration {
        self.generation
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct TransportReset {
    retired: TransportEpoch,
}

impl TransportReset {
    pub const fn retired_epoch(&self) -> TransportEpoch {
        self.retired
    }

    #[cfg(test)]
    pub(crate) const fn test_for_epoch(retired: TransportEpoch) -> Self {
        Self { retired }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceIdentityRefusal {
    Zero,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportResource {
    epoch: TransportEpoch,
    id: NonZeroU32,
}

impl TransportResource {
    #[cfg(test)]
    pub(crate) const fn from_raw(
        epoch: TransportEpoch,
        id: u32,
    ) -> Result<Self, ResourceIdentityRefusal> {
        match NonZeroU32::new(id) {
            Some(id) => Ok(Self { epoch, id }),
            None => Err(ResourceIdentityRefusal::Zero),
        }
    }

    pub const fn epoch(self) -> TransportEpoch {
        self.epoch
    }

    pub const fn id(self) -> u32 {
        self.id.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceAllocationRefusal {
    Exhausted,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedResourceAllocation {
    reason: ResourceAllocationRefusal,
    generation: TransportGeneration,
}

impl RefusedResourceAllocation {
    pub const fn reason(&self) -> ResourceAllocationRefusal {
        self.reason
    }

    pub const fn generation(&self) -> &TransportGeneration {
        &self.generation
    }

    pub fn into_generation(self) -> TransportGeneration {
        self.generation
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ResourceAllocation {
    generation: TransportGeneration,
    reservation: ResourceReservation,
}

impl ResourceAllocation {
    pub const fn resource(&self) -> TransportResource {
        self.reservation.resource
    }

    pub fn into_parts(self) -> (TransportGeneration, ResourceReservation) {
        (self.generation, self.reservation)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ResourceReservation {
    resource: TransportResource,
}

impl ResourceReservation {
    pub const fn resource(&self) -> TransportResource {
        self.resource
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextIdentityRefusal {
    Zero,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportContext {
    epoch: TransportEpoch,
    id: NonZeroU32,
}

impl TransportContext {
    #[cfg(test)]
    pub(crate) const fn from_raw(
        epoch: TransportEpoch,
        id: u32,
    ) -> Result<Self, ContextIdentityRefusal> {
        match NonZeroU32::new(id) {
            Some(id) => Ok(Self { epoch, id }),
            None => Err(ContextIdentityRefusal::Zero),
        }
    }

    pub const fn epoch(self) -> TransportEpoch {
        self.epoch
    }

    pub const fn id(self) -> u32 {
        self.id.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextAllocationRefusal {
    Exhausted,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedContextAllocation {
    reason: ContextAllocationRefusal,
    generation: TransportGeneration,
}

impl RefusedContextAllocation {
    pub const fn reason(&self) -> ContextAllocationRefusal {
        self.reason
    }

    pub const fn generation(&self) -> &TransportGeneration {
        &self.generation
    }

    pub fn into_generation(self) -> TransportGeneration {
        self.generation
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ContextAllocation {
    generation: TransportGeneration,
    reservation: ContextReservation,
}

impl ContextAllocation {
    pub const fn context(&self) -> TransportContext {
        self.reservation.context
    }

    pub fn into_parts(self) -> (TransportGeneration, ContextReservation) {
        (self.generation, self.reservation)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ContextReservation {
    context: TransportContext,
}

impl ContextReservation {
    pub const fn context(&self) -> TransportContext {
        self.context
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentAllocationRefusal {
    ResourceDomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    ContextDomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    ResourceEpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    ContextEpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    Exhausted,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedAttachmentAllocation {
    reason: AttachmentAllocationRefusal,
    generation: TransportGeneration,
}

impl RefusedAttachmentAllocation {
    pub const fn reason(&self) -> AttachmentAllocationRefusal {
        self.reason
    }

    pub const fn generation(&self) -> &TransportGeneration {
        &self.generation
    }

    pub fn into_generation(self) -> TransportGeneration {
        self.generation
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct AttachmentAllocation {
    generation: TransportGeneration,
    reservation: AttachmentReservation,
}

impl AttachmentAllocation {
    pub const fn attachment(&self) -> TransportAttachment {
        self.reservation.attachment
    }

    pub fn into_parts(self) -> (TransportGeneration, AttachmentReservation) {
        (self.generation, self.reservation)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct AttachmentReservation {
    attachment: TransportAttachment,
}

impl AttachmentReservation {
    pub const fn attachment(&self) -> TransportAttachment {
        self.attachment
    }

    #[cfg(test)]
    pub(crate) const fn test_for_attachment(attachment: TransportAttachment) -> Self {
        Self { attachment }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct AttachmentsClosed {
    resource: TransportResource,
}

impl AttachmentsClosed {
    /// Safety: canonical admission is closed, every creator and secondary pair
    /// for `resource` is absent, and admission stays closed until resource terminal.
    pub const unsafe fn new(resource: TransportResource) -> Self {
        Self { resource }
    }

    #[cfg(test)]
    pub(crate) const fn test_for_resource(resource: TransportResource) -> Self {
        Self { resource }
    }

    pub const fn resource(&self) -> TransportResource {
        self.resource
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportAttachment {
    epoch: TransportEpoch,
    resource_id: NonZeroU32,
    context_id: NonZeroU32,
    instance: NonZeroU64,
}

impl TransportAttachment {
    #[cfg(test)]
    pub(crate) const fn from_raw(
        resource: TransportResource,
        context: TransportContext,
        instance: NonZeroU64,
    ) -> Self {
        Self {
            epoch: resource.epoch,
            resource_id: resource.id,
            context_id: context.id,
            instance,
        }
    }

    pub const fn resource(self) -> TransportResource {
        TransportResource {
            epoch: self.epoch,
            id: self.resource_id,
        }
    }

    pub const fn context(self) -> TransportContext {
        TransportContext {
            epoch: self.epoch,
            id: self.context_id,
        }
    }

    pub const fn epoch(self) -> TransportEpoch {
        self.epoch
    }

    pub const fn instance(self) -> u64 {
        self.instance.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowIdentityRefusal {
    ZeroLength,
    RangeOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportWindow {
    resource: TransportResource,
    offset: u64,
    length: u64,
}

impl TransportWindow {
    pub const fn new(
        resource: TransportResource,
        offset: u64,
        length: u64,
    ) -> Result<Self, WindowIdentityRefusal> {
        if length == 0 {
            return Err(WindowIdentityRefusal::ZeroLength);
        }
        if offset.checked_add(length).is_none() {
            return Err(WindowIdentityRefusal::RangeOverflow);
        }
        Ok(Self {
            resource,
            offset,
            length,
        })
    }

    pub const fn resource(self) -> TransportResource {
        self.resource
    }

    pub const fn epoch(self) -> TransportEpoch {
        self.resource.epoch()
    }

    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn length(self) -> u64 {
        self.length
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostRejectionRefusal {
    UndocumentedResponse(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostRejection(u32);

impl HostRejection {
    pub const fn from_response_type(raw: u32) -> Result<Self, HostRejectionRefusal> {
        match raw {
            VIRTIO_GPU_RESP_ERR_UNSPEC
            | VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY
            | VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID
            | VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID
            | VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID
            | VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER => Ok(Self(raw)),
            _ => Err(HostRejectionRefusal::UndocumentedResponse(raw)),
        }
    }

    pub const fn response_type(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbandonReason {
    Timeout,
    NotOurs,
    TransportAborted,
    MalformedResponse,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct AbandonedControl {
    epoch: TransportEpoch,
    reason: AbandonReason,
}

impl AbandonedControl {
    const fn new(epoch: TransportEpoch, reason: AbandonReason) -> Self {
        Self { epoch, reason }
    }

    pub const fn epoch(&self) -> TransportEpoch {
        self.epoch
    }

    pub const fn reason(&self) -> AbandonReason {
        self.reason
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub enum ControlOutcome<T, E> {
    DefiniteNotEnqueued(E),
    Completed(Result<T, HostRejection>),
    Ambiguous(AbandonedControl),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlVerb {
    Create,
    Attach,
    Detach,
    Unref,
    Map,
    Unmap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlSubject {
    Resource(TransportResource),
    Attachment(TransportAttachment),
    Window(TransportWindow),
}

impl ControlSubject {
    pub const fn epoch(self) -> TransportEpoch {
        match self {
            Self::Resource(resource) => resource.epoch(),
            Self::Attachment(attachment) => attachment.epoch(),
            Self::Window(window) => window.epoch(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ControlKey {
    verb: ControlVerb,
    subject: ControlSubject,
    sequence: NonZeroU64,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct PreparedControl {
    key: ControlKey,
}

impl PreparedControl {
    pub(crate) const fn from_parts(
        verb: ControlVerb,
        subject: ControlSubject,
        sequence: NonZeroU64,
    ) -> Self {
        Self {
            key: ControlKey {
                verb,
                subject,
                sequence,
            },
        }
    }

    pub const fn verb(&self) -> ControlVerb {
        self.key.verb
    }

    pub const fn subject(&self) -> ControlSubject {
        self.key.subject
    }

    pub const fn sequence(&self) -> u64 {
        self.key.sequence.get()
    }

    /// Safety: the transport must prove this request was never accepted or enqueued.
    pub unsafe fn definite_not_enqueued<E>(self, error: E) -> ClassifiedControl<(), E> {
        ClassifiedControl {
            key: self.key,
            outcome: ControlOutcome::DefiniteNotEnqueued(error),
        }
    }

    /// Safety: a device response for this exact request must prove completed success.
    pub unsafe fn completed_ok<E>(self) -> ClassifiedControl<(), E> {
        ClassifiedControl {
            key: self.key,
            outcome: ControlOutcome::Completed(Ok(())),
        }
    }

    /// Safety: a device response for this exact request must carry this rejection.
    pub unsafe fn completed_rejection<E>(
        self,
        rejection: HostRejection,
    ) -> ClassifiedControl<(), E> {
        ClassifiedControl {
            key: self.key,
            outcome: ControlOutcome::Completed(Err(rejection)),
        }
    }

    pub fn ambiguous<E>(
        self,
        epoch: TransportEpoch,
        reason: AbandonReason,
    ) -> Result<ClassifiedControl<(), E>, RefusedControlClassification> {
        let expected = self.key.subject.epoch();
        if epoch.domain() != expected.domain() {
            return Err(RefusedControlClassification {
                reason: ControlClassificationRefusal::DomainMismatch {
                    expected: expected.domain(),
                    found: epoch.domain(),
                },
                request: self,
            });
        }
        if epoch != expected {
            return Err(RefusedControlClassification {
                reason: ControlClassificationRefusal::EpochMismatch {
                    expected,
                    found: epoch,
                },
                request: self,
            });
        }
        Ok(ClassifiedControl {
            key: self.key,
            outcome: ControlOutcome::Ambiguous(AbandonedControl::new(epoch, reason)),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlClassificationRefusal {
    DomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    EpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedControlClassification {
    reason: ControlClassificationRefusal,
    request: PreparedControl,
}

impl RefusedControlClassification {
    pub const fn reason(&self) -> ControlClassificationRefusal {
        self.reason
    }

    pub fn into_request(self) -> PreparedControl {
        self.request
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ClassifiedControl<T, E> {
    key: ControlKey,
    outcome: ControlOutcome<T, E>,
}

impl<T, E> ClassifiedControl<T, E> {
    pub const fn verb(&self) -> ControlVerb {
        self.key.verb
    }

    pub const fn subject(&self) -> ControlSubject {
        self.key.subject
    }

    pub const fn sequence(&self) -> u64 {
        self.key.sequence.get()
    }

    pub const fn outcome(&self) -> &ControlOutcome<T, E> {
        &self.outcome
    }

    pub fn into_outcome(self) -> ControlOutcome<T, E> {
        self.outcome
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourcePhase {
    Reserved,
    CreatePending,
    Created,
    AttachPending,
    Live,
    DetachPending,
    Detached,
    RetirementRequired,
    UnrefPending,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceOperation {
    Create,
    Attach,
    Detach,
    Unref,
    CancelReservation,
    TransportReset,
    ConsumeTerminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceRefusal {
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
    ResetEpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    ResetDomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    WrongPhase {
        operation: ResourceOperation,
        phase: ResourcePhase,
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
    MissingAttachment,
    AttachmentMayBeLive,
    AttachmentsAlreadyClosed,
    AttachmentsNotClosed,
    AlreadyTerminal,
    TerminalAuthorityMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceBeginEffect {
    CreateStarted,
    AttachStarted,
    DetachStarted,
    UnrefStarted,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ResourceBegin {
    effect: ResourceBeginEffect,
    request: PreparedControl,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedResourceAttachBegin {
    reason: ResourceRefusal,
    reservation: AttachmentReservation,
}

impl RefusedResourceAttachBegin {
    pub const fn reason(&self) -> ResourceRefusal {
        self.reason
    }

    pub fn into_reservation(self) -> AttachmentReservation {
        self.reservation
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedAttachmentsClosed {
    reason: ResourceRefusal,
    witness: AttachmentsClosed,
}

impl RefusedAttachmentsClosed {
    pub const fn reason(&self) -> ResourceRefusal {
        self.reason
    }

    pub fn into_witness(self) -> AttachmentsClosed {
        self.witness
    }
}

impl ResourceBegin {
    pub const fn effect(&self) -> ResourceBeginEffect {
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
pub enum ResourceFinishEffect<E> {
    CreateDefiniteNotEnqueued(E),
    CreateCompleted,
    CreateHostRejected(HostRejection),
    CreateAmbiguous(AbandonReason),
    AttachDefiniteNotEnqueued(E),
    AttachCompleted,
    AttachHostRejected(HostRejection),
    AttachAmbiguous(AbandonReason),
    DetachDefiniteNotEnqueued(E),
    DetachCompleted,
    DetachHostRejected(HostRejection),
    DetachAmbiguous(AbandonReason),
    UnrefDefiniteNotEnqueued(E),
    UnrefCompleted,
    UnrefHostRejected(HostRejection),
    UnrefAmbiguous(AbandonReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestroyAuthorityKind {
    ReservationCancelled,
    CreateDefiniteNotEnqueued,
    CreateHostRejected(HostRejection),
    UnrefCompleted,
    TransportReset(TransportEpoch),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct DestroyBacking {
    resource: TransportResource,
    authority: DestroyAuthorityKind,
}

impl DestroyBacking {
    pub const fn resource(&self) -> TransportResource {
        self.resource
    }

    pub const fn authority(&self) -> DestroyAuthorityKind {
        self.authority
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ResourceFinish<E> {
    effect: ResourceFinishEffect<E>,
    destroy: Option<DestroyBacking>,
}

impl<E> ResourceFinish<E> {
    pub const fn effect(&self) -> &ResourceFinishEffect<E> {
        &self.effect
    }

    pub const fn destroy_authority(&self) -> Option<&DestroyBacking> {
        self.destroy.as_ref()
    }

    pub fn into_parts(self) -> (ResourceFinishEffect<E>, Option<DestroyBacking>) {
        (self.effect, self.destroy)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedResourceOutcome<T, E> {
    reason: ResourceRefusal,
    completion: ClassifiedControl<T, E>,
}

impl<T, E> RefusedResourceOutcome<T, E> {
    pub const fn reason(&self) -> ResourceRefusal {
        self.reason
    }

    pub fn into_completion(self) -> ClassifiedControl<T, E> {
        self.completion
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct ResourceLifecycle<B> {
    resource: TransportResource,
    attachment: Option<AttachmentReservation>,
    attachment_may_be_live: bool,
    attachments_closed: Option<AttachmentsClosed>,
    phase: ResourcePhase,
    uncertain: bool,
    control_high_water: u64,
    pending: Option<ControlKey>,
    terminal_authority: Option<DestroyAuthorityKind>,
    backing: ManuallyDrop<B>,
}

impl<B> ResourceLifecycle<B> {
    pub const fn new(reservation: ResourceReservation, backing: B) -> Self {
        Self {
            resource: reservation.resource,
            attachment: None,
            attachment_may_be_live: false,
            attachments_closed: None,
            phase: ResourcePhase::Reserved,
            uncertain: false,
            control_high_water: 0,
            pending: None,
            terminal_authority: None,
            backing: ManuallyDrop::new(backing),
        }
    }

    pub const fn resource(&self) -> TransportResource {
        self.resource
    }

    pub const fn attachment(&self) -> Option<TransportAttachment> {
        match self.attachment.as_ref() {
            Some(reservation) => Some(reservation.attachment),
            None => None,
        }
    }

    pub const fn attachment_may_be_live(&self) -> bool {
        self.attachment_may_be_live
    }

    pub const fn attachments_are_closed(&self) -> bool {
        self.attachments_closed.is_some()
    }

    pub const fn phase(&self) -> ResourcePhase {
        self.phase
    }

    pub const fn is_uncertain(&self) -> bool {
        self.uncertain
    }

    pub const fn control_high_water(&self) -> u64 {
        self.control_high_water
    }

    pub fn begin_create(
        &mut self,
        resource: TransportResource,
    ) -> Result<ResourceBegin, ResourceRefusal> {
        self.check_begin(resource, ResourceOperation::Create, ResourcePhase::Reserved)?;
        let request =
            self.mint_control(ControlVerb::Create, ControlSubject::Resource(self.resource))?;
        self.phase = ResourcePhase::CreatePending;
        Ok(ResourceBegin {
            effect: ResourceBeginEffect::CreateStarted,
            request,
        })
    }

    pub fn finish_create<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<ResourceFinish<E>, RefusedResourceOutcome<(), E>> {
        if let Err(reason) = self.check_finish(
            ResourceOperation::Create,
            ResourcePhase::CreatePending,
            ControlVerb::Create,
            &completion,
        ) {
            return Err(RefusedResourceOutcome { reason, completion });
        }
        self.pending = None;

        let (effect, authority) = match completion.outcome {
            ControlOutcome::DefiniteNotEnqueued(error) => (
                ResourceFinishEffect::CreateDefiniteNotEnqueued(error),
                DestroyAuthorityKind::CreateDefiniteNotEnqueued,
            ),
            ControlOutcome::Completed(Ok(())) => {
                self.phase = ResourcePhase::Created;
                return Ok(ResourceFinish {
                    effect: ResourceFinishEffect::CreateCompleted,
                    destroy: None,
                });
            }
            ControlOutcome::Completed(Err(status)) => (
                ResourceFinishEffect::CreateHostRejected(status),
                DestroyAuthorityKind::CreateHostRejected(status),
            ),
            ControlOutcome::Ambiguous(abandoned) => {
                self.phase = ResourcePhase::RetirementRequired;
                self.uncertain = true;
                return Ok(ResourceFinish {
                    effect: ResourceFinishEffect::CreateAmbiguous(abandoned.reason()),
                    destroy: None,
                });
            }
        };
        self.terminalize(authority);
        Ok(ResourceFinish {
            effect,
            destroy: Some(self.destroy_token(authority)),
        })
    }

    pub fn begin_attach(
        &mut self,
        reservation: AttachmentReservation,
    ) -> Result<ResourceBegin, RefusedResourceAttachBegin> {
        let attachment = reservation.attachment;
        if let Err(reason) = self.check_resource(attachment.resource()) {
            return Err(RefusedResourceAttachBegin {
                reason,
                reservation,
            });
        }
        if self.phase == ResourcePhase::Terminal {
            return Err(RefusedResourceAttachBegin {
                reason: ResourceRefusal::AlreadyTerminal,
                reservation,
            });
        }
        if self.attachments_closed.is_some() {
            return Err(RefusedResourceAttachBegin {
                reason: ResourceRefusal::AttachmentsAlreadyClosed,
                reservation,
            });
        }
        if self.phase != ResourcePhase::Created {
            return Err(RefusedResourceAttachBegin {
                reason: ResourceRefusal::WrongPhase {
                    operation: ResourceOperation::Attach,
                    phase: self.phase,
                },
                reservation,
            });
        }
        let request =
            match self.mint_control(ControlVerb::Attach, ControlSubject::Attachment(attachment)) {
                Ok(request) => request,
                Err(reason) => {
                    return Err(RefusedResourceAttachBegin {
                        reason,
                        reservation,
                    });
                }
            };
        self.attachment = Some(reservation);
        self.attachment_may_be_live = true;
        self.phase = ResourcePhase::AttachPending;
        Ok(ResourceBegin {
            effect: ResourceBeginEffect::AttachStarted,
            request,
        })
    }

    pub fn finish_attach<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<ResourceFinish<E>, RefusedResourceOutcome<(), E>> {
        if let Err(reason) = self.check_finish(
            ResourceOperation::Attach,
            ResourcePhase::AttachPending,
            ControlVerb::Attach,
            &completion,
        ) {
            return Err(RefusedResourceOutcome { reason, completion });
        }
        self.pending = None;

        let effect = match completion.outcome {
            ControlOutcome::DefiniteNotEnqueued(error) => {
                self.attachment_may_be_live = false;
                ResourceFinishEffect::AttachDefiniteNotEnqueued(error)
            }
            ControlOutcome::Completed(Ok(())) => {
                self.phase = ResourcePhase::Live;
                return Ok(ResourceFinish {
                    effect: ResourceFinishEffect::AttachCompleted,
                    destroy: None,
                });
            }
            ControlOutcome::Completed(Err(status)) => {
                self.attachment_may_be_live = false;
                ResourceFinishEffect::AttachHostRejected(status)
            }
            ControlOutcome::Ambiguous(abandoned) => {
                self.uncertain = true;
                ResourceFinishEffect::AttachAmbiguous(abandoned.reason())
            }
        };
        self.phase = ResourcePhase::RetirementRequired;
        Ok(ResourceFinish {
            effect,
            destroy: None,
        })
    }

    pub fn begin_detach(
        &mut self,
        attachment: TransportAttachment,
    ) -> Result<ResourceBegin, ResourceRefusal> {
        self.check_resource(attachment.resource())?;
        if self.phase == ResourcePhase::Terminal {
            return Err(ResourceRefusal::AlreadyTerminal);
        }
        if !matches!(
            self.phase,
            ResourcePhase::Live | ResourcePhase::RetirementRequired
        ) {
            return Err(ResourceRefusal::WrongPhase {
                operation: ResourceOperation::Detach,
                phase: self.phase,
            });
        }
        self.check_attachment(attachment)?;
        if !self.attachment_may_be_live {
            return Err(ResourceRefusal::WrongPhase {
                operation: ResourceOperation::Detach,
                phase: self.phase,
            });
        }
        let request =
            self.mint_control(ControlVerb::Detach, ControlSubject::Attachment(attachment))?;
        self.phase = ResourcePhase::DetachPending;
        Ok(ResourceBegin {
            effect: ResourceBeginEffect::DetachStarted,
            request,
        })
    }

    pub fn finish_detach<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<ResourceFinish<E>, RefusedResourceOutcome<(), E>> {
        if let Err(reason) = self.check_finish(
            ResourceOperation::Detach,
            ResourcePhase::DetachPending,
            ControlVerb::Detach,
            &completion,
        ) {
            return Err(RefusedResourceOutcome { reason, completion });
        }
        self.pending = None;

        let effect = match completion.outcome {
            ControlOutcome::DefiniteNotEnqueued(error) => {
                ResourceFinishEffect::DetachDefiniteNotEnqueued(error)
            }
            ControlOutcome::Completed(Ok(())) => {
                self.attachment_may_be_live = false;
                self.phase = ResourcePhase::Detached;
                return Ok(ResourceFinish {
                    effect: ResourceFinishEffect::DetachCompleted,
                    destroy: None,
                });
            }
            ControlOutcome::Completed(Err(status)) => {
                ResourceFinishEffect::DetachHostRejected(status)
            }
            ControlOutcome::Ambiguous(abandoned) => {
                self.uncertain = true;
                ResourceFinishEffect::DetachAmbiguous(abandoned.reason())
            }
        };
        self.phase = ResourcePhase::RetirementRequired;
        Ok(ResourceFinish {
            effect,
            destroy: None,
        })
    }

    pub fn begin_unref(
        &mut self,
        resource: TransportResource,
    ) -> Result<ResourceBegin, ResourceRefusal> {
        self.check_resource(resource)?;
        if self.phase == ResourcePhase::Terminal {
            return Err(ResourceRefusal::AlreadyTerminal);
        }
        if self.attachment_may_be_live {
            return Err(ResourceRefusal::AttachmentMayBeLive);
        }
        if self.attachments_closed.is_none() {
            return Err(ResourceRefusal::AttachmentsNotClosed);
        }
        if !matches!(
            self.phase,
            ResourcePhase::Created
                | ResourcePhase::Live
                | ResourcePhase::Detached
                | ResourcePhase::RetirementRequired
        ) {
            return Err(ResourceRefusal::WrongPhase {
                operation: ResourceOperation::Unref,
                phase: self.phase,
            });
        }
        let request =
            self.mint_control(ControlVerb::Unref, ControlSubject::Resource(self.resource))?;
        self.phase = ResourcePhase::UnrefPending;
        Ok(ResourceBegin {
            effect: ResourceBeginEffect::UnrefStarted,
            request,
        })
    }

    pub fn install_attachments_closed(
        &mut self,
        witness: AttachmentsClosed,
    ) -> Result<(), RefusedAttachmentsClosed> {
        let reason = self
            .check_resource(witness.resource)
            .and_then(|()| {
                if self.phase == ResourcePhase::Terminal {
                    Err(ResourceRefusal::AlreadyTerminal)
                } else if self.attachment_may_be_live {
                    Err(ResourceRefusal::AttachmentMayBeLive)
                } else if self.attachments_closed.is_some() {
                    Err(ResourceRefusal::AttachmentsAlreadyClosed)
                } else {
                    Ok(())
                }
            })
            .err();
        if let Some(reason) = reason {
            return Err(RefusedAttachmentsClosed { reason, witness });
        }
        self.attachment = None;
        self.attachments_closed = Some(witness);
        Ok(())
    }

    pub fn finish_unref<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<ResourceFinish<E>, RefusedResourceOutcome<(), E>> {
        if let Err(reason) = self.check_finish(
            ResourceOperation::Unref,
            ResourcePhase::UnrefPending,
            ControlVerb::Unref,
            &completion,
        ) {
            return Err(RefusedResourceOutcome { reason, completion });
        }
        self.pending = None;

        let effect = match completion.outcome {
            ControlOutcome::DefiniteNotEnqueued(error) => {
                ResourceFinishEffect::UnrefDefiniteNotEnqueued(error)
            }
            ControlOutcome::Completed(Ok(())) => {
                let authority = DestroyAuthorityKind::UnrefCompleted;
                self.terminalize(authority);
                return Ok(ResourceFinish {
                    effect: ResourceFinishEffect::UnrefCompleted,
                    destroy: Some(self.destroy_token(authority)),
                });
            }
            ControlOutcome::Completed(Err(status)) => {
                ResourceFinishEffect::UnrefHostRejected(status)
            }
            ControlOutcome::Ambiguous(abandoned) => {
                self.uncertain = true;
                ResourceFinishEffect::UnrefAmbiguous(abandoned.reason())
            }
        };
        self.phase = ResourcePhase::RetirementRequired;
        Ok(ResourceFinish {
            effect,
            destroy: None,
        })
    }

    pub fn cancel_reservation(
        &mut self,
        resource: TransportResource,
    ) -> Result<DestroyBacking, ResourceRefusal> {
        self.check_begin(
            resource,
            ResourceOperation::CancelReservation,
            ResourcePhase::Reserved,
        )?;
        let authority = DestroyAuthorityKind::ReservationCancelled;
        self.terminalize(authority);
        Ok(self.destroy_token(authority))
    }

    pub fn transport_reset(
        &mut self,
        resource: TransportResource,
        reset: &TransportReset,
    ) -> Result<DestroyBacking, ResourceRefusal> {
        self.check_resource(resource)?;
        if reset.retired_epoch().domain() != self.resource.epoch().domain() {
            return Err(ResourceRefusal::ResetDomainMismatch {
                expected: self.resource.epoch().domain(),
                found: reset.retired_epoch().domain(),
            });
        }
        if reset.retired_epoch() != self.resource.epoch() {
            return Err(ResourceRefusal::ResetEpochMismatch {
                expected: self.resource.epoch(),
                found: reset.retired_epoch(),
            });
        }
        if self.phase == ResourcePhase::Terminal {
            return Err(ResourceRefusal::AlreadyTerminal);
        }
        let authority = DestroyAuthorityKind::TransportReset(reset.retired_epoch());
        self.terminalize(authority);
        Ok(self.destroy_token(authority))
    }

    pub fn consume_terminal(
        self,
        authority: DestroyBacking,
    ) -> Result<B, RefusedDestroyBacking<B>> {
        let reason = self
            .check_resource(authority.resource)
            .and_then(|()| {
                if self.phase != ResourcePhase::Terminal {
                    Err(ResourceRefusal::WrongPhase {
                        operation: ResourceOperation::ConsumeTerminal,
                        phase: self.phase,
                    })
                } else if self.terminal_authority != Some(authority.authority) {
                    Err(ResourceRefusal::TerminalAuthorityMismatch)
                } else {
                    Ok(())
                }
            })
            .err();
        if let Some(reason) = reason {
            return Err(RefusedDestroyBacking {
                reason,
                lifecycle: self,
                authority,
            });
        }
        Ok(ManuallyDrop::into_inner(self.backing))
    }

    fn check_begin(
        &self,
        resource: TransportResource,
        operation: ResourceOperation,
        expected: ResourcePhase,
    ) -> Result<(), ResourceRefusal> {
        self.check_resource(resource)?;
        if self.phase == ResourcePhase::Terminal {
            return Err(ResourceRefusal::AlreadyTerminal);
        }
        if self.phase != expected {
            return Err(ResourceRefusal::WrongPhase {
                operation,
                phase: self.phase,
            });
        }
        Ok(())
    }

    fn check_finish<T, E>(
        &self,
        operation: ResourceOperation,
        expected_phase: ResourcePhase,
        expected_verb: ControlVerb,
        completion: &ClassifiedControl<T, E>,
    ) -> Result<(), ResourceRefusal> {
        if self.phase == ResourcePhase::Terminal {
            return Err(ResourceRefusal::AlreadyTerminal);
        }
        if self.phase != expected_phase {
            return Err(ResourceRefusal::WrongPhase {
                operation,
                phase: self.phase,
            });
        }
        let Some(pending) = self.pending else {
            return Err(ResourceRefusal::MissingPendingControl);
        };
        if pending.verb != expected_verb {
            return Err(ResourceRefusal::ControlVerbMismatch {
                expected: expected_verb,
                found: pending.verb,
            });
        }
        let expected_epoch = pending.subject.epoch();
        let found_epoch = completion.key.subject.epoch();
        if found_epoch.domain() != expected_epoch.domain() {
            return Err(ResourceRefusal::DomainMismatch {
                expected: expected_epoch.domain(),
                found: found_epoch.domain(),
            });
        }
        if found_epoch != expected_epoch {
            return Err(ResourceRefusal::EpochMismatch {
                expected: expected_epoch,
                found: found_epoch,
            });
        }
        self.check_control_subject(pending.subject, completion.key.subject)?;
        if completion.key.verb != pending.verb {
            return Err(ResourceRefusal::ControlVerbMismatch {
                expected: pending.verb,
                found: completion.key.verb,
            });
        }
        if completion.key.sequence != pending.sequence {
            return Err(ResourceRefusal::ControlSequenceMismatch {
                expected: pending.sequence.get(),
                found: completion.key.sequence.get(),
            });
        }
        Ok(())
    }

    fn mint_control(
        &mut self,
        verb: ControlVerb,
        subject: ControlSubject,
    ) -> Result<PreparedControl, ResourceRefusal> {
        if self.pending.is_some() {
            return Err(ResourceRefusal::ControlAlreadyPending);
        }
        let Some(raw) = self.control_high_water.checked_add(1) else {
            return Err(ResourceRefusal::ControlSequenceExhausted);
        };
        let Some(sequence) = NonZeroU64::new(raw) else {
            return Err(ResourceRefusal::ControlSequenceExhausted);
        };
        let key = ControlKey {
            verb,
            subject,
            sequence,
        };
        self.control_high_water = raw;
        self.pending = Some(key);
        Ok(PreparedControl { key })
    }

    fn check_resource(&self, resource: TransportResource) -> Result<(), ResourceRefusal> {
        if resource.epoch().domain() != self.resource.epoch().domain() {
            return Err(ResourceRefusal::DomainMismatch {
                expected: self.resource.epoch().domain(),
                found: resource.epoch().domain(),
            });
        }
        if resource.epoch() != self.resource.epoch() {
            return Err(ResourceRefusal::EpochMismatch {
                expected: self.resource.epoch(),
                found: resource.epoch(),
            });
        }
        if resource.id() != self.resource.id() {
            return Err(ResourceRefusal::ResourceMismatch {
                expected: self.resource.id(),
                found: resource.id(),
            });
        }
        Ok(())
    }

    fn check_attachment(&self, attachment: TransportAttachment) -> Result<(), ResourceRefusal> {
        self.check_resource(attachment.resource())?;
        let Some(expected) = self.attachment.as_ref() else {
            return Err(ResourceRefusal::MissingAttachment);
        };
        let expected = expected.attachment;
        if attachment.context().id() != expected.context().id() {
            return Err(ResourceRefusal::ContextMismatch {
                expected: expected.context().id(),
                found: attachment.context().id(),
            });
        }
        if attachment.instance() != expected.instance() {
            return Err(ResourceRefusal::AttachmentInstanceMismatch {
                expected: expected.instance(),
                found: attachment.instance(),
            });
        }
        Ok(())
    }

    fn check_control_subject(
        &self,
        expected: ControlSubject,
        found: ControlSubject,
    ) -> Result<(), ResourceRefusal> {
        match (expected, found) {
            (ControlSubject::Attachment(expected), ControlSubject::Attachment(found)) => {
                self.check_resource(found.resource())?;
                if found.resource().id() != expected.resource().id() {
                    return Err(ResourceRefusal::ResourceMismatch {
                        expected: expected.resource().id(),
                        found: found.resource().id(),
                    });
                }
                if found.context().id() != expected.context().id() {
                    return Err(ResourceRefusal::ContextMismatch {
                        expected: expected.context().id(),
                        found: found.context().id(),
                    });
                }
                if found.instance() != expected.instance() {
                    return Err(ResourceRefusal::AttachmentInstanceMismatch {
                        expected: expected.instance(),
                        found: found.instance(),
                    });
                }
                Ok(())
            }
            _ if expected == found => Ok(()),
            _ => Err(ResourceRefusal::ControlSubjectMismatch),
        }
    }

    fn terminalize(&mut self, authority: DestroyAuthorityKind) {
        self.phase = ResourcePhase::Terminal;
        self.attachment_may_be_live = false;
        self.attachments_closed = None;
        self.pending = None;
        self.terminal_authority = Some(authority);
    }

    const fn destroy_token(&self, authority: DestroyAuthorityKind) -> DestroyBacking {
        DestroyBacking {
            resource: self.resource,
            authority,
        }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedDestroyBacking<B> {
    reason: ResourceRefusal,
    lifecycle: ResourceLifecycle<B>,
    authority: DestroyBacking,
}

impl<B> RefusedDestroyBacking<B> {
    pub const fn reason(&self) -> ResourceRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (ResourceLifecycle<B>, DestroyBacking) {
        (self.lifecycle, self.authority)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowPhase {
    Unmapped,
    MapPending,
    Mapped,
    MapUncertain,
    UnmapUncertain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowOperation {
    FinishMap,
    BeginUnmap,
    FinishUnmap,
    TransportReset,
    ConsumeUnmapped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowRefusal {
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
    RangeMismatch {
        expected_offset: u64,
        expected_length: u64,
        found_offset: u64,
        found_length: u64,
    },
    ResetEpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    ResetDomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    WrongPhase {
        operation: WindowOperation,
        phase: WindowPhase,
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
    UnmapAlreadyInFlight,
    UnmapNotInFlight,
    AlreadyReleased,
    ReleaseAuthorityMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowBeginEffect {
    UnmapStarted,
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct WindowBegin {
    effect: WindowBeginEffect,
    request: PreparedControl,
}

impl WindowBegin {
    pub const fn effect(&self) -> WindowBeginEffect {
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
pub enum WindowFinishEffect<E> {
    MapDefiniteNotEnqueued(E),
    MapCompleted,
    MapHostRejected(HostRejection),
    MapAmbiguous(AbandonReason),
    UnmapDefiniteNotEnqueued(E),
    UnmapCompleted,
    UnmapHostRejected(HostRejection),
    UnmapAmbiguous(AbandonReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowReleaseAuthorityKind {
    MapDefiniteNotEnqueued,
    MapHostRejected(HostRejection),
    UnmapCompleted,
    TransportReset(TransportEpoch),
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct ReleaseWindow {
    window: TransportWindow,
    authority: WindowReleaseAuthorityKind,
}

impl ReleaseWindow {
    pub const fn window(&self) -> TransportWindow {
        self.window
    }

    pub const fn authority(&self) -> WindowReleaseAuthorityKind {
        self.authority
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct WindowFinish<E> {
    effect: WindowFinishEffect<E>,
    release: Option<ReleaseWindow>,
}

impl<E> WindowFinish<E> {
    pub const fn effect(&self) -> &WindowFinishEffect<E> {
        &self.effect
    }

    pub const fn release_authority(&self) -> Option<&ReleaseWindow> {
        self.release.as_ref()
    }

    pub fn into_parts(self) -> (WindowFinishEffect<E>, Option<ReleaseWindow>) {
        (self.effect, self.release)
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedWindowOutcome<T, E> {
    reason: WindowRefusal,
    completion: ClassifiedControl<T, E>,
}

impl<T, E> RefusedWindowOutcome<T, E> {
    pub const fn reason(&self) -> WindowRefusal {
        self.reason
    }

    pub fn into_completion(self) -> ClassifiedControl<T, E> {
        self.completion
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct WindowAdmission<R> {
    lifecycle: WindowLifecycle<R>,
    request: PreparedControl,
}

impl<R> WindowAdmission<R> {
    pub const fn lifecycle(&self) -> &WindowLifecycle<R> {
        &self.lifecycle
    }

    pub const fn request(&self) -> &PreparedControl {
        &self.request
    }

    pub fn into_parts(self) -> (WindowLifecycle<R>, PreparedControl) {
        (self.lifecycle, self.request)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct WindowLifecycle<R> {
    window: TransportWindow,
    phase: WindowPhase,
    unmap_in_flight: bool,
    control_high_water: u64,
    pending: Option<ControlKey>,
    release_authority: Option<WindowReleaseAuthorityKind>,
    reservation: ManuallyDrop<R>,
}

impl<R> WindowLifecycle<R> {
    /// Safety: the allocator proves this fresh, bounded range does not overlap another owner;
    /// no prior MAP for it was accepted, and `reservation` is its sole live witness.
    pub const unsafe fn reserve(window: TransportWindow, reservation: R) -> WindowAdmission<R> {
        let key = ControlKey {
            verb: ControlVerb::Map,
            subject: ControlSubject::Window(window),
            sequence: NonZeroU64::MIN,
        };
        WindowAdmission {
            lifecycle: Self {
                window,
                phase: WindowPhase::MapPending,
                unmap_in_flight: false,
                control_high_water: 1,
                pending: Some(key),
                release_authority: None,
                reservation: ManuallyDrop::new(reservation),
            },
            request: PreparedControl { key },
        }
    }

    pub const fn window(&self) -> TransportWindow {
        self.window
    }

    pub const fn phase(&self) -> WindowPhase {
        self.phase
    }

    pub const fn is_unmap_in_flight(&self) -> bool {
        self.unmap_in_flight
    }

    pub const fn control_high_water(&self) -> u64 {
        self.control_high_water
    }

    pub fn finish_map<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<WindowFinish<E>, RefusedWindowOutcome<(), E>> {
        if let Err(reason) = self.check_finish(
            WindowOperation::FinishMap,
            WindowPhase::MapPending,
            ControlVerb::Map,
            &completion,
        ) {
            return Err(RefusedWindowOutcome { reason, completion });
        }
        self.pending = None;

        let (effect, authority) = match completion.outcome {
            ControlOutcome::DefiniteNotEnqueued(error) => (
                WindowFinishEffect::MapDefiniteNotEnqueued(error),
                WindowReleaseAuthorityKind::MapDefiniteNotEnqueued,
            ),
            ControlOutcome::Completed(Ok(())) => {
                self.phase = WindowPhase::Mapped;
                return Ok(WindowFinish {
                    effect: WindowFinishEffect::MapCompleted,
                    release: None,
                });
            }
            ControlOutcome::Completed(Err(status)) => (
                WindowFinishEffect::MapHostRejected(status),
                WindowReleaseAuthorityKind::MapHostRejected(status),
            ),
            ControlOutcome::Ambiguous(abandoned) => {
                self.phase = WindowPhase::MapUncertain;
                return Ok(WindowFinish {
                    effect: WindowFinishEffect::MapAmbiguous(abandoned.reason()),
                    release: None,
                });
            }
        };
        self.release(authority);
        Ok(WindowFinish {
            effect,
            release: Some(self.release_token(authority)),
        })
    }

    pub fn begin_unmap(&mut self, window: TransportWindow) -> Result<WindowBegin, WindowRefusal> {
        self.check_window(window)?;
        match self.phase {
            WindowPhase::Unmapped => return Err(WindowRefusal::AlreadyReleased),
            WindowPhase::Mapped | WindowPhase::MapUncertain => {}
            WindowPhase::UnmapUncertain if self.unmap_in_flight => {
                return Err(WindowRefusal::UnmapAlreadyInFlight)
            }
            WindowPhase::UnmapUncertain => {}
            phase => {
                return Err(WindowRefusal::WrongPhase {
                    operation: WindowOperation::BeginUnmap,
                    phase,
                })
            }
        }
        let request = self.mint_control(ControlVerb::Unmap)?;
        self.phase = WindowPhase::UnmapUncertain;
        self.unmap_in_flight = true;
        Ok(WindowBegin {
            effect: WindowBeginEffect::UnmapStarted,
            request,
        })
    }

    pub fn finish_unmap<E>(
        &mut self,
        completion: ClassifiedControl<(), E>,
    ) -> Result<WindowFinish<E>, RefusedWindowOutcome<(), E>> {
        if self.phase == WindowPhase::Unmapped {
            return Err(RefusedWindowOutcome {
                reason: WindowRefusal::AlreadyReleased,
                completion,
            });
        }
        if self.phase != WindowPhase::UnmapUncertain {
            return Err(RefusedWindowOutcome {
                reason: WindowRefusal::WrongPhase {
                    operation: WindowOperation::FinishUnmap,
                    phase: self.phase,
                },
                completion,
            });
        }
        if !self.unmap_in_flight {
            return Err(RefusedWindowOutcome {
                reason: WindowRefusal::UnmapNotInFlight,
                completion,
            });
        }
        if let Err(reason) = self.check_control(ControlVerb::Unmap, &completion) {
            return Err(RefusedWindowOutcome { reason, completion });
        }

        self.pending = None;
        self.unmap_in_flight = false;
        let effect = match completion.outcome {
            ControlOutcome::DefiniteNotEnqueued(error) => {
                WindowFinishEffect::UnmapDefiniteNotEnqueued(error)
            }
            ControlOutcome::Completed(Ok(())) => {
                let authority = WindowReleaseAuthorityKind::UnmapCompleted;
                self.release(authority);
                return Ok(WindowFinish {
                    effect: WindowFinishEffect::UnmapCompleted,
                    release: Some(self.release_token(authority)),
                });
            }
            ControlOutcome::Completed(Err(status)) => WindowFinishEffect::UnmapHostRejected(status),
            ControlOutcome::Ambiguous(abandoned) => {
                WindowFinishEffect::UnmapAmbiguous(abandoned.reason())
            }
        };
        Ok(WindowFinish {
            effect,
            release: None,
        })
    }

    pub fn transport_reset(
        &mut self,
        window: TransportWindow,
        reset: &TransportReset,
    ) -> Result<ReleaseWindow, WindowRefusal> {
        self.check_window(window)?;
        if reset.retired_epoch().domain() != self.window.epoch().domain() {
            return Err(WindowRefusal::ResetDomainMismatch {
                expected: self.window.epoch().domain(),
                found: reset.retired_epoch().domain(),
            });
        }
        if reset.retired_epoch() != self.window.epoch() {
            return Err(WindowRefusal::ResetEpochMismatch {
                expected: self.window.epoch(),
                found: reset.retired_epoch(),
            });
        }
        if self.phase == WindowPhase::Unmapped {
            return Err(WindowRefusal::AlreadyReleased);
        }
        let authority = WindowReleaseAuthorityKind::TransportReset(reset.retired_epoch());
        self.release(authority);
        Ok(self.release_token(authority))
    }

    pub fn consume_unmapped(self, authority: ReleaseWindow) -> Result<R, RefusedReleaseWindow<R>> {
        let reason = self
            .check_window(authority.window)
            .and_then(|()| {
                if self.phase != WindowPhase::Unmapped {
                    Err(WindowRefusal::WrongPhase {
                        operation: WindowOperation::ConsumeUnmapped,
                        phase: self.phase,
                    })
                } else if self.release_authority != Some(authority.authority) {
                    Err(WindowRefusal::ReleaseAuthorityMismatch)
                } else {
                    Ok(())
                }
            })
            .err();
        if let Some(reason) = reason {
            return Err(RefusedReleaseWindow {
                reason,
                lifecycle: self,
                authority,
            });
        }
        Ok(ManuallyDrop::into_inner(self.reservation))
    }

    fn check_finish<T, E>(
        &self,
        operation: WindowOperation,
        expected_phase: WindowPhase,
        expected_verb: ControlVerb,
        completion: &ClassifiedControl<T, E>,
    ) -> Result<(), WindowRefusal> {
        if self.phase == WindowPhase::Unmapped {
            return Err(WindowRefusal::AlreadyReleased);
        }
        if self.phase != expected_phase {
            return Err(WindowRefusal::WrongPhase {
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
    ) -> Result<(), WindowRefusal> {
        let Some(pending) = self.pending else {
            return Err(WindowRefusal::MissingPendingControl);
        };
        if pending.verb != expected_verb {
            return Err(WindowRefusal::ControlVerbMismatch {
                expected: expected_verb,
                found: pending.verb,
            });
        }
        let expected_epoch = pending.subject.epoch();
        let found_epoch = completion.key.subject.epoch();
        if found_epoch.domain() != expected_epoch.domain() {
            return Err(WindowRefusal::DomainMismatch {
                expected: expected_epoch.domain(),
                found: found_epoch.domain(),
            });
        }
        if found_epoch != expected_epoch {
            return Err(WindowRefusal::EpochMismatch {
                expected: expected_epoch,
                found: found_epoch,
            });
        }
        if completion.key.subject != pending.subject {
            return Err(WindowRefusal::ControlSubjectMismatch);
        }
        if completion.key.verb != pending.verb {
            return Err(WindowRefusal::ControlVerbMismatch {
                expected: pending.verb,
                found: completion.key.verb,
            });
        }
        if completion.key.sequence != pending.sequence {
            return Err(WindowRefusal::ControlSequenceMismatch {
                expected: pending.sequence.get(),
                found: completion.key.sequence.get(),
            });
        }
        Ok(())
    }

    fn mint_control(&mut self, verb: ControlVerb) -> Result<PreparedControl, WindowRefusal> {
        if self.pending.is_some() {
            return Err(WindowRefusal::ControlAlreadyPending);
        }
        let Some(raw) = self.control_high_water.checked_add(1) else {
            return Err(WindowRefusal::ControlSequenceExhausted);
        };
        let Some(sequence) = NonZeroU64::new(raw) else {
            return Err(WindowRefusal::ControlSequenceExhausted);
        };
        let key = ControlKey {
            verb,
            subject: ControlSubject::Window(self.window),
            sequence,
        };
        self.control_high_water = raw;
        self.pending = Some(key);
        Ok(PreparedControl { key })
    }

    fn check_window(&self, window: TransportWindow) -> Result<(), WindowRefusal> {
        if window.epoch().domain() != self.window.epoch().domain() {
            return Err(WindowRefusal::DomainMismatch {
                expected: self.window.epoch().domain(),
                found: window.epoch().domain(),
            });
        }
        if window.epoch() != self.window.epoch() {
            return Err(WindowRefusal::EpochMismatch {
                expected: self.window.epoch(),
                found: window.epoch(),
            });
        }
        if window.resource().id() != self.window.resource().id() {
            return Err(WindowRefusal::ResourceMismatch {
                expected: self.window.resource().id(),
                found: window.resource().id(),
            });
        }
        if window.offset() != self.window.offset() || window.length() != self.window.length() {
            return Err(WindowRefusal::RangeMismatch {
                expected_offset: self.window.offset(),
                expected_length: self.window.length(),
                found_offset: window.offset(),
                found_length: window.length(),
            });
        }
        Ok(())
    }

    fn release(&mut self, authority: WindowReleaseAuthorityKind) {
        self.phase = WindowPhase::Unmapped;
        self.unmap_in_flight = false;
        self.pending = None;
        self.release_authority = Some(authority);
    }

    const fn release_token(&self, authority: WindowReleaseAuthorityKind) -> ReleaseWindow {
        ReleaseWindow {
            window: self.window,
            authority,
        }
    }
}

#[must_use]
#[derive(Debug, Eq, PartialEq)]
pub struct RefusedReleaseWindow<R> {
    reason: WindowRefusal,
    lifecycle: WindowLifecycle<R>,
    authority: ReleaseWindow,
}

impl<R> RefusedReleaseWindow<R> {
    pub const fn reason(&self) -> WindowRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (WindowLifecycle<R>, ReleaseWindow) {
        (self.lifecycle, self.authority)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use self::std::cell::Cell;
    use self::std::rc::Rc;
    use super::*;

    const HOST_ERROR: u32 = VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID;
    const DEFINITE_ERROR: u8 = 0x5a;
    const TEST_DOMAIN: u64 = 1;

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

    fn domain(raw: u64) -> TransportDomainId {
        TransportDomainId::from_raw(raw).unwrap()
    }

    fn root(raw: u64) -> TransportDomainRoot {
        unsafe { TransportDomainRoot::new(raw).unwrap() }
    }

    fn epoch_in_domain(domain_raw: u64, raw: u64) -> TransportEpoch {
        TransportEpoch::from_raw(domain(domain_raw), raw).unwrap()
    }

    fn epoch(raw: u64) -> TransportEpoch {
        epoch_in_domain(TEST_DOMAIN, raw)
    }

    fn resource(epoch: TransportEpoch, id: u32) -> TransportResource {
        TransportResource::from_raw(epoch, id).unwrap()
    }

    fn context(epoch: TransportEpoch, id: u32) -> TransportContext {
        TransportContext::from_raw(epoch, id).unwrap()
    }

    fn attachment(resource: TransportResource, context_id: u32) -> TransportAttachment {
        attachment_with_instance(resource, context_id, 1)
    }

    fn attachment_with_instance(
        resource: TransportResource,
        context_id: u32,
        instance: u64,
    ) -> TransportAttachment {
        TransportAttachment::from_raw(
            resource,
            context(resource.epoch(), context_id),
            NonZeroU64::new(instance).unwrap(),
        )
    }

    fn attachment_reservation(
        resource: TransportResource,
        context_id: u32,
    ) -> AttachmentReservation {
        AttachmentReservation::test_for_attachment(attachment(resource, context_id))
    }

    fn window(resource: TransportResource, offset: u64) -> TransportWindow {
        TransportWindow::new(resource, offset, 0x1000).unwrap()
    }

    fn host_error() -> HostRejection {
        HostRejection::from_response_type(HOST_ERROR).unwrap()
    }

    fn generation(epoch: u64, high_water: u32) -> TransportGeneration {
        TransportGeneration {
            epoch: self::epoch(epoch),
            resource_high_water: high_water,
            context_high_water: 0,
            attachment_high_water: 0,
        }
    }

    fn resource_lifecycle<B>(resource: TransportResource, backing: B) -> ResourceLifecycle<B> {
        ResourceLifecycle::new(ResourceReservation { resource }, backing)
    }

    fn close_attachments<B>(lifecycle: &mut ResourceLifecycle<B>, resource: TransportResource) {
        // SAFETY: each call site owns the full test table and has no live pair.
        let witness = unsafe { AttachmentsClosed::new(resource) };
        lifecycle.install_attachments_closed(witness).unwrap();
    }

    fn window_admission<R>(
        window: TransportWindow,
        reservation: R,
    ) -> (WindowLifecycle<R>, PreparedControl) {
        unsafe { WindowLifecycle::reserve(window, reservation).into_parts() }
    }

    fn reset_authority(retired: TransportEpoch) -> TransportReset {
        TransportReset::test_for_epoch(retired)
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

    fn completed_ok<E>(request: PreparedControl) -> ClassifiedControl<(), E> {
        unsafe { request.completed_ok() }
    }

    fn definite_not_enqueued<E>(request: PreparedControl, error: E) -> ClassifiedControl<(), E> {
        unsafe { request.definite_not_enqueued(error) }
    }

    fn ambiguous<E>(
        request: PreparedControl,
        epoch: TransportEpoch,
        reason: AbandonReason,
    ) -> ClassifiedControl<(), E> {
        request.ambiguous(epoch, reason).unwrap()
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

    fn created(resource: TransportResource) -> ResourceLifecycle<()> {
        let mut lifecycle = resource_lifecycle(resource, ());
        let begin = lifecycle.begin_create(resource).unwrap();
        assert_eq!(begin.effect(), ResourceBeginEffect::CreateStarted);
        let finish = lifecycle
            .finish_create(completed_ok::<u8>(begin.into_request()))
            .unwrap();
        assert_eq!(finish.effect(), &ResourceFinishEffect::CreateCompleted);
        lifecycle
    }

    fn live(resource: TransportResource) -> ResourceLifecycle<()> {
        let mut lifecycle = created(resource);
        let begin = lifecycle
            .begin_attach(attachment_reservation(resource, 1))
            .unwrap();
        assert_eq!(begin.effect(), ResourceBeginEffect::AttachStarted);
        let finish = lifecycle
            .finish_attach(completed_ok::<u8>(begin.into_request()))
            .unwrap();
        assert_eq!(finish.effect(), &ResourceFinishEffect::AttachCompleted);
        lifecycle
    }

    fn mapped(window: TransportWindow) -> WindowLifecycle<()> {
        let (mut lifecycle, request) = window_admission(window, ());
        let finish = lifecycle.finish_map(completed_ok::<u8>(request)).unwrap();
        assert_eq!(finish.effect(), &WindowFinishEffect::MapCompleted);
        lifecycle
    }

    #[test]
    fn identities_are_nonzero_epoch_qualified_and_exhaust_without_wrap() {
        const ROOT_DOMAIN: u64 = 0x100;
        assert_eq!(
            TransportDomainId::from_raw(0),
            Err(TransportDomainRefusal::Zero)
        );
        let domain_root = root(ROOT_DOMAIN);
        assert_eq!(domain_root.id().get(), ROOT_DOMAIN);
        assert_eq!(
            TransportEpoch::from_raw(domain(TEST_DOMAIN), 0),
            Err(EpochRefusal::Zero)
        );
        let active_generation = TransportGeneration::bootstrap(domain_root);
        assert_eq!(active_generation.epoch().get(), 1);
        assert_eq!(active_generation.epoch().domain(), domain(ROOT_DOMAIN));
        let advance = unsafe { active_generation.advance().unwrap() };
        assert_eq!(advance.generation().epoch().get(), 2);
        assert_eq!(
            advance.reset().retired_epoch(),
            epoch_in_domain(ROOT_DOMAIN, 1)
        );
        let (active_generation, _) = advance.into_parts();
        let next = unsafe { active_generation.advance().unwrap() };
        assert_eq!(next.generation().epoch().get(), 3);
        assert_eq!(next.reset().retired_epoch().get(), 2);
        assert_eq!(next.generation().epoch().domain(), domain(ROOT_DOMAIN));

        let refusal = unsafe { TransportGeneration::restore(root(0x101), 0, 7, 8, 9).unwrap_err() };
        assert_eq!(refusal.reason(), EpochRefusal::Zero);
        assert_eq!(refusal.into_root().id().get(), 0x101);
        let restored = unsafe { TransportGeneration::restore(root(0x102), 9, 7, 8, 9).unwrap() };
        assert_eq!(restored.epoch(), epoch_in_domain(0x102, 9));
        assert_eq!(restored.resource_high_water(), 7);
        assert_eq!(restored.context_high_water(), 8);
        assert_eq!(restored.attachment_high_water(), 9);
        let exhausted = generation(u64::MAX, 7);
        let refusal = unsafe { exhausted.advance().unwrap_err() };
        assert_eq!(refusal.reason(), EpochRefusal::Exhausted);
        let exhausted = refusal.into_generation();
        assert_eq!(exhausted.epoch(), epoch(u64::MAX));
        assert_eq!(exhausted.resource_high_water(), 7);
        assert_eq!(
            TransportResource::from_raw(epoch(1), 0),
            Err(ResourceIdentityRefusal::Zero)
        );
        assert_ne!(resource(epoch(1), 7), resource(epoch(2), 7));

        let allocation = generation(9, 0).allocate_resource().unwrap();
        assert_eq!(allocation.resource().id(), 1);
        assert_eq!(allocation.resource().epoch(), epoch(9));
        let (active_generation, _) = allocation.into_parts();
        assert_eq!(active_generation.resource_high_water(), 1);
        let allocation = active_generation.allocate_resource().unwrap();
        assert_eq!(allocation.resource().id(), 2);

        let allocation = generation(9, u32::MAX - 1).allocate_resource().unwrap();
        assert_eq!(allocation.resource().id(), u32::MAX);
        let (exhausted, _) = allocation.into_parts();
        let refusal = exhausted.allocate_resource().unwrap_err();
        assert_eq!(refusal.reason(), ResourceAllocationRefusal::Exhausted);
        assert_eq!(refusal.generation().resource_high_water(), u32::MAX);
        assert_eq!(refusal.into_generation().epoch(), epoch(9));

        assert_eq!(
            TransportContext::from_raw(epoch(9), 0),
            Err(ContextIdentityRefusal::Zero)
        );
        let allocation = generation(9, 7).allocate_context().unwrap();
        assert_eq!(allocation.context().id(), 1);
        let (active_generation, _) = allocation.into_parts();
        assert_eq!(active_generation.resource_high_water(), 7);
        assert_eq!(active_generation.context_high_water(), 1);
        let exhausted = TransportGeneration {
            epoch: epoch(9),
            resource_high_water: 7,
            context_high_water: u32::MAX - 1,
            attachment_high_water: 11,
        };
        let allocation = exhausted.allocate_context().unwrap();
        assert_eq!(allocation.context().id(), u32::MAX);
        let (exhausted, _) = allocation.into_parts();
        let refusal = exhausted.allocate_context().unwrap_err();
        assert_eq!(refusal.reason(), ContextAllocationRefusal::Exhausted);
        assert_eq!(refusal.generation().context_high_water(), u32::MAX);
        assert_eq!(refusal.into_generation().attachment_high_water(), 11);

        let allocation = generation(10, 0).allocate_resource().unwrap();
        let current_resource = allocation.resource();
        let (active_generation, _) = allocation.into_parts();
        let allocation = active_generation.allocate_context().unwrap();
        let current_context = allocation.context();
        let (active_generation, _) = allocation.into_parts();
        // SAFETY: this test owns the empty canonical table and retains the context.
        let allocation =
            unsafe { active_generation.allocate_attachment(current_resource, current_context) }
                .unwrap();
        let first = allocation.attachment();
        let (active_generation, first_reservation) = allocation.into_parts();
        let allocation = active_generation.allocate_resource().unwrap();
        let second_resource = allocation.resource();
        let (active_generation, _) = allocation.into_parts();
        // SAFETY: this distinct pair is absent and the same live context is retained.
        let allocation =
            unsafe { active_generation.allocate_attachment(second_resource, current_context) }
                .unwrap();
        let second = allocation.attachment();
        let (active_generation, second_reservation) = allocation.into_parts();
        assert_ne!(first.resource(), second.resource());
        assert_eq!(first.context(), second.context());
        assert_eq!(first.instance(), 1);
        assert_eq!(second.instance(), 2);

        let foreign_resource = resource(epoch_in_domain(TEST_DOMAIN + 1, 10), 1);
        // SAFETY: domain validation rejects this before canonical admission.
        let refusal =
            unsafe { active_generation.allocate_attachment(foreign_resource, current_context) }
                .unwrap_err();
        assert_eq!(
            refusal.reason(),
            AttachmentAllocationRefusal::ResourceDomainMismatch {
                expected: current_resource.epoch().domain(),
                found: foreign_resource.epoch().domain(),
            }
        );
        let active_generation = refusal.into_generation();
        assert_eq!(active_generation.attachment_high_water(), 2);
        let stale_context = context(epoch(11), 1);
        // SAFETY: epoch validation rejects this before canonical admission.
        let refusal =
            unsafe { active_generation.allocate_attachment(current_resource, stale_context) }
                .unwrap_err();
        assert_eq!(
            refusal.reason(),
            AttachmentAllocationRefusal::ContextEpochMismatch {
                expected: current_resource.epoch(),
                found: stale_context.epoch(),
            }
        );

        let max_resource = resource(current_resource.epoch(), 3);
        let next_resource = resource(current_resource.epoch(), 4);
        let max_context = context(current_resource.epoch(), 2);
        let exhausted = TransportGeneration {
            epoch: current_resource.epoch(),
            resource_high_water: 4,
            context_high_water: 2,
            attachment_high_water: u64::MAX - 1,
        };
        // SAFETY: this distinct pair is absent and its test context stays retained.
        let allocation =
            unsafe { exhausted.allocate_attachment(max_resource, max_context) }.unwrap();
        assert_eq!(allocation.attachment().instance(), u64::MAX);
        let (exhausted, max_reservation) = allocation.into_parts();
        // SAFETY: this other pair is absent; issuance fails before creating its row.
        let refusal =
            unsafe { exhausted.allocate_attachment(next_resource, max_context) }.unwrap_err();
        assert_eq!(refusal.reason(), AttachmentAllocationRefusal::Exhausted);
        assert_eq!(refusal.generation().attachment_high_water(), u64::MAX);
        let _exhausted = refusal.into_generation();
        let retired = TransportGeneration {
            epoch: current_resource.epoch(),
            resource_high_water: 4,
            context_high_water: 2,
            attachment_high_water: u64::MAX,
        };
        // SAFETY: this synthetic old epoch is admission-closed, drained, and DMA-dead.
        let advance = unsafe { retired.advance().unwrap() };
        assert_eq!(advance.generation().resource_high_water(), 0);
        assert_eq!(advance.generation().context_high_water(), 0);
        assert_eq!(advance.generation().attachment_high_water(), 0);
        drop(first_reservation);
        drop(second_reservation);
        drop(max_reservation);
    }

    #[test]
    fn windows_and_host_errors_reject_invalid_sentinels_and_overflow() {
        let resource = resource(epoch(1), 1);
        assert_eq!(
            TransportWindow::new(resource, 0, 0),
            Err(WindowIdentityRefusal::ZeroLength)
        );
        assert_eq!(
            TransportWindow::new(resource, u64::MAX, 1),
            Err(WindowIdentityRefusal::RangeOverflow)
        );
        let window = TransportWindow::new(resource, u64::MAX - 1, 1).unwrap();
        assert_eq!(window.offset(), u64::MAX - 1);
        assert_eq!(window.length(), 1);
        assert_eq!(host_error().response_type(), HOST_ERROR);
    }

    #[test]
    fn malformed_or_ok_responses_cannot_grant_create_unwind() {
        for raw in [
            VIRTIO_GPU_RESP_ERR_UNSPEC,
            VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY,
            VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID,
            VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID,
            VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID,
            VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER,
        ] {
            assert_eq!(
                HostRejection::from_response_type(raw)
                    .unwrap()
                    .response_type(),
                raw
            );
        }
        for raw in [0, 0x0100, 0x1100, 0x1106, 0x1206, u32::MAX] {
            assert_eq!(
                HostRejection::from_response_type(raw),
                Err(HostRejectionRefusal::UndocumentedResponse(raw))
            );
        }
        let epoch = epoch(10);
        let resource = resource(epoch, 1);
        let mut lifecycle = resource_lifecycle(resource, ());
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        let finish = lifecycle
            .finish_create(ambiguous::<u8>(
                request,
                epoch,
                AbandonReason::MalformedResponse,
            ))
            .unwrap();
        assert_eq!(
            finish.effect(),
            &ResourceFinishEffect::CreateAmbiguous(AbandonReason::MalformedResponse)
        );
        assert_eq!(lifecycle.phase(), ResourcePhase::RetirementRequired);
        assert!(finish.destroy_authority().is_none());
    }

    #[test]
    fn create_outcome_matrix_has_exact_safe_unwind_boundary() {
        let epoch = epoch(11);
        let resource = resource(epoch, 1);
        for case in OUTCOMES {
            let mut lifecycle = resource_lifecycle(resource, ());
            let request = lifecycle.begin_create(resource).unwrap().into_request();
            let finish = lifecycle
                .finish_create(outcome(case, request, epoch))
                .unwrap();
            match case {
                OutcomeCase::DefiniteNotEnqueued => {
                    assert_eq!(
                        finish.effect(),
                        &ResourceFinishEffect::CreateDefiniteNotEnqueued(DEFINITE_ERROR)
                    );
                    assert_eq!(lifecycle.phase(), ResourcePhase::Terminal);
                    assert_eq!(
                        finish.destroy_authority().unwrap().authority(),
                        DestroyAuthorityKind::CreateDefiniteNotEnqueued
                    );
                }
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &ResourceFinishEffect::CreateCompleted);
                    assert_eq!(lifecycle.phase(), ResourcePhase::Created);
                    assert!(finish.destroy_authority().is_none());
                }
                OutcomeCase::HostRejected => {
                    assert_eq!(
                        finish.effect(),
                        &ResourceFinishEffect::CreateHostRejected(host_error())
                    );
                    assert_eq!(lifecycle.phase(), ResourcePhase::Terminal);
                    assert_eq!(
                        finish.destroy_authority().unwrap().authority(),
                        DestroyAuthorityKind::CreateHostRejected(host_error())
                    );
                }
                _ => {
                    assert_eq!(
                        finish.effect(),
                        &ResourceFinishEffect::CreateAmbiguous(abandon_reason(case).unwrap())
                    );
                    assert_eq!(lifecycle.phase(), ResourcePhase::RetirementRequired);
                    assert!(lifecycle.is_uncertain());
                    assert!(finish.destroy_authority().is_none());
                }
            }
        }
    }

    #[test]
    fn attach_outcome_matrix_retains_every_created_resource_on_non_success() {
        let epoch = epoch(12);
        let resource = resource(epoch, 2);
        for case in OUTCOMES {
            let mut lifecycle = created(resource);
            let request = lifecycle
                .begin_attach(attachment_reservation(resource, 1))
                .unwrap()
                .into_request();
            let finish = lifecycle
                .finish_attach(outcome(case, request, epoch))
                .unwrap();
            assert!(finish.destroy_authority().is_none());
            match case {
                OutcomeCase::DefiniteNotEnqueued => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::AttachDefiniteNotEnqueued(DEFINITE_ERROR)
                ),
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &ResourceFinishEffect::AttachCompleted);
                    assert_eq!(lifecycle.phase(), ResourcePhase::Live);
                    continue;
                }
                OutcomeCase::HostRejected => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::AttachHostRejected(host_error())
                ),
                _ => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::AttachAmbiguous(abandon_reason(case).unwrap())
                ),
            }
            assert_eq!(lifecycle.phase(), ResourcePhase::RetirementRequired);
            assert_eq!(lifecycle.is_uncertain(), abandon_reason(case).is_some());
        }
    }

    #[test]
    fn creator_attachment_provenance_rejects_foreign_pair_instances_recoverably() {
        let epoch = epoch(121);
        let other_resource = resource(epoch, 3);
        let resource = resource(epoch, 2);
        let mut lifecycle = created(resource);

        let wrong_reservation =
            AttachmentReservation::test_for_attachment(attachment(other_resource, 1));
        let refusal = lifecycle.begin_attach(wrong_reservation).unwrap_err();
        assert_eq!(
            refusal.reason(),
            ResourceRefusal::ResourceMismatch {
                expected: resource.id(),
                found: other_resource.id(),
            }
        );
        assert_eq!(
            refusal.into_reservation().attachment().resource(),
            other_resource
        );

        let creator = attachment_with_instance(resource, 1, 1);
        let foreign_instance = attachment_with_instance(resource, 1, 2);
        let reservation = AttachmentReservation::test_for_attachment(creator);
        let request = lifecycle.begin_attach(reservation).unwrap().into_request();
        let foreign = PreparedControl::from_parts(
            ControlVerb::Attach,
            ControlSubject::Attachment(foreign_instance),
            NonZeroU64::new(request.sequence()).unwrap(),
        );
        let refusal = lifecycle
            .finish_attach(completed_ok::<u8>(foreign))
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            ResourceRefusal::AttachmentInstanceMismatch {
                expected: creator.instance(),
                found: foreign_instance.instance(),
            }
        );
        drop(refusal.into_completion());
        assert_eq!(lifecycle.phase(), ResourcePhase::AttachPending);
        let _ = lifecycle
            .finish_attach(completed_ok::<u8>(request))
            .unwrap();

        assert_eq!(
            lifecycle.begin_detach(foreign_instance),
            Err(ResourceRefusal::AttachmentInstanceMismatch {
                expected: creator.instance(),
                found: foreign_instance.instance(),
            })
        );
        let detach = lifecycle.begin_detach(creator).unwrap().into_request();
        let _ = lifecycle.finish_detach(completed_ok::<u8>(detach)).unwrap();
        assert!(!lifecycle.attachment_may_be_live());
    }

    #[test]
    fn detach_outcome_matrix_never_authorizes_backing_destruction() {
        let epoch = epoch(13);
        let resource = resource(epoch, 3);
        for case in OUTCOMES {
            let mut lifecycle = live(resource);
            let request = lifecycle
                .begin_detach(attachment(resource, 1))
                .unwrap()
                .into_request();
            let finish = lifecycle
                .finish_detach(outcome(case, request, epoch))
                .unwrap();
            assert!(finish.destroy_authority().is_none());
            match case {
                OutcomeCase::DefiniteNotEnqueued => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::DetachDefiniteNotEnqueued(DEFINITE_ERROR)
                ),
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &ResourceFinishEffect::DetachCompleted);
                    assert_eq!(lifecycle.phase(), ResourcePhase::Detached);
                    continue;
                }
                OutcomeCase::HostRejected => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::DetachHostRejected(host_error())
                ),
                _ => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::DetachAmbiguous(abandon_reason(case).unwrap())
                ),
            }
            assert_eq!(lifecycle.phase(), ResourcePhase::RetirementRequired);
            assert_eq!(lifecycle.is_uncertain(), abandon_reason(case).is_some());
        }
    }

    #[test]
    fn unref_outcome_matrix_grants_terminal_authority_only_for_completed_ok() {
        let epoch = epoch(14);
        let resource = resource(epoch, 4);
        for case in OUTCOMES {
            let mut lifecycle = created(resource);
            close_attachments(&mut lifecycle, resource);
            let request = lifecycle.begin_unref(resource).unwrap().into_request();
            let finish = lifecycle
                .finish_unref(outcome(case, request, epoch))
                .unwrap();
            match case {
                OutcomeCase::DefiniteNotEnqueued => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::UnrefDefiniteNotEnqueued(DEFINITE_ERROR)
                ),
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &ResourceFinishEffect::UnrefCompleted);
                    assert_eq!(lifecycle.phase(), ResourcePhase::Terminal);
                    assert_eq!(
                        finish.destroy_authority().unwrap().authority(),
                        DestroyAuthorityKind::UnrefCompleted
                    );
                    continue;
                }
                OutcomeCase::HostRejected => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::UnrefHostRejected(host_error())
                ),
                _ => assert_eq!(
                    finish.effect(),
                    &ResourceFinishEffect::UnrefAmbiguous(abandon_reason(case).unwrap())
                ),
            }
            assert_eq!(lifecycle.phase(), ResourcePhase::RetirementRequired);
            assert!(finish.destroy_authority().is_none());
        }
    }

    #[test]
    fn attachments_closed_is_required_resource_bound_and_closes_admission() {
        let epoch = epoch(140);
        let foreign_resource = resource(epoch, 2);
        let resource = resource(epoch, 1);
        let mut lifecycle = created(resource);

        assert_eq!(
            lifecycle.begin_unref(resource),
            Err(ResourceRefusal::AttachmentsNotClosed)
        );

        let foreign = AttachmentsClosed::test_for_resource(foreign_resource);
        let refusal = lifecycle.install_attachments_closed(foreign).unwrap_err();
        assert_eq!(
            refusal.reason(),
            ResourceRefusal::ResourceMismatch {
                expected: resource.id(),
                found: foreign_resource.id(),
            }
        );
        assert_eq!(refusal.into_witness().resource(), foreign_resource);

        // SAFETY: this test has closed admission and its canonical pair table is empty.
        let witness = unsafe { AttachmentsClosed::new(resource) };
        lifecycle.install_attachments_closed(witness).unwrap();
        assert!(lifecycle.attachments_are_closed());

        let duplicate = AttachmentsClosed::test_for_resource(resource);
        let refusal = lifecycle.install_attachments_closed(duplicate).unwrap_err();
        assert_eq!(refusal.reason(), ResourceRefusal::AttachmentsAlreadyClosed);
        assert_eq!(refusal.into_witness().resource(), resource);

        let reservation = attachment_reservation(resource, 1);
        let attachment = reservation.attachment();
        let refusal = lifecycle.begin_attach(reservation).unwrap_err();
        assert_eq!(refusal.reason(), ResourceRefusal::AttachmentsAlreadyClosed);
        assert_eq!(refusal.into_reservation().attachment(), attachment);

        let request = lifecycle.begin_unref(resource).unwrap().into_request();
        let finish = lifecycle.finish_unref(completed_ok::<u8>(request)).unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("completed UNREF must authorize destruction");
        };
        assert_eq!(lifecycle.consume_terminal(authority), Ok(()));
    }

    #[test]
    fn creator_may_live_refuses_close_and_unref_retry_retains_witness() {
        let epoch = epoch(142);
        let resource = resource(epoch, 1);
        let mut lifecycle = live(resource);

        let forged = AttachmentsClosed::test_for_resource(resource);
        let refusal = lifecycle.install_attachments_closed(forged).unwrap_err();
        assert_eq!(refusal.reason(), ResourceRefusal::AttachmentMayBeLive);
        assert_eq!(refusal.into_witness().resource(), resource);
        assert_eq!(
            lifecycle.begin_unref(resource),
            Err(ResourceRefusal::AttachmentMayBeLive)
        );

        let request = lifecycle
            .begin_detach(attachment(resource, 1))
            .unwrap()
            .into_request();
        let _ = lifecycle
            .finish_detach(completed_ok::<u8>(request))
            .unwrap();
        // SAFETY: exact DETACH removed the only row after test admission closed.
        let witness = unsafe { AttachmentsClosed::new(resource) };
        lifecycle.install_attachments_closed(witness).unwrap();

        let request = lifecycle.begin_unref(resource).unwrap().into_request();
        let finish = lifecycle
            .finish_unref(ambiguous::<u8>(request, epoch, AbandonReason::Timeout))
            .unwrap();
        assert_eq!(
            finish.effect(),
            &ResourceFinishEffect::UnrefAmbiguous(AbandonReason::Timeout)
        );
        assert!(lifecycle.attachments_are_closed());

        let request = lifecycle.begin_unref(resource).unwrap().into_request();
        let finish = lifecycle.finish_unref(completed_ok::<u8>(request)).unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("exact UNREF retry must authorize destruction");
        };
        assert_eq!(lifecycle.consume_terminal(authority), Ok(()));
    }

    #[test]
    fn live_resource_requires_exact_detach_before_unref() {
        let epoch = epoch(141);
        let resource = resource(epoch, 1);
        let mut lifecycle = live(resource);
        assert_eq!(
            lifecycle.begin_unref(resource),
            Err(ResourceRefusal::AttachmentMayBeLive)
        );
        let detach = lifecycle
            .begin_detach(attachment(resource, 1))
            .unwrap()
            .into_request();
        let _ = lifecycle.finish_detach(completed_ok::<u8>(detach)).unwrap();
        close_attachments(&mut lifecycle, resource);
        let begin = lifecycle.begin_unref(resource).unwrap();
        assert_eq!(begin.effect(), ResourceBeginEffect::UnrefStarted);
        let finish = lifecycle
            .finish_unref(completed_ok::<u8>(begin.into_request()))
            .unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("completed UNREF must authorize destruction after DETACH");
        };
        assert_eq!(authority.authority(), DestroyAuthorityKind::UnrefCompleted);
        assert_eq!(lifecycle.consume_terminal(authority), Ok(()));
    }

    #[derive(Clone, Copy)]
    enum PriorAmbiguity {
        Create,
        Attach,
        Detach,
        Unref,
    }

    #[test]
    fn attachment_ambiguity_requires_exact_detach_before_unref() {
        let epoch = epoch(15);
        for (index, prior) in [
            PriorAmbiguity::Create,
            PriorAmbiguity::Attach,
            PriorAmbiguity::Detach,
            PriorAmbiguity::Unref,
        ]
        .into_iter()
        .enumerate()
        {
            let resource = resource(epoch, index as u32 + 1);
            let mut lifecycle = match prior {
                PriorAmbiguity::Create => {
                    let mut state = resource_lifecycle(resource, ());
                    let request = state.begin_create(resource).unwrap().into_request();
                    let _ = state
                        .finish_create(ambiguous::<u8>(request, epoch, AbandonReason::Timeout))
                        .unwrap();
                    state
                }
                PriorAmbiguity::Attach => {
                    let mut state = created(resource);
                    let request = state
                        .begin_attach(attachment_reservation(resource, 1))
                        .unwrap()
                        .into_request();
                    let _ = state
                        .finish_attach(ambiguous::<u8>(request, epoch, AbandonReason::NotOurs))
                        .unwrap();
                    state
                }
                PriorAmbiguity::Detach => {
                    let mut state = live(resource);
                    let request = state
                        .begin_detach(attachment(resource, 1))
                        .unwrap()
                        .into_request();
                    let _ = state
                        .finish_detach(ambiguous::<u8>(
                            request,
                            epoch,
                            AbandonReason::TransportAborted,
                        ))
                        .unwrap();
                    state
                }
                PriorAmbiguity::Unref => {
                    let mut state = created(resource);
                    close_attachments(&mut state, resource);
                    let request = state.begin_unref(resource).unwrap().into_request();
                    let _ = state
                        .finish_unref(ambiguous::<u8>(request, epoch, AbandonReason::Timeout))
                        .unwrap();
                    state
                }
            };
            assert!(lifecycle.is_uncertain());
            if matches!(prior, PriorAmbiguity::Attach | PriorAmbiguity::Detach) {
                assert!(lifecycle.attachment_may_be_live());
                assert_eq!(
                    lifecycle.begin_unref(resource),
                    Err(ResourceRefusal::AttachmentMayBeLive)
                );
                let detach = lifecycle
                    .begin_detach(attachment(resource, 1))
                    .unwrap()
                    .into_request();
                let _ = lifecycle.finish_detach(completed_ok::<u8>(detach)).unwrap();
            }
            if !lifecycle.attachments_are_closed() {
                close_attachments(&mut lifecycle, resource);
            }
            let request = lifecycle.begin_unref(resource).unwrap().into_request();
            let finish = lifecycle.finish_unref(completed_ok::<u8>(request)).unwrap();
            let (_, Some(authority)) = finish.into_parts() else {
                panic!("completed UNREF must produce terminal authority");
            };
            assert_eq!(authority.authority(), DestroyAuthorityKind::UnrefCompleted);
            assert_eq!(lifecycle.consume_terminal(authority), Ok(()));
        }
    }

    fn resource_at_phase(
        resource: TransportResource,
        phase: ResourcePhase,
    ) -> ResourceLifecycle<()> {
        let mut lifecycle = resource_lifecycle(resource, ());
        match phase {
            ResourcePhase::Reserved => {}
            ResourcePhase::CreatePending => {
                let _pending = lifecycle.begin_create(resource).unwrap();
            }
            ResourcePhase::Created => return created(resource),
            ResourcePhase::AttachPending => {
                lifecycle = created(resource);
                let _pending = lifecycle
                    .begin_attach(attachment_reservation(resource, 1))
                    .unwrap();
            }
            ResourcePhase::Live => return live(resource),
            ResourcePhase::DetachPending => {
                lifecycle = live(resource);
                let _pending = lifecycle.begin_detach(attachment(resource, 1)).unwrap();
            }
            ResourcePhase::Detached => {
                lifecycle = live(resource);
                let request = lifecycle
                    .begin_detach(attachment(resource, 1))
                    .unwrap()
                    .into_request();
                let _ = lifecycle
                    .finish_detach(completed_ok::<u8>(request))
                    .unwrap();
            }
            ResourcePhase::RetirementRequired => {
                let request = lifecycle.begin_create(resource).unwrap().into_request();
                let _ = lifecycle
                    .finish_create(ambiguous::<u8>(
                        request,
                        resource.epoch(),
                        AbandonReason::Timeout,
                    ))
                    .unwrap();
            }
            ResourcePhase::UnrefPending => {
                lifecycle = created(resource);
                close_attachments(&mut lifecycle, resource);
                let _pending = lifecycle.begin_unref(resource).unwrap();
            }
            ResourcePhase::Terminal => panic!("terminal is tested as reset replay"),
        }
        assert_eq!(lifecycle.phase(), phase);
        lifecycle
    }

    #[test]
    fn exact_transport_reset_terminalizes_every_nonterminal_resource_phase() {
        let epoch = epoch(16);
        let reset = reset_authority(epoch);
        let phases = [
            ResourcePhase::Reserved,
            ResourcePhase::CreatePending,
            ResourcePhase::Created,
            ResourcePhase::AttachPending,
            ResourcePhase::Live,
            ResourcePhase::DetachPending,
            ResourcePhase::Detached,
            ResourcePhase::RetirementRequired,
            ResourcePhase::UnrefPending,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            let resource = resource(epoch, index as u32 + 1);
            let mut lifecycle = resource_at_phase(resource, phase);
            let authority = lifecycle.transport_reset(resource, &reset).unwrap();
            assert_eq!(lifecycle.phase(), ResourcePhase::Terminal);
            assert_eq!(
                authority.authority(),
                DestroyAuthorityKind::TransportReset(epoch)
            );
            assert_eq!(
                lifecycle.transport_reset(resource, &reset),
                Err(ResourceRefusal::AlreadyTerminal)
            );
            assert_eq!(lifecycle.consume_terminal(authority), Ok(()));
        }
    }

    #[test]
    fn stale_resource_epoch_and_reset_are_inert_and_outcomes_are_recoverable() {
        let current_epoch = epoch(17);
        let stale_epoch = epoch(18);
        let current_resource = resource(current_epoch, 1);
        let stale_resource = resource(stale_epoch, 1);
        let wrong_resource = resource(current_epoch, 2);
        let stale_reset = reset_authority(stale_epoch);
        let current_reset = reset_authority(current_epoch);
        let mut lifecycle = resource_lifecycle(current_resource, ());

        assert_eq!(
            lifecycle.begin_create(stale_resource),
            Err(ResourceRefusal::EpochMismatch {
                expected: current_epoch,
                found: stale_epoch,
            })
        );
        assert_eq!(lifecycle.phase(), ResourcePhase::Reserved);
        assert_eq!(
            lifecycle.begin_create(wrong_resource),
            Err(ResourceRefusal::ResourceMismatch {
                expected: 1,
                found: 2,
            })
        );
        let request = lifecycle
            .begin_create(current_resource)
            .unwrap()
            .into_request();

        let wrong_request = PreparedControl {
            key: ControlKey {
                verb: ControlVerb::Create,
                subject: ControlSubject::Resource(stale_resource),
                sequence: NonZeroU64::MIN,
            },
        };
        let refusal = lifecycle
            .finish_create(definite_not_enqueued(
                wrong_request,
                DropToken::new(&Rc::new(Cell::new(0))),
            ))
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            ResourceRefusal::EpochMismatch {
                expected: current_epoch,
                found: stale_epoch,
            }
        );
        assert!(matches!(
            refusal.into_completion().into_outcome(),
            ControlOutcome::DefiniteNotEnqueued(_)
        ));
        assert_eq!(lifecycle.phase(), ResourcePhase::CreatePending);

        let refusal = request
            .ambiguous::<u8>(stale_epoch, AbandonReason::Timeout)
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            ControlClassificationRefusal::EpochMismatch {
                expected: current_epoch,
                found: stale_epoch,
            }
        );
        let _request = refusal.into_request();
        assert_eq!(lifecycle.phase(), ResourcePhase::CreatePending);

        assert_eq!(
            lifecycle.transport_reset(current_resource, &stale_reset),
            Err(ResourceRefusal::ResetEpochMismatch {
                expected: current_epoch,
                found: stale_epoch,
            })
        );
        assert_eq!(lifecycle.phase(), ResourcePhase::CreatePending);
        assert!(matches!(
            lifecycle.transport_reset(wrong_resource, &current_reset),
            Err(ResourceRefusal::ResourceMismatch { .. })
        ));
        assert_eq!(lifecycle.phase(), ResourcePhase::CreatePending);
    }

    #[test]
    fn resource_replay_and_terminal_authority_are_exact() {
        let epoch = epoch(19);
        let resource_a = resource(epoch, 1);
        let resource_b = resource(epoch, 2);
        let mut a = resource_lifecycle(resource_a, 10u8);
        let mut b = resource_lifecycle(resource_b, 20u8);
        let authority_a = a.cancel_reservation(resource_a).unwrap();
        let authority_b = b.cancel_reservation(resource_b).unwrap();
        assert_eq!(
            a.cancel_reservation(resource_a),
            Err(ResourceRefusal::AlreadyTerminal)
        );

        let refusal = a.consume_terminal(authority_b).unwrap_err();
        assert!(matches!(
            refusal.reason(),
            ResourceRefusal::ResourceMismatch { .. }
        ));
        let (a, authority_b) = refusal.into_parts();
        assert_eq!(a.consume_terminal(authority_a), Ok(10));
        assert_eq!(b.consume_terminal(authority_b), Ok(20));
    }

    #[test]
    fn backing_token_survives_ambiguity_until_unique_unref_authority_is_consumed() {
        let drops = Rc::new(Cell::new(0));
        let epoch = epoch(20);
        let resource = resource(epoch, 1);
        let mut lifecycle = resource_lifecycle(resource, DropToken::new(&drops));
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        let _ = lifecycle
            .finish_create(completed_ok::<u8>(request))
            .unwrap();
        let request = lifecycle
            .begin_attach(attachment_reservation(resource, 1))
            .unwrap()
            .into_request();
        let _ = lifecycle
            .finish_attach(ambiguous::<u8>(
                request,
                epoch,
                AbandonReason::TransportAborted,
            ))
            .unwrap();
        assert_eq!(drops.get(), 0);

        assert_eq!(
            lifecycle.begin_unref(resource),
            Err(ResourceRefusal::AttachmentMayBeLive)
        );
        let request = lifecycle
            .begin_detach(attachment(resource, 1))
            .unwrap()
            .into_request();
        let _ = lifecycle
            .finish_detach(completed_ok::<u8>(request))
            .unwrap();

        close_attachments(&mut lifecycle, resource);

        let request = lifecycle.begin_unref(resource).unwrap().into_request();
        let _ = lifecycle
            .finish_unref(ambiguous::<u8>(request, epoch, AbandonReason::Timeout))
            .unwrap();
        assert_eq!(drops.get(), 0);
        let request = lifecycle.begin_unref(resource).unwrap().into_request();
        let finish = lifecycle.finish_unref(completed_ok::<u8>(request)).unwrap();
        let (_, Some(authority)) = finish.into_parts() else {
            panic!("completed UNREF must authorize destruction");
        };
        assert_eq!(drops.get(), 0);
        let backing = lifecycle.consume_terminal(authority).unwrap();
        assert_eq!(drops.get(), 0);
        drop(backing);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn dropping_uncertain_state_quarantines_embedded_ownership_tokens() {
        let backing_drops = Rc::new(Cell::new(0));
        let window_drops = Rc::new(Cell::new(0));
        let epoch = epoch(201);
        let resource = resource(epoch, 1);
        let mut backing = resource_lifecycle(resource, DropToken::new(&backing_drops));
        let request = backing.begin_create(resource).unwrap().into_request();
        let _ = backing
            .finish_create(ambiguous::<u8>(
                request,
                epoch,
                AbandonReason::TransportAborted,
            ))
            .unwrap();
        drop(backing);
        assert_eq!(backing_drops.get(), 0);

        let window = window(resource, 0x9000);
        let (mut reservation, request) = window_admission(window, DropToken::new(&window_drops));
        let _ = reservation
            .finish_map(ambiguous::<u8>(request, epoch, AbandonReason::Timeout))
            .unwrap();
        drop(reservation);
        assert_eq!(window_drops.get(), 0);
    }

    #[test]
    fn map_outcome_matrix_only_releases_a_new_reservation_when_safe() {
        let epoch = epoch(21);
        let window = window(resource(epoch, 1), 0x1000);
        for case in OUTCOMES {
            let (mut lifecycle, request) = window_admission(window, ());
            let finish = lifecycle.finish_map(outcome(case, request, epoch)).unwrap();
            match case {
                OutcomeCase::DefiniteNotEnqueued => {
                    assert_eq!(
                        finish.effect(),
                        &WindowFinishEffect::MapDefiniteNotEnqueued(DEFINITE_ERROR)
                    );
                    assert_eq!(lifecycle.phase(), WindowPhase::Unmapped);
                    assert_eq!(
                        finish.release_authority().unwrap().authority(),
                        WindowReleaseAuthorityKind::MapDefiniteNotEnqueued
                    );
                }
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &WindowFinishEffect::MapCompleted);
                    assert_eq!(lifecycle.phase(), WindowPhase::Mapped);
                    assert!(finish.release_authority().is_none());
                }
                OutcomeCase::HostRejected => {
                    assert_eq!(
                        finish.effect(),
                        &WindowFinishEffect::MapHostRejected(host_error())
                    );
                    assert_eq!(lifecycle.phase(), WindowPhase::Unmapped);
                    assert_eq!(
                        finish.release_authority().unwrap().authority(),
                        WindowReleaseAuthorityKind::MapHostRejected(host_error())
                    );
                }
                _ => {
                    assert_eq!(
                        finish.effect(),
                        &WindowFinishEffect::MapAmbiguous(abandon_reason(case).unwrap())
                    );
                    assert_eq!(lifecycle.phase(), WindowPhase::MapUncertain);
                    assert!(finish.release_authority().is_none());
                }
            }
        }
    }

    #[test]
    fn unmap_outcome_matrix_releases_only_on_completed_ok() {
        let epoch = epoch(22);
        let window = window(resource(epoch, 1), 0x2000);
        for case in OUTCOMES {
            let mut lifecycle = mapped(window);
            let begin = lifecycle.begin_unmap(window).unwrap();
            assert_eq!(begin.effect(), WindowBeginEffect::UnmapStarted);
            let finish = lifecycle
                .finish_unmap(outcome(case, begin.into_request(), epoch))
                .unwrap();
            match case {
                OutcomeCase::DefiniteNotEnqueued => assert_eq!(
                    finish.effect(),
                    &WindowFinishEffect::UnmapDefiniteNotEnqueued(DEFINITE_ERROR)
                ),
                OutcomeCase::Completed => {
                    assert_eq!(finish.effect(), &WindowFinishEffect::UnmapCompleted);
                    assert_eq!(lifecycle.phase(), WindowPhase::Unmapped);
                    assert_eq!(
                        finish.release_authority().unwrap().authority(),
                        WindowReleaseAuthorityKind::UnmapCompleted
                    );
                    continue;
                }
                OutcomeCase::HostRejected => assert_eq!(
                    finish.effect(),
                    &WindowFinishEffect::UnmapHostRejected(host_error())
                ),
                _ => assert_eq!(
                    finish.effect(),
                    &WindowFinishEffect::UnmapAmbiguous(abandon_reason(case).unwrap())
                ),
            }
            assert_eq!(lifecycle.phase(), WindowPhase::UnmapUncertain);
            assert!(!lifecycle.is_unmap_in_flight());
            assert!(finish.release_authority().is_none());
            let retry = lifecycle.begin_unmap(window).unwrap();
            assert_eq!(retry.effect(), WindowBeginEffect::UnmapStarted);
        }
    }

    #[test]
    fn map_ambiguity_quarantines_until_completed_unmap() {
        let epoch = epoch(23);
        let window = window(resource(epoch, 1), 0x3000);
        let (mut lifecycle, request) = window_admission(window, ());
        let map = lifecycle
            .finish_map(ambiguous::<u8>(
                request,
                epoch,
                AbandonReason::TransportAborted,
            ))
            .unwrap();
        assert_eq!(
            map.effect(),
            &WindowFinishEffect::MapAmbiguous(AbandonReason::TransportAborted)
        );
        assert!(map.release_authority().is_none());
        assert_eq!(lifecycle.phase(), WindowPhase::MapUncertain);

        let request = lifecycle.begin_unmap(window).unwrap().into_request();
        let unmap = lifecycle.finish_unmap(completed_ok::<u8>(request)).unwrap();
        let (_, Some(authority)) = unmap.into_parts() else {
            panic!("completed UNMAP must authorize range reuse");
        };
        assert_eq!(lifecycle.consume_unmapped(authority), Ok(()));
    }

    fn window_at_phase(window: TransportWindow, phase: WindowPhase) -> WindowLifecycle<()> {
        let (mut lifecycle, request) = window_admission(window, ());
        match phase {
            WindowPhase::MapPending => drop(request),
            WindowPhase::Mapped => return mapped(window),
            WindowPhase::MapUncertain => {
                let _ = lifecycle
                    .finish_map(ambiguous::<u8>(
                        request,
                        window.epoch(),
                        AbandonReason::Timeout,
                    ))
                    .unwrap();
            }
            WindowPhase::UnmapUncertain => {
                lifecycle = mapped(window);
                let _request = lifecycle.begin_unmap(window).unwrap();
            }
            WindowPhase::Unmapped => panic!("released phase is tested as reset replay"),
        }
        assert_eq!(lifecycle.phase(), phase);
        lifecycle
    }

    #[test]
    fn exact_transport_reset_releases_every_owned_window_phase() {
        let epoch = epoch(24);
        let reset = reset_authority(epoch);
        let phases = [
            WindowPhase::MapPending,
            WindowPhase::Mapped,
            WindowPhase::MapUncertain,
            WindowPhase::UnmapUncertain,
        ];
        for (index, phase) in phases.into_iter().enumerate() {
            let window = window(resource(epoch, index as u32 + 1), index as u64 * 0x1000);
            let mut lifecycle = window_at_phase(window, phase);
            let authority = lifecycle.transport_reset(window, &reset).unwrap();
            assert_eq!(lifecycle.phase(), WindowPhase::Unmapped);
            assert_eq!(
                authority.authority(),
                WindowReleaseAuthorityKind::TransportReset(epoch)
            );
            assert_eq!(
                lifecycle.transport_reset(window, &reset),
                Err(WindowRefusal::AlreadyReleased)
            );
            assert_eq!(lifecycle.consume_unmapped(authority), Ok(()));
        }
    }

    #[test]
    fn stale_window_identity_epoch_and_outcome_are_inert_and_recoverable() {
        let current_epoch = epoch(25);
        let stale_epoch = epoch(26);
        let current_resource = resource(current_epoch, 1);
        let current_window = window(current_resource, 0x4000);
        let stale_window = window(resource(stale_epoch, 1), 0x4000);
        let wrong_resource_window = window(resource(current_epoch, 2), 0x4000);
        let wrong_range = window(current_resource, 0x5000);
        let stale_reset = reset_authority(stale_epoch);
        let (mut lifecycle, request) = window_admission(current_window, ());

        for (found, expected) in [
            (
                stale_window,
                WindowRefusal::EpochMismatch {
                    expected: current_epoch,
                    found: stale_epoch,
                },
            ),
            (wrong_resource_window, WindowRefusal::ControlSubjectMismatch),
            (wrong_range, WindowRefusal::ControlSubjectMismatch),
        ] {
            let wrong_request = PreparedControl {
                key: ControlKey {
                    verb: ControlVerb::Map,
                    subject: ControlSubject::Window(found),
                    sequence: NonZeroU64::MIN,
                },
            };
            let refusal = lifecycle
                .finish_map(definite_not_enqueued(wrong_request, DEFINITE_ERROR))
                .unwrap_err();
            assert_eq!(refusal.reason(), expected);
            assert_eq!(
                refusal.into_completion().into_outcome(),
                ControlOutcome::DefiniteNotEnqueued(DEFINITE_ERROR)
            );
            assert_eq!(lifecycle.phase(), WindowPhase::MapPending);
        }

        let refusal = request
            .ambiguous::<u8>(stale_epoch, AbandonReason::NotOurs)
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            ControlClassificationRefusal::EpochMismatch {
                expected: current_epoch,
                found: stale_epoch,
            }
        );
        let _request = refusal.into_request();
        assert_eq!(lifecycle.phase(), WindowPhase::MapPending);
        assert_eq!(
            lifecycle.transport_reset(current_window, &stale_reset),
            Err(WindowRefusal::ResetEpochMismatch {
                expected: current_epoch,
                found: stale_epoch,
            })
        );
        assert_eq!(lifecycle.phase(), WindowPhase::MapPending);
    }

    #[test]
    fn unmap_replay_requires_a_fresh_begin_and_exact_terminal_token() {
        let epoch = epoch(27);
        let window_a = window(resource(epoch, 1), 0x6000);
        let window_b = window(resource(epoch, 2), 0x6000);
        let mut a = mapped(window_a);
        let begin = a.begin_unmap(window_a).unwrap();
        let sequence = begin.request().sequence();
        assert_eq!(
            a.begin_unmap(window_a),
            Err(WindowRefusal::UnmapAlreadyInFlight)
        );
        let _ = a
            .finish_unmap(definite_not_enqueued(begin.into_request(), DEFINITE_ERROR))
            .unwrap();
        let replay = PreparedControl {
            key: ControlKey {
                verb: ControlVerb::Unmap,
                subject: ControlSubject::Window(window_a),
                sequence: NonZeroU64::new(sequence).unwrap(),
            },
        };
        assert!(matches!(
            a.finish_unmap(completed_ok::<u8>(replay)),
            Err(RefusedWindowOutcome {
                reason: WindowRefusal::UnmapNotInFlight,
                ..
            })
        ));
        let request = a.begin_unmap(window_a).unwrap().into_request();
        let finish_a = a.finish_unmap(completed_ok::<u8>(request)).unwrap();
        let (_, Some(authority_a)) = finish_a.into_parts() else {
            panic!("completed UNMAP must authorize release");
        };

        let (mut b, request) = window_admission(window_b, 22u8);
        let finish_b = b
            .finish_map(definite_not_enqueued(request, DEFINITE_ERROR))
            .unwrap();
        let (_, Some(authority_b)) = finish_b.into_parts() else {
            panic!("definite MAP refusal must authorize release");
        };
        let refusal = a.consume_unmapped(authority_b).unwrap_err();
        assert!(matches!(
            refusal.reason(),
            WindowRefusal::ResourceMismatch { .. }
        ));
        let (a, authority_b) = refusal.into_parts();
        assert_eq!(a.consume_unmapped(authority_a), Ok(()));
        assert_eq!(b.consume_unmapped(authority_b), Ok(22));
    }

    #[test]
    fn reservation_token_is_quarantined_across_map_and_unmap_ambiguity() {
        let drops = Rc::new(Cell::new(0));
        let epoch = epoch(28);
        let window = window(resource(epoch, 1), 0x7000);
        let (mut lifecycle, request) = window_admission(window, DropToken::new(&drops));
        let _ = lifecycle
            .finish_map(ambiguous::<u8>(
                request,
                epoch,
                AbandonReason::TransportAborted,
            ))
            .unwrap();
        assert_eq!(drops.get(), 0);
        let request = lifecycle.begin_unmap(window).unwrap().into_request();
        let _ = lifecycle
            .finish_unmap(ambiguous::<u8>(request, epoch, AbandonReason::Timeout))
            .unwrap();
        assert_eq!(drops.get(), 0);
        let reset = reset_authority(epoch);
        let authority = lifecycle.transport_reset(window, &reset).unwrap();
        assert_eq!(drops.get(), 0);
        let reservation = lifecycle.consume_unmapped(authority).unwrap();
        assert_eq!(drops.get(), 0);
        drop(reservation);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn one_generation_mints_unique_reservations_and_one_borrowable_reset() {
        let generation = TransportGeneration::bootstrap(root(0x103));
        let allocation = generation.allocate_resource().unwrap();
        let resource_a = allocation.resource();
        let (generation, reservation_a) = allocation.into_parts();
        let allocation = generation.allocate_resource().unwrap();
        let resource_b = allocation.resource();
        let (generation, reservation_b) = allocation.into_parts();
        assert_ne!(resource_a, resource_b);

        let mut a = ResourceLifecycle::new(reservation_a, ());
        let mut b = ResourceLifecycle::new(reservation_b, ());
        let request_a = a.begin_create(resource_a).unwrap().into_request();
        let request_b = b.begin_create(resource_b).unwrap().into_request();
        let refusal = b.finish_create(completed_ok::<u8>(request_a)).unwrap_err();
        assert_eq!(refusal.reason(), ResourceRefusal::ControlSubjectMismatch);
        let completion_a = refusal.into_completion();
        let _ = a.finish_create(completion_a).unwrap();
        let _ = b.finish_create(completed_ok::<u8>(request_b)).unwrap();

        let advance = unsafe { generation.advance().unwrap() };
        let (_, reset) = advance.into_parts();
        let authority_a = a.transport_reset(resource_a, &reset).unwrap();
        let authority_b = b.transport_reset(resource_b, &reset).unwrap();
        let refusal = a.consume_terminal(authority_b).unwrap_err();
        assert!(matches!(
            refusal.reason(),
            ResourceRefusal::ResourceMismatch { .. }
        ));
        let (a, authority_b) = refusal.into_parts();
        assert_eq!(a.consume_terminal(authority_a), Ok(()));
        assert_eq!(b.consume_terminal(authority_b), Ok(()));
    }

    #[test]
    fn domain_brand_blocks_same_value_cross_device_completions_and_resets() {
        let allocation_a = TransportGeneration::bootstrap(root(0x104))
            .allocate_resource()
            .unwrap();
        let resource_a = allocation_a.resource();
        let (generation_a, reservation_a) = allocation_a.into_parts();
        let allocation_b = TransportGeneration::bootstrap(root(0x105))
            .allocate_resource()
            .unwrap();
        let resource_b = allocation_b.resource();
        let (generation_b, reservation_b) = allocation_b.into_parts();
        assert_eq!(resource_a.epoch().get(), resource_b.epoch().get());
        assert_eq!(resource_a.id(), resource_b.id());
        assert_ne!(resource_a.epoch().domain(), resource_b.epoch().domain());

        let mut a = ResourceLifecycle::new(reservation_a, ());
        let mut b = ResourceLifecycle::new(reservation_b, ());
        let request_a = a.begin_create(resource_a).unwrap().into_request();
        let request_b = b.begin_create(resource_b).unwrap().into_request();
        assert_eq!(request_a.sequence(), request_b.sequence());
        let refusal = request_a
            .ambiguous::<u8>(resource_b.epoch(), AbandonReason::Timeout)
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            ControlClassificationRefusal::DomainMismatch {
                expected: resource_a.epoch().domain(),
                found: resource_b.epoch().domain(),
            }
        );
        let request_a = refusal.into_request();
        let refusal = b.finish_create(completed_ok::<u8>(request_a)).unwrap_err();
        assert_eq!(
            refusal.reason(),
            ResourceRefusal::DomainMismatch {
                expected: resource_b.epoch().domain(),
                found: resource_a.epoch().domain(),
            }
        );
        let completion_a = refusal.into_completion();
        let _ = a.finish_create(completion_a).unwrap();
        let _ = b.finish_create(completed_ok::<u8>(request_b)).unwrap();

        drop(generation_a);
        drop(generation_b);
        let reset_a = TransportReset::test_for_epoch(resource_a.epoch());
        let reset_b = TransportReset::test_for_epoch(resource_b.epoch());
        assert_eq!(
            b.transport_reset(resource_b, &reset_a),
            Err(ResourceRefusal::ResetDomainMismatch {
                expected: resource_b.epoch().domain(),
                found: resource_a.epoch().domain(),
            })
        );
        assert_eq!(b.phase(), ResourcePhase::Created);
        let authority_b = b.transport_reset(resource_b, &reset_b).unwrap();
        let authority_a = a.transport_reset(resource_a, &reset_a).unwrap();
        assert_eq!(b.consume_terminal(authority_b), Ok(()));
        assert_eq!(a.consume_terminal(authority_a), Ok(()));

        let window_a = window(resource_a, 0xc000);
        let window_b = window(resource_b, 0xc000);
        let (mut a, request_a) = window_admission(window_a, ());
        let (mut b, request_b) = window_admission(window_b, ());
        assert_eq!(request_a.sequence(), request_b.sequence());
        let refusal = b.finish_map(completed_ok::<u8>(request_a)).unwrap_err();
        assert_eq!(
            refusal.reason(),
            WindowRefusal::DomainMismatch {
                expected: window_b.epoch().domain(),
                found: window_a.epoch().domain(),
            }
        );
        let completion_a = refusal.into_completion();
        let _ = a.finish_map(completion_a).unwrap();
        let _ = b.finish_map(completed_ok::<u8>(request_b)).unwrap();
        assert_eq!(
            b.transport_reset(window_b, &reset_a),
            Err(WindowRefusal::ResetDomainMismatch {
                expected: window_b.epoch().domain(),
                found: window_a.epoch().domain(),
            })
        );
        assert_eq!(b.phase(), WindowPhase::Mapped);
        let authority_b = b.transport_reset(window_b, &reset_b).unwrap();
        let authority_a = a.transport_reset(window_a, &reset_a).unwrap();
        assert_eq!(b.consume_unmapped(authority_b), Ok(()));
        assert_eq!(a.consume_unmapped(authority_a), Ok(()));
    }

    #[test]
    fn stale_sequence_and_late_map_completion_cannot_cross_current_operation() {
        let epoch = epoch(281);
        let resource = resource(epoch, 1);
        let mut lifecycle = resource_lifecycle(resource, ());
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        let stale = PreparedControl {
            key: ControlKey {
                verb: ControlVerb::Create,
                subject: ControlSubject::Resource(resource),
                sequence: NonZeroU64::new(request.sequence() + 1).unwrap(),
            },
        };
        let refusal = lifecycle
            .finish_create(completed_ok::<u8>(stale))
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            ResourceRefusal::ControlSequenceMismatch {
                expected: request.sequence(),
                found: request.sequence() + 1,
            }
        );
        let _ = lifecycle
            .finish_create(completed_ok::<u8>(request))
            .unwrap();

        let window = window(resource, 0x8000);
        let (mut lifecycle, map_request) = window_admission(window, ());
        let late_map_sequence = map_request.sequence();
        let _ = lifecycle
            .finish_map(ambiguous::<u8>(map_request, epoch, AbandonReason::Timeout))
            .unwrap();
        let unmap_request = lifecycle.begin_unmap(window).unwrap().into_request();
        let late_map = PreparedControl {
            key: ControlKey {
                verb: ControlVerb::Map,
                subject: ControlSubject::Window(window),
                sequence: NonZeroU64::new(late_map_sequence).unwrap(),
            },
        };
        let refusal = lifecycle
            .finish_unmap(completed_ok::<u8>(late_map))
            .unwrap_err();
        assert_eq!(
            refusal.reason(),
            WindowRefusal::ControlVerbMismatch {
                expected: ControlVerb::Unmap,
                found: ControlVerb::Map,
            }
        );
        let _ = lifecycle
            .finish_unmap(completed_ok::<u8>(unmap_request))
            .unwrap();
    }

    #[test]
    fn sequence_exhaustion_is_inert_and_dropping_requests_quarantines_owners() {
        let epoch = epoch(282);
        let resource = resource(epoch, 1);
        let mut exhausted = resource_lifecycle(resource, ());
        exhausted.control_high_water = u64::MAX;
        assert_eq!(
            exhausted.begin_create(resource),
            Err(ResourceRefusal::ControlSequenceExhausted)
        );
        assert_eq!(exhausted.phase(), ResourcePhase::Reserved);
        assert!(exhausted.pending.is_none());

        let window = window(resource, 0xa000);
        let mut mapped = mapped(window);
        mapped.control_high_water = u64::MAX;
        assert_eq!(
            mapped.begin_unmap(window),
            Err(WindowRefusal::ControlSequenceExhausted)
        );
        assert_eq!(mapped.phase(), WindowPhase::Mapped);
        assert!(mapped.pending.is_none());

        let request_drops = Rc::new(Cell::new(0));
        let mut lifecycle = resource_lifecycle(resource, DropToken::new(&request_drops));
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        drop(request);
        drop(lifecycle);
        assert_eq!(request_drops.get(), 0);

        let completion_drops = Rc::new(Cell::new(0));
        let mut lifecycle = resource_lifecycle(resource, DropToken::new(&completion_drops));
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        let completion = ambiguous::<u8>(request, epoch, AbandonReason::TransportAborted);
        drop(completion);
        drop(lifecycle);
        assert_eq!(completion_drops.get(), 0);
    }

    #[test]
    fn refused_move_only_error_and_abandoned_outcomes_are_not_lost() {
        let drops = Rc::new(Cell::new(0));
        let current_epoch = epoch(29);
        let current_resource = resource(current_epoch, 1);
        let mut lifecycle = resource_lifecycle(current_resource, ());
        let request = lifecycle
            .begin_create(current_resource)
            .unwrap()
            .into_request();
        let wrong_operation = PreparedControl {
            key: ControlKey {
                verb: ControlVerb::Attach,
                subject: ControlSubject::Resource(current_resource),
                sequence: NonZeroU64::new(request.sequence()).unwrap(),
            },
        };
        let refusal = lifecycle
            .finish_create(definite_not_enqueued(
                wrong_operation,
                DropToken::new(&drops),
            ))
            .unwrap_err();
        assert!(matches!(
            refusal.reason(),
            ResourceRefusal::ControlVerbMismatch { .. }
        ));
        assert_eq!(drops.get(), 0);
        let ControlOutcome::DefiniteNotEnqueued(error) = refusal.into_completion().into_outcome()
        else {
            panic!("refusal must return the exact outcome");
        };
        assert_eq!(drops.get(), 0);
        drop(error);
        assert_eq!(drops.get(), 1);

        let other_resource = resource(current_epoch, 2);
        let mut other = resource_lifecycle(other_resource, ());
        let other_request = other.begin_create(other_resource).unwrap().into_request();
        let refusal = lifecycle
            .finish_create(ambiguous::<u8>(
                other_request,
                current_epoch,
                AbandonReason::TransportAborted,
            ))
            .unwrap_err();
        assert_eq!(refusal.reason(), ResourceRefusal::ControlSubjectMismatch);
        let ControlOutcome::Ambiguous(abandoned) = refusal.into_completion().into_outcome() else {
            panic!("refusal must return the abandoned control token");
        };
        assert_eq!(abandoned.epoch(), current_epoch);
        assert_eq!(abandoned.reason(), AbandonReason::TransportAborted);
        assert_eq!(lifecycle.phase(), ResourcePhase::CreatePending);
        drop(request);
    }
}
