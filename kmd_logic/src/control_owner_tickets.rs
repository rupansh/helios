//! Table-owned tickets keep control requests and rundown custody out of wire handles.

use crate::control_owner_slots::{
    ContextSlotKind, PairSlotKind, ResourceSlotKind, SlotHandle, SlotTableId, WindowSlotKind,
};
use crate::control_ownership::{
    AbandonReason, ClassifiedControl, ControlOutcome, ControlSubject, ControlVerb, ExactNoData,
    HostRejection, PreparedControl, TransportDomainId, TransportEpoch, TransportReset,
};
use core::fmt;
use core::marker::PhantomData;
use core::mem::{replace, size_of, ManuallyDrop};
use helios_protocol::virtio_gpu::{
    VirtioGpuCtrlHdr, VirtioGpuRespMapInfo, VIRTIO_GPU_RESP_OK_MAP_INFO, VIRTIO_GPU_RESP_OK_NODATA,
};

mod sealed {
    pub trait Sealed {}

    impl Sealed for super::ResourceSlotKind {}
    impl Sealed for super::ContextSlotKind {}
    impl Sealed for super::PairSlotKind {}
    impl Sealed for super::WindowSlotKind {}
}

pub trait TicketRowKind: sealed::Sealed {
    fn accepts(verb: ControlVerb, subject: ControlSubject) -> bool;
}

impl TicketRowKind for ResourceSlotKind {
    fn accepts(verb: ControlVerb, subject: ControlSubject) -> bool {
        matches!(
            (verb, subject),
            (
                ControlVerb::Create | ControlVerb::Unref,
                ControlSubject::Resource(_)
            ) | (
                ControlVerb::Attach | ControlVerb::Detach,
                ControlSubject::Attachment(_)
            )
        )
    }
}

impl TicketRowKind for ContextSlotKind {
    fn accepts(verb: ControlVerb, subject: ControlSubject) -> bool {
        matches!(
            (verb, subject),
            (
                ControlVerb::ContextCreate | ControlVerb::ContextDestroy,
                ControlSubject::Context(_)
            )
        )
    }
}

impl TicketRowKind for PairSlotKind {
    fn accepts(verb: ControlVerb, subject: ControlSubject) -> bool {
        matches!(
            (verb, subject),
            (
                ControlVerb::Attach | ControlVerb::Detach,
                ControlSubject::Attachment(_)
            )
        )
    }
}

impl TicketRowKind for WindowSlotKind {
    fn accepts(verb: ControlVerb, subject: ControlSubject) -> bool {
        matches!(
            (verb, subject),
            (
                ControlVerb::Map | ControlVerb::Unmap,
                ControlSubject::Window(_)
            )
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TicketState {
    Empty,
    Reserved,
    Prepared,
    MayHaveSubmitted,
    LifecyclePending,
    LifecycleApplying,
    LifecyclePoisoned,
    ReleasePending,
    ResetPending,
    Retired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpectedReply {
    UnitNoData,
    ContextNoData,
    MapInfo,
}

impl ExpectedReply {
    const fn for_verb(verb: ControlVerb) -> Self {
        match verb {
            ControlVerb::Map => Self::MapInfo,
            ControlVerb::ContextCreate | ControlVerb::ContextDestroy => Self::ContextNoData,
            ControlVerb::Create
            | ControlVerb::Attach
            | ControlVerb::Detach
            | ControlVerb::Unref
            | ControlVerb::Unmap => Self::UnitNoData,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RowIdentity {
    table: SlotTableId,
    epoch: TransportEpoch,
    index: u32,
    incarnation: u64,
}

impl RowIdentity {
    fn from_handle<K>(handle: SlotHandle<K>) -> Self {
        Self {
            table: handle.table(),
            epoch: handle.epoch(),
            index: handle.index(),
            incarnation: handle.incarnation(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TicketIdentity {
    row: RowIdentity,
    sequence: u64,
    verb: ControlVerb,
    subject: ControlSubject,
    expected: ExpectedReply,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TicketKey {
    row: RowIdentity,
    sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompletionOrigin {
    table: SlotTableId,
    index: u32,
    incarnation: u64,
}

impl TicketIdentity {
    const fn key(self) -> TicketKey {
        TicketKey {
            row: self.row,
            sequence: self.sequence,
        }
    }

    const fn completion_origin(self) -> CompletionOrigin {
        CompletionOrigin {
            table: self.row.table,
            index: self.row.index,
            incarnation: self.row.incarnation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TicketRefusal {
    TableMismatch {
        expected: SlotTableId,
        found: SlotTableId,
    },
    DomainMismatch {
        expected: TransportDomainId,
        found: TransportDomainId,
    },
    EpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    IndexOutOfRange {
        index: u32,
        capacity: u32,
    },
    RowIncarnationWentBackward {
        high_water: u64,
        found: u64,
    },
    RowIncarnationMismatch {
        expected: u64,
        found: u64,
    },
    SequenceReusedOrWentBackward {
        high_water: u64,
        found: u64,
    },
    SequenceExhausted,
    ControlSequenceMismatch {
        expected: u64,
        found: u64,
    },
    ControlSubjectMismatch,
    SubjectEpochMismatch {
        expected: TransportEpoch,
        found: TransportEpoch,
    },
    WrongState {
        expected: TicketState,
        found: TicketState,
    },
    NotResettable {
        found: TicketState,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TicketTableRefusal {
    CapacityTooLarge { found: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TicketEpochRebindRefusal {
    pub index: u32,
    pub state: TicketState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlWireKey {
    verb: ControlVerb,
    subject: ControlSubject,
    sequence: u64,
}

impl ControlWireKey {
    pub const fn verb(self) -> ControlVerb {
        self.verb
    }

    pub const fn subject(self) -> ControlSubject {
        self.subject
    }

    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

#[must_use]
pub enum RunnerOutcome<E> {
    DefiniteNotEnqueued(E),
    HostResponse {
        response_type: u32,
        written_length: usize,
        map_info: u32,
    },
    Ambiguous(AbandonReason),
}

#[must_use]
enum TicketControl<E> {
    Unit(ClassifiedControl<(), E>),
    Context(ClassifiedControl<ExactNoData, E>),
    Map {
        completion: ClassifiedControl<(), E>,
        map_info: Option<u32>,
    },
}

impl<E> TicketControl<E> {
    const fn expected(&self) -> ExpectedReply {
        match self {
            Self::Unit(_) => ExpectedReply::UnitNoData,
            Self::Context(_) => ExpectedReply::ContextNoData,
            Self::Map { .. } => ExpectedReply::MapInfo,
        }
    }

    const fn verb(&self) -> ControlVerb {
        match self {
            Self::Unit(completion) | Self::Map { completion, .. } => completion.verb(),
            Self::Context(completion) => completion.verb(),
        }
    }

    const fn subject(&self) -> ControlSubject {
        match self {
            Self::Unit(completion) | Self::Map { completion, .. } => completion.subject(),
            Self::Context(completion) => completion.subject(),
        }
    }

    const fn sequence(&self) -> u64 {
        match self {
            Self::Unit(completion) | Self::Map { completion, .. } => completion.sequence(),
            Self::Context(completion) => completion.sequence(),
        }
    }

    const fn map_info(&self) -> Option<u32> {
        match self {
            Self::Map { map_info, .. } => *map_info,
            Self::Unit(_) | Self::Context(_) => None,
        }
    }
}

#[must_use]
pub struct TicketCompletion<E, K> {
    origin: CompletionOrigin,
    control: TicketControl<E>,
    kind: PhantomData<fn(K) -> K>,
}

impl<E, K> TicketCompletion<E, K> {
    pub const fn expected(&self) -> ExpectedReply {
        self.control.expected()
    }

    pub const fn verb(&self) -> ControlVerb {
        self.control.verb()
    }

    pub const fn subject(&self) -> ControlSubject {
        self.control.subject()
    }

    pub const fn sequence(&self) -> u64 {
        self.control.sequence()
    }

    pub const fn map_info(&self) -> Option<u32> {
        self.control.map_info()
    }

    pub fn apply_unit<T, F>(self, apply: F) -> Result<T, Self>
    where
        F: FnOnce(ClassifiedControl<(), E>) -> Result<T, ClassifiedControl<(), E>>,
    {
        let Self {
            origin, control, ..
        } = self;
        match control {
            TicketControl::Unit(completion) => apply(completion).map_err(|completion| Self {
                origin,
                control: TicketControl::Unit(completion),
                kind: PhantomData,
            }),
            control => Err(Self {
                origin,
                control,
                kind: PhantomData,
            }),
        }
    }

    pub fn apply_context<T, F>(self, apply: F) -> Result<T, Self>
    where
        F: FnOnce(
            ClassifiedControl<ExactNoData, E>,
        ) -> Result<T, ClassifiedControl<ExactNoData, E>>,
    {
        let Self {
            origin, control, ..
        } = self;
        match control {
            TicketControl::Context(completion) => apply(completion).map_err(|completion| Self {
                origin,
                control: TicketControl::Context(completion),
                kind: PhantomData,
            }),
            control => Err(Self {
                origin,
                control,
                kind: PhantomData,
            }),
        }
    }

    pub fn apply_map<T, F>(self, apply: F) -> Result<(T, Option<u32>), Self>
    where
        F: FnOnce(ClassifiedControl<(), E>) -> Result<T, ClassifiedControl<(), E>>,
    {
        let Self {
            origin, control, ..
        } = self;
        match control {
            TicketControl::Map {
                completion,
                map_info,
            } => apply(completion)
                .map(|value| (value, map_info))
                .map_err(|completion| Self {
                    origin,
                    control: TicketControl::Map {
                        completion,
                        map_info,
                    },
                    kind: PhantomData,
                }),
            control => Err(Self {
                origin,
                control,
                kind: PhantomData,
            }),
        }
    }

    fn matches(&self, identity: TicketIdentity) -> bool {
        if self.origin != identity.completion_origin()
            || self.expected() != identity.expected
            || self.verb() != identity.verb
            || self.subject() != identity.subject
            || self.sequence() != identity.sequence
        {
            return false;
        }
        match &self.control {
            TicketControl::Map {
                completion,
                map_info,
            } => {
                matches!(completion.outcome(), ControlOutcome::Completed(Ok(())))
                    == map_info.is_some()
            }
            TicketControl::Unit(_) | TicketControl::Context(_) => true,
        }
    }
}

#[derive(Clone, Copy)]
struct EmptyState {
    row_incarnation_high_water: u64,
    sequence_high_water: u64,
}

struct ReservedState<R> {
    row: RowIdentity,
    sequence_high_water: u64,
    rundown: ManuallyDrop<R>,
}

struct PreparedState<R> {
    row: RowIdentity,
    request: ManuallyDrop<PreparedControl>,
    rundown: ManuallyDrop<R>,
}

struct PendingState<R, E, K> {
    row: RowIdentity,
    completion: ManuallyDrop<TicketControl<E>>,
    rundown: ManuallyDrop<R>,
    poisoned: bool,
    kind: PhantomData<fn(K) -> K>,
}

struct ApplyingState<R> {
    row: RowIdentity,
    rundown: ManuallyDrop<R>,
}

impl<R> PreparedState<R> {
    fn identity(&self) -> TicketIdentity {
        let request = &*self.request;
        TicketIdentity {
            row: self.row,
            sequence: request.sequence(),
            verb: request.verb(),
            subject: request.subject(),
            expected: ExpectedReply::for_verb(request.verb()),
        }
    }
}

impl<R, E, K> PendingState<R, E, K> {
    fn identity(&self) -> TicketIdentity {
        TicketIdentity {
            row: self.row,
            sequence: self.completion.sequence(),
            verb: self.completion.verb(),
            subject: self.completion.subject(),
            expected: self.completion.expected(),
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ReleaseState {
    row: RowIdentity,
    sequence_high_water: u64,
}

enum TicketStorage<R, E, K> {
    Empty(EmptyState),
    Reserved(ReservedState<R>),
    Prepared(PreparedState<R>),
    MayHaveSubmitted(PreparedState<R>),
    LifecyclePending(PendingState<R, E, K>),
    LifecycleApplying(ApplyingState<R>),
    ReleasePending(ReleaseState),
    ResetPending(RowIdentity),
    Retired,
}

pub struct ControlTicketSlot<R, E, K> {
    storage: TicketStorage<R, E, K>,
}

impl<R, E, K> ControlTicketSlot<R, E, K> {
    pub const fn empty() -> Self {
        Self {
            storage: TicketStorage::Empty(EmptyState {
                row_incarnation_high_water: 0,
                sequence_high_water: 0,
            }),
        }
    }

    pub const fn state(&self) -> TicketState {
        self.storage.state()
    }

    pub(crate) const fn is_fresh(&self) -> bool {
        matches!(
            self.storage,
            TicketStorage::Empty(EmptyState {
                row_incarnation_high_water: 0,
                sequence_high_water: 0,
            })
        )
    }
}

impl<R, E, K> TicketStorage<R, E, K> {
    const fn state(&self) -> TicketState {
        match self {
            Self::Empty(_) => TicketState::Empty,
            Self::Reserved(_) => TicketState::Reserved,
            Self::Prepared(_) => TicketState::Prepared,
            Self::MayHaveSubmitted(_) => TicketState::MayHaveSubmitted,
            Self::LifecyclePending(pending) if pending.poisoned => TicketState::LifecyclePoisoned,
            Self::LifecyclePending(_) => TicketState::LifecyclePending,
            Self::LifecycleApplying(_) => TicketState::LifecycleApplying,
            Self::ReleasePending(_) => TicketState::ReleasePending,
            Self::ResetPending(_) => TicketState::ResetPending,
            Self::Retired => TicketState::Retired,
        }
    }
}

#[must_use]
pub struct TicketReservation<K> {
    row: RowIdentity,
    kind: PhantomData<fn(K) -> K>,
}

impl<K> TicketReservation<K> {
    /// # Safety
    ///
    /// The caller will next attempt the exact canonical row lifecycle begin
    /// and will retain this post-begin custody until that begin is refused
    /// without mutation, the resulting request is installed, or reset retires
    /// the reserved row.
    pub unsafe fn begin_lifecycle(self) -> PostBeginReservation<K> {
        PostBeginReservation {
            row: self.row,
            kind: PhantomData,
        }
    }
}

#[must_use]
pub struct PostBeginReservation<K> {
    row: RowIdentity,
    kind: PhantomData<fn(K) -> K>,
}

impl<K> PostBeginReservation<K> {
    /// # Safety
    ///
    /// The exact canonical row lifecycle refused its begin without mutation,
    /// so no request or pending lifecycle operation exists.
    pub unsafe fn assume_lifecycle_not_begun(self) -> TicketReservation<K> {
        TicketReservation {
            row: self.row,
            kind: PhantomData,
        }
    }

    /// # Safety
    ///
    /// The refused, still-unpublished request was classified
    /// definite-not-enqueued and its exact row lifecycle accepted that
    /// reconciliation. A host rejection cannot authorize this conversion.
    pub unsafe fn assume_lifecycle_reconciled(self) -> TicketReservation<K> {
        TicketReservation {
            row: self.row,
            kind: PhantomData,
        }
    }
}

#[must_use]
pub struct PreparedTicket<K> {
    key: TicketKey,
    kind: PhantomData<fn(K) -> K>,
}

#[must_use]
pub struct DispatchPermit<K> {
    identity: TicketIdentity,
    kind: PhantomData<fn(K) -> K>,
}

#[must_use]
pub struct LifecycleAction<K> {
    key: TicketKey,
    kind: PhantomData<fn(K) -> K>,
}

#[must_use]
pub struct RefusedAction<A> {
    reason: TicketRefusal,
    action: A,
}

impl<A> RefusedAction<A> {
    pub const fn reason(&self) -> TicketRefusal {
        self.reason
    }

    pub fn into_action(self) -> A {
        self.action
    }
}

impl<A> fmt::Debug for RefusedAction<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedAction")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

#[must_use]
pub struct RefusedReserve<R> {
    reason: TicketRefusal,
    rundown: ManuallyDrop<R>,
}

impl<R> RefusedReserve<R> {
    pub const fn reason(&self) -> TicketRefusal {
        self.reason
    }

    pub fn into_rundown(self) -> R {
        ManuallyDrop::into_inner(self.rundown)
    }
}

impl<R> fmt::Debug for RefusedReserve<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedReserve")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

#[must_use]
pub struct RefusedInstall<K> {
    reason: TicketRefusal,
    reservation: PostBeginReservation<K>,
    request: ManuallyDrop<PreparedControl>,
}

impl<K> RefusedInstall<K> {
    pub const fn reason(&self) -> TicketRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (PostBeginReservation<K>, PreparedControl) {
        (self.reservation, ManuallyDrop::into_inner(self.request))
    }
}

impl<K> fmt::Debug for RefusedInstall<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedInstall")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

#[must_use]
pub struct RefusedCancel<E, K> {
    reason: TicketRefusal,
    ticket: PreparedTicket<K>,
    error: ManuallyDrop<E>,
}

impl<E, K> RefusedCancel<E, K> {
    pub const fn reason(&self) -> TicketRefusal {
        self.reason
    }

    pub fn into_parts(self) -> (PreparedTicket<K>, E) {
        (self.ticket, ManuallyDrop::into_inner(self.error))
    }
}

impl<E, K> fmt::Debug for RefusedCancel<E, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefusedCancel")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum ObservationView<'a, E> {
    DefiniteNotEnqueued(&'a E),
    UnitCompleted(Result<(), HostRejection>),
    ContextCompleted(Result<(), HostRejection>),
    MapCompleted(Result<u32, HostRejection>),
    Ambiguous(AbandonReason),
}

pub struct PendingControlView<'a, E, K> {
    identity: TicketIdentity,
    completion: &'a TicketControl<E>,
    kind: PhantomData<fn(K) -> K>,
}

impl<'a, E, K> PendingControlView<'a, E, K> {
    pub const fn verb(&self) -> ControlVerb {
        self.identity.verb
    }

    pub const fn subject(&self) -> ControlSubject {
        self.identity.subject
    }

    pub const fn sequence(&self) -> u64 {
        self.identity.sequence
    }

    pub const fn expected(&self) -> ExpectedReply {
        self.identity.expected
    }

    pub fn observation(&self) -> ObservationView<'a, E> {
        match self.completion {
            TicketControl::Unit(completion) => match completion.outcome() {
                ControlOutcome::DefiniteNotEnqueued(error) => {
                    ObservationView::DefiniteNotEnqueued(error)
                }
                ControlOutcome::Completed(Ok(())) => ObservationView::UnitCompleted(Ok(())),
                ControlOutcome::Completed(Err(rejection)) => {
                    ObservationView::UnitCompleted(Err(*rejection))
                }
                ControlOutcome::Ambiguous(abandoned) => {
                    ObservationView::Ambiguous(abandoned.reason())
                }
            },
            TicketControl::Context(completion) => match completion.outcome() {
                ControlOutcome::DefiniteNotEnqueued(error) => {
                    ObservationView::DefiniteNotEnqueued(error)
                }
                ControlOutcome::Completed(Ok(_)) => ObservationView::ContextCompleted(Ok(())),
                ControlOutcome::Completed(Err(rejection)) => {
                    ObservationView::ContextCompleted(Err(*rejection))
                }
                ControlOutcome::Ambiguous(abandoned) => {
                    ObservationView::Ambiguous(abandoned.reason())
                }
            },
            TicketControl::Map {
                completion,
                map_info,
            } => match completion.outcome() {
                ControlOutcome::DefiniteNotEnqueued(error) => {
                    ObservationView::DefiniteNotEnqueued(error)
                }
                ControlOutcome::Completed(Ok(())) => match map_info {
                    Some(map_info) => ObservationView::MapCompleted(Ok(*map_info)),
                    None => ObservationView::Ambiguous(AbandonReason::MalformedResponse),
                },
                ControlOutcome::Completed(Err(rejection)) => {
                    ObservationView::MapCompleted(Err(*rejection))
                }
                ControlOutcome::Ambiguous(abandoned) => {
                    ObservationView::Ambiguous(abandoned.reason())
                }
            },
        }
    }
}

#[must_use]
pub struct ObservedDispatch<E, K> {
    key: TicketKey,
    outcome: ManuallyDrop<RunnerOutcome<E>>,
    kind: PhantomData<fn(K) -> K>,
}

impl<K> DispatchPermit<K> {
    /// # Safety
    ///
    /// The caller runs `runner` without the owner/table spin lock held and
    /// without reentering the table. The runner must encode and publish this
    /// exact command at most once, retain no usable key, and return only that
    /// descriptor's lossless outcome, including the actual written length.
    /// Definite-not-enqueued is valid only before transport acceptance. A
    /// physical reset must not race a live permit or runner.
    pub unsafe fn run_once<E, F>(self, runner: F) -> ObservedDispatch<E, K>
    where
        F: FnOnce(ControlWireKey) -> RunnerOutcome<E>,
    {
        let identity = self.identity;
        let outcome = runner(ControlWireKey {
            verb: identity.verb,
            subject: identity.subject,
            sequence: identity.sequence,
        });
        ObservedDispatch {
            key: identity.key(),
            outcome: ManuallyDrop::new(outcome),
            kind: PhantomData,
        }
    }
}

#[must_use]
pub enum ApplyRefusal<F, K> {
    Before {
        reason: TicketRefusal,
        action: LifecycleAction<K>,
        apply: ManuallyDrop<F>,
    },
    LifecycleRefused {
        action: LifecycleAction<K>,
    },
    CompletionMismatch,
}

impl<F, K> ApplyRefusal<F, K> {
    pub const fn reason(&self) -> Option<TicketRefusal> {
        match self {
            Self::Before { reason, .. } => Some(*reason),
            Self::LifecycleRefused { .. } | Self::CompletionMismatch => None,
        }
    }

    pub fn into_before_parts(self) -> Result<(LifecycleAction<K>, F), Self> {
        match self {
            Self::Before { action, apply, .. } => Ok((action, ManuallyDrop::into_inner(apply))),
            refusal => Err(refusal),
        }
    }

    pub fn into_lifecycle_action(self) -> Result<LifecycleAction<K>, Self> {
        match self {
            Self::LifecycleRefused { action } => Ok(action),
            refusal => Err(refusal),
        }
    }
}

impl<F, K> fmt::Debug for ApplyRefusal<F, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Before { reason, .. } => f
                .debug_struct("ApplyRefusal")
                .field("reason", reason)
                .finish_non_exhaustive(),
            Self::LifecycleRefused { .. } => f.write_str("ApplyRefusal::LifecycleRefused"),
            Self::CompletionMismatch => f.write_str("ApplyRefusal::CompletionMismatch"),
        }
    }
}

#[must_use]
pub struct RundownRelease<R, K> {
    release: ReleaseState,
    rundown: ManuallyDrop<R>,
    kind: PhantomData<fn(K) -> K>,
}

impl<R, K> RundownRelease<R, K> {
    pub fn into_parts(self) -> (R, PendingReleaseAck<K>) {
        (
            ManuallyDrop::into_inner(self.rundown),
            PendingReleaseAck {
                release: self.release,
                kind: PhantomData,
            },
        )
    }
}

#[must_use]
pub struct PendingReleaseAck<K> {
    release: ReleaseState,
    kind: PhantomData<fn(K) -> K>,
}

impl<K> PendingReleaseAck<K> {
    /// # Safety
    ///
    /// The rundown token has been released or irreversibly quarantined.
    pub unsafe fn assume_released(self) -> FinalizedReleaseAck<K> {
        FinalizedReleaseAck {
            release: self.release,
            kind: PhantomData,
        }
    }
}

#[must_use]
pub struct FinalizedReleaseAck<K> {
    release: ReleaseState,
    kind: PhantomData<fn(K) -> K>,
}

#[must_use]
pub struct ResetRelease<R, K> {
    row: RowIdentity,
    rundown: ManuallyDrop<R>,
    kind: PhantomData<fn(K) -> K>,
}

impl<R, K> ResetRelease<R, K> {
    pub fn into_parts(self) -> (R, PendingResetAck<K>) {
        (
            ManuallyDrop::into_inner(self.rundown),
            PendingResetAck {
                row: self.row,
                kind: PhantomData,
            },
        )
    }
}

#[must_use]
pub struct PendingResetAck<K> {
    row: RowIdentity,
    kind: PhantomData<fn(K) -> K>,
}

impl<K> PendingResetAck<K> {
    /// # Safety
    ///
    /// Reset cleanup released or irreversibly quarantined every extracted
    /// token.
    pub unsafe fn assume_released(self) -> FinalizedResetAck<K> {
        FinalizedResetAck {
            row: self.row,
            kind: PhantomData,
        }
    }
}

#[must_use]
pub struct FinalizedResetAck<K> {
    row: RowIdentity,
    kind: PhantomData<fn(K) -> K>,
}

pub struct ControlTickets<'a, R, E, K> {
    table: SlotTableId,
    epoch: TransportEpoch,
    slots: &'a mut [ControlTicketSlot<R, E, K>],
    kind: PhantomData<fn(K) -> K>,
}

impl<'a, R, E, K: TicketRowKind> ControlTickets<'a, R, E, K> {
    /// # Safety
    ///
    /// `table` descends from its live unique root. `slots` is the sole
    /// canonical ticket array for (`table`, `epoch`, `K`) and has the exact
    /// stable-row table's length and index map. Its storage remains bound to
    /// `table` and `K`; epoch rebinding requires only Empty/Retired slots, no
    /// live ticket/action/permit/observation/ack, and completed producer and
    /// effect rundown for every copied old-epoch wire key.
    pub unsafe fn new(
        table: SlotTableId,
        epoch: TransportEpoch,
        slots: &'a mut [ControlTicketSlot<R, E, K>],
    ) -> Result<Self, TicketTableRefusal> {
        if slots.len() > u32::MAX as usize {
            return Err(TicketTableRefusal::CapacityTooLarge { found: slots.len() });
        }
        Ok(Self {
            table,
            epoch,
            slots,
            kind: PhantomData,
        })
    }

    /// Safety: the caller established every `new` precondition and validated
    /// that `slots.len()` is representable as `u32`.
    pub(crate) unsafe fn new_canonical(
        table: SlotTableId,
        epoch: TransportEpoch,
        slots: &'a mut [ControlTicketSlot<R, E, K>],
    ) -> Self {
        Self {
            table,
            epoch,
            slots,
            kind: PhantomData,
        }
    }

    pub fn state_at(&self, index: u32) -> Option<TicketState> {
        self.slots.get(index as usize).map(ControlTicketSlot::state)
    }

    pub(crate) fn validate_epoch_rebind(&self) -> Result<(), TicketEpochRebindRefusal> {
        for (index, slot) in self.slots.iter().enumerate() {
            let state = slot.state();
            if !matches!(state, TicketState::Empty | TicketState::Retired) {
                return Err(TicketEpochRebindRefusal {
                    index: index as u32,
                    state,
                });
            }
        }
        Ok(())
    }

    /// Safety: all canonical arrays passed read-only rebind validation, every
    /// old authority/rundown is drained, and `epoch` is the exact successor.
    pub(crate) unsafe fn apply_epoch_rebind(
        &mut self,
        epoch: TransportEpoch,
        mut stable_slot_is_vacant: impl FnMut(u32) -> bool,
    ) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if stable_slot_is_vacant(index as u32) {
                if matches!(slot.storage, TicketStorage::Retired) {
                    slot.storage = TicketStorage::Empty(EmptyState {
                        row_incarnation_high_water: 0,
                        sequence_high_water: 0,
                    });
                }
            } else {
                slot.storage = TicketStorage::Retired;
            }
        }
        self.epoch = epoch;
    }

    /// # Safety
    ///
    /// `row` is the exact currently Occupied stable row, `rundown` is its
    /// unique producer/effect custody, and the exact row lifecycle is idle
    /// with no begun request. The sole owner keeps both live or quarantined
    /// through this ticket and consumes the returned reservation through
    /// `begin_lifecycle` before invoking that lifecycle's begin operation.
    pub unsafe fn reserve(
        &mut self,
        row: SlotHandle<K>,
        rundown: R,
    ) -> Result<TicketReservation<K>, RefusedReserve<R>> {
        let row = RowIdentity::from_handle(row);
        let index = match self.checked_row(row) {
            Ok(index) => index,
            Err(reason) => {
                return Err(RefusedReserve {
                    reason,
                    rundown: ManuallyDrop::new(rundown),
                });
            }
        };
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        match old {
            TicketStorage::Empty(empty) => {
                if row.incarnation < empty.row_incarnation_high_water {
                    let high_water = empty.row_incarnation_high_water;
                    self.slots[index].storage = TicketStorage::Empty(empty);
                    return Err(RefusedReserve {
                        reason: TicketRefusal::RowIncarnationWentBackward {
                            high_water,
                            found: row.incarnation,
                        },
                        rundown: ManuallyDrop::new(rundown),
                    });
                }
                let sequence_high_water = if row.incarnation == empty.row_incarnation_high_water {
                    empty.sequence_high_water
                } else {
                    0
                };
                self.slots[index].storage = TicketStorage::Reserved(ReservedState {
                    row,
                    sequence_high_water,
                    rundown: ManuallyDrop::new(rundown),
                });
                Ok(TicketReservation {
                    row,
                    kind: PhantomData,
                })
            }
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                Err(RefusedReserve {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::Empty,
                        found,
                    },
                    rundown: ManuallyDrop::new(rundown),
                })
            }
        }
    }

    /// # Safety
    ///
    /// `request` was minted by the reserved row's exact lifecycle after
    /// `reservation` entered post-begin custody, its subject is that
    /// lifecycle's canonical object, and no intervening row or lifecycle
    /// mutation occurred. The request has never been encoded, accepted,
    /// enqueued, or otherwise published, the caller retained no usable wire
    /// tuple or publication authority, and this call transfers its sole
    /// prepublication custody into the table.
    pub unsafe fn install(
        &mut self,
        reservation: PostBeginReservation<K>,
        request: PreparedControl,
    ) -> Result<PreparedTicket<K>, RefusedInstall<K>> {
        let row = reservation.row;
        let index = match self.checked_row(row) {
            Ok(index) => index,
            Err(reason) => {
                return Err(RefusedInstall {
                    reason,
                    reservation,
                    request: ManuallyDrop::new(request),
                });
            }
        };
        let found = self.slots[index].storage.state();
        let reserved = match &self.slots[index].storage {
            TicketStorage::Reserved(reserved) if reserved.row == row => reserved,
            TicketStorage::Reserved(reserved) => {
                return Err(RefusedInstall {
                    reason: TicketRefusal::RowIncarnationMismatch {
                        expected: reserved.row.incarnation,
                        found: row.incarnation,
                    },
                    reservation,
                    request: ManuallyDrop::new(request),
                });
            }
            _ => {
                return Err(RefusedInstall {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::Reserved,
                        found,
                    },
                    reservation,
                    request: ManuallyDrop::new(request),
                });
            }
        };
        if request.subject().epoch() != self.epoch {
            return Err(RefusedInstall {
                reason: TicketRefusal::SubjectEpochMismatch {
                    expected: self.epoch,
                    found: request.subject().epoch(),
                },
                reservation,
                request: ManuallyDrop::new(request),
            });
        }
        if !Self::verb_accepts_subject(request.verb(), request.subject()) {
            return Err(RefusedInstall {
                reason: TicketRefusal::ControlSubjectMismatch,
                reservation,
                request: ManuallyDrop::new(request),
            });
        }
        let sequence = request.sequence();
        let high_water = reserved.sequence_high_water;
        if high_water == u64::MAX {
            return Err(RefusedInstall {
                reason: TicketRefusal::SequenceExhausted,
                reservation,
                request: ManuallyDrop::new(request),
            });
        }
        if sequence <= high_water {
            return Err(RefusedInstall {
                reason: TicketRefusal::SequenceReusedOrWentBackward {
                    high_water,
                    found: sequence,
                },
                reservation,
                request: ManuallyDrop::new(request),
            });
        }
        let identity = TicketIdentity {
            row,
            sequence,
            verb: request.verb(),
            subject: request.subject(),
            expected: ExpectedReply::for_verb(request.verb()),
        };
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        match old {
            TicketStorage::Reserved(reserved) => {
                self.slots[index].storage = TicketStorage::Prepared(PreparedState {
                    row,
                    request: ManuallyDrop::new(request),
                    rundown: reserved.rundown,
                });
                Ok(PreparedTicket {
                    key: identity.key(),
                    kind: PhantomData,
                })
            }
            storage => {
                self.slots[index].storage = storage;
                Err(RefusedInstall {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::Reserved,
                        found,
                    },
                    reservation,
                    request: ManuallyDrop::new(request),
                })
            }
        }
    }

    pub fn begin_dispatch(
        &mut self,
        ticket: PreparedTicket<K>,
    ) -> Result<DispatchPermit<K>, RefusedAction<PreparedTicket<K>>> {
        let key = ticket.key;
        let (index, identity) = match self.checked_key(key, TicketState::Prepared) {
            Ok(found) => found,
            Err(reason) => {
                return Err(RefusedAction {
                    reason,
                    action: ticket,
                })
            }
        };
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        match old {
            TicketStorage::Prepared(prepared) => {
                self.slots[index].storage = TicketStorage::MayHaveSubmitted(prepared);
                Ok(DispatchPermit {
                    identity,
                    kind: PhantomData,
                })
            }
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                Err(RefusedAction {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::Prepared,
                        found,
                    },
                    action: ticket,
                })
            }
        }
    }

    pub(crate) fn prepared_verb(
        &self,
        ticket: &PreparedTicket<K>,
    ) -> Result<ControlVerb, TicketRefusal> {
        self.checked_key(ticket.key, TicketState::Prepared)
            .map(|(_, identity)| identity.verb)
    }

    pub fn cancel_reservation(
        &mut self,
        reservation: TicketReservation<K>,
    ) -> Result<RundownRelease<R, K>, RefusedAction<TicketReservation<K>>> {
        let row = reservation.row;
        let index = match self.checked_row(row) {
            Ok(index) => index,
            Err(reason) => {
                return Err(RefusedAction {
                    reason,
                    action: reservation,
                })
            }
        };
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        match old {
            TicketStorage::Reserved(reserved) if reserved.row == row => {
                let release = ReleaseState {
                    row,
                    sequence_high_water: reserved.sequence_high_water,
                };
                self.slots[index].storage = TicketStorage::ReleasePending(release);
                Ok(RundownRelease {
                    release,
                    rundown: reserved.rundown,
                    kind: PhantomData,
                })
            }
            TicketStorage::Reserved(reserved) => {
                let expected = reserved.row.incarnation;
                self.slots[index].storage = TicketStorage::Reserved(reserved);
                Err(RefusedAction {
                    reason: TicketRefusal::RowIncarnationMismatch {
                        expected,
                        found: row.incarnation,
                    },
                    action: reservation,
                })
            }
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                Err(RefusedAction {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::Reserved,
                        found,
                    },
                    action: reservation,
                })
            }
        }
    }

    pub(crate) fn validate_reservation(
        &self,
        reservation: &TicketReservation<K>,
    ) -> Result<(), TicketRefusal> {
        let row = reservation.row;
        let index = self.checked_row(row)?;
        match &self.slots[index].storage {
            TicketStorage::Reserved(found) if found.row == row => Ok(()),
            TicketStorage::Reserved(found) => Err(TicketRefusal::RowIncarnationMismatch {
                expected: found.row.incarnation,
                found: row.incarnation,
            }),
            storage => Err(TicketRefusal::WrongState {
                expected: TicketState::Reserved,
                found: storage.state(),
            }),
        }
    }

    pub(crate) fn can_commit_empty(&self, row: SlotHandle<K>) -> bool {
        let row = RowIdentity::from_handle(row);
        let Ok(index) = self.checked_row(row) else {
            return false;
        };
        matches!(
            &self.slots[index].storage,
            TicketStorage::Empty(empty)
                if row.incarnation >= empty.row_incarnation_high_water
        )
    }

    /// Safety: `reservation` passed exact validation, its lifecycle never
    /// began, and its rundown is fully finalized by the sole table owner.
    pub(crate) unsafe fn cancel_reservation_validated(
        &mut self,
        reservation: TicketReservation<K>,
    ) -> Option<R> {
        let row = reservation.row;
        let index = row.index as usize;
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        let TicketStorage::Reserved(reserved) = old else {
            self.slots[index].storage = old;
            return None;
        };
        if reserved.row != row {
            self.slots[index].storage = TicketStorage::Reserved(reserved);
            return None;
        }
        self.slots[index].storage = TicketStorage::Empty(EmptyState {
            row_incarnation_high_water: reserved.row.incarnation,
            sequence_high_water: reserved.sequence_high_water,
        });
        Some(ManuallyDrop::into_inner(reserved.rundown))
    }

    pub fn cancel_prepared(
        &mut self,
        ticket: PreparedTicket<K>,
        error: E,
    ) -> Result<LifecycleAction<K>, RefusedCancel<E, K>> {
        let key = ticket.key;
        let (index, identity) = match self.checked_key(key, TicketState::Prepared) {
            Ok(found) => found,
            Err(reason) => {
                return Err(RefusedCancel {
                    reason,
                    ticket,
                    error: ManuallyDrop::new(error),
                })
            }
        };
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        match old {
            TicketStorage::Prepared(prepared) => {
                let request = ManuallyDrop::into_inner(prepared.request);
                self.slots[index].storage = TicketStorage::LifecyclePending(PendingState {
                    row: identity.row,
                    completion: ManuallyDrop::new(Self::classify_definite_not_enqueued(
                        identity, request, error,
                    )),
                    rundown: prepared.rundown,
                    poisoned: false,
                    kind: PhantomData,
                });
                Ok(LifecycleAction {
                    key,
                    kind: PhantomData,
                })
            }
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                Err(RefusedCancel {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::Prepared,
                        found,
                    },
                    ticket,
                    error: ManuallyDrop::new(error),
                })
            }
        }
    }

    pub fn finish_observed(
        &mut self,
        observed: ObservedDispatch<E, K>,
    ) -> Result<LifecycleAction<K>, RefusedAction<ObservedDispatch<E, K>>> {
        let key = observed.key;
        let (index, identity) = match self.checked_key(key, TicketState::MayHaveSubmitted) {
            Ok(found) => found,
            Err(reason) => {
                return Err(RefusedAction {
                    reason,
                    action: observed,
                });
            }
        };
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        let prepared = match old {
            TicketStorage::MayHaveSubmitted(prepared) => prepared,
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                return Err(RefusedAction {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::MayHaveSubmitted,
                        found,
                    },
                    action: observed,
                });
            }
        };
        let request = ManuallyDrop::into_inner(prepared.request);
        let outcome = ManuallyDrop::into_inner(observed.outcome);
        self.slots[index].storage = TicketStorage::LifecyclePending(PendingState {
            row: identity.row,
            completion: ManuallyDrop::new(Self::normalize(identity, request, outcome)),
            rundown: prepared.rundown,
            poisoned: false,
            kind: PhantomData,
        });
        Ok(LifecycleAction {
            key: identity.key(),
            kind: PhantomData,
        })
    }

    pub fn pending_control<'b>(
        &'b self,
        action: &LifecycleAction<K>,
    ) -> Result<PendingControlView<'b, E, K>, TicketRefusal> {
        let key = action.key;
        let (index, identity) = self.checked_key(key, TicketState::LifecyclePending)?;
        let TicketStorage::LifecyclePending(pending) = &self.slots[index].storage else {
            return Err(TicketRefusal::WrongState {
                expected: TicketState::LifecyclePending,
                found: self.slots[index].storage.state(),
            });
        };
        Ok(PendingControlView {
            identity,
            completion: &pending.completion,
            kind: PhantomData,
        })
    }

    /// # Safety
    ///
    /// `apply` runs under the owner lock, mutates only the exact canonical row
    /// lifecycle, performs no wait or table reentry, and returns `Err` with the
    /// exact refused completion unchanged. `Ok` means that lifecycle consumed
    /// and accepted the completion.
    pub unsafe fn apply_once<F, T>(
        &mut self,
        action: LifecycleAction<K>,
        apply: F,
    ) -> Result<(T, RundownRelease<R, K>), ApplyRefusal<F, K>>
    where
        F: FnOnce(TicketCompletion<E, K>) -> Result<T, TicketCompletion<E, K>>,
    {
        let key = action.key;
        let (index, identity) = match self.checked_key(key, TicketState::LifecyclePending) {
            Ok(found) => found,
            Err(reason) => {
                return Err(ApplyRefusal::Before {
                    reason,
                    action,
                    apply: ManuallyDrop::new(apply),
                });
            }
        };
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        let pending = match old {
            TicketStorage::LifecyclePending(pending) => pending,
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                return Err(ApplyRefusal::Before {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::LifecyclePending,
                        found,
                    },
                    action,
                    apply: ManuallyDrop::new(apply),
                });
            }
        };
        self.slots[index].storage = TicketStorage::LifecycleApplying(ApplyingState {
            row: identity.row,
            rundown: pending.rundown,
        });
        let applying = match &mut self.slots[index].storage {
            TicketStorage::LifecycleApplying(applying) => applying,
            _ => {
                return Err(ApplyRefusal::Before {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::LifecycleApplying,
                        found: TicketState::Retired,
                    },
                    action,
                    apply: ManuallyDrop::new(apply),
                });
            }
        };
        let completion = TicketCompletion {
            origin: identity.completion_origin(),
            control: ManuallyDrop::into_inner(pending.completion),
            kind: PhantomData,
        };
        match apply(completion) {
            Ok(value) => {
                let rundown = unsafe { ManuallyDrop::take(&mut applying.rundown) };
                let release = ReleaseState {
                    row: identity.row,
                    sequence_high_water: identity.sequence,
                };
                self.slots[index].storage = TicketStorage::ReleasePending(release);
                Ok((
                    value,
                    RundownRelease {
                        release,
                        rundown: ManuallyDrop::new(rundown),
                        kind: PhantomData,
                    },
                ))
            }
            Err(completion) if completion.matches(identity) => {
                let TicketCompletion { control, .. } = completion;
                let rundown = unsafe { ManuallyDrop::take(&mut applying.rundown) };
                self.slots[index].storage = TicketStorage::LifecyclePending(PendingState {
                    row: identity.row,
                    completion: ManuallyDrop::new(control),
                    rundown: ManuallyDrop::new(rundown),
                    poisoned: false,
                    kind: PhantomData,
                });
                Err(ApplyRefusal::LifecycleRefused { action })
            }
            Err(completion) => {
                let TicketCompletion { control, .. } = completion;
                let rundown = unsafe { ManuallyDrop::take(&mut applying.rundown) };
                self.slots[index].storage = TicketStorage::LifecyclePending(PendingState {
                    row: identity.row,
                    completion: ManuallyDrop::new(control),
                    rundown: ManuallyDrop::new(rundown),
                    poisoned: true,
                    kind: PhantomData,
                });
                Err(ApplyRefusal::CompletionMismatch)
            }
        }
    }

    pub fn ack_release(
        &mut self,
        ack: FinalizedReleaseAck<K>,
    ) -> Result<(), RefusedAction<FinalizedReleaseAck<K>>> {
        let release = ack.release;
        let index = match self.checked_row(release.row) {
            Ok(index) => index,
            Err(reason) => {
                return Err(RefusedAction {
                    reason,
                    action: ack,
                })
            }
        };
        match &self.slots[index].storage {
            TicketStorage::ReleasePending(found) if *found == release => {}
            TicketStorage::ReleasePending(found)
                if found.row.incarnation != release.row.incarnation =>
            {
                return Err(RefusedAction {
                    reason: TicketRefusal::RowIncarnationMismatch {
                        expected: found.row.incarnation,
                        found: release.row.incarnation,
                    },
                    action: ack,
                });
            }
            TicketStorage::ReleasePending(found) => {
                return Err(RefusedAction {
                    reason: TicketRefusal::ControlSequenceMismatch {
                        expected: found.sequence_high_water,
                        found: release.sequence_high_water,
                    },
                    action: ack,
                });
            }
            storage => {
                return Err(RefusedAction {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::ReleasePending,
                        found: storage.state(),
                    },
                    action: ack,
                });
            }
        }
        self.slots[index].storage = TicketStorage::Empty(EmptyState {
            row_incarnation_high_water: release.row.incarnation,
            sequence_high_water: release.sequence_high_water,
        });
        Ok(())
    }

    pub(crate) fn consume_rundown_release(
        &mut self,
        release: RundownRelease<R, K>,
    ) -> Result<R, RefusedAction<RundownRelease<R, K>>> {
        let expected = release.release;
        let index = match self.checked_row(expected.row) {
            Ok(index) => index,
            Err(reason) => {
                return Err(RefusedAction {
                    reason,
                    action: release,
                })
            }
        };
        match &self.slots[index].storage {
            TicketStorage::ReleasePending(found) if *found == expected => {}
            TicketStorage::ReleasePending(found) => {
                return Err(RefusedAction {
                    reason: TicketRefusal::ControlSequenceMismatch {
                        expected: found.sequence_high_water,
                        found: expected.sequence_high_water,
                    },
                    action: release,
                })
            }
            storage => {
                return Err(RefusedAction {
                    reason: TicketRefusal::WrongState {
                        expected: TicketState::ReleasePending,
                        found: storage.state(),
                    },
                    action: release,
                })
            }
        }
        let RundownRelease { rundown, .. } = release;
        self.slots[index].storage = TicketStorage::Empty(EmptyState {
            row_incarnation_high_water: expected.row.incarnation,
            sequence_high_water: expected.sequence_high_water,
        });
        Ok(ManuallyDrop::into_inner(rundown))
    }

    pub(crate) fn validate_rundown_release(
        &self,
        release: &RundownRelease<R, K>,
    ) -> Result<(), TicketRefusal> {
        let expected = release.release;
        let index = self.checked_row(expected.row)?;
        match &self.slots[index].storage {
            TicketStorage::ReleasePending(found) if *found == expected => Ok(()),
            TicketStorage::ReleasePending(found) => Err(TicketRefusal::ControlSequenceMismatch {
                expected: found.sequence_high_water,
                found: expected.sequence_high_water,
            }),
            storage => Err(TicketRefusal::WrongState {
                expected: TicketState::ReleasePending,
                found: storage.state(),
            }),
        }
    }

    /// Safety: `release` passed exact validation and every owner-side effect
    /// is committed; this is the final, infallible mutation for that control.
    pub(crate) unsafe fn consume_rundown_release_validated(
        &mut self,
        release: RundownRelease<R, K>,
    ) -> R {
        let expected = release.release;
        let index = expected.row.index as usize;
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        let found = match old {
            TicketStorage::ReleasePending(found) if found == expected => found,
            storage => {
                self.slots[index].storage = storage;
                return ManuallyDrop::into_inner(release.rundown);
            }
        };
        let empty = EmptyState {
            row_incarnation_high_water: found.row.incarnation,
            sequence_high_water: found.sequence_high_water,
        };
        self.slots[index].storage = TicketStorage::Empty(empty);
        ManuallyDrop::into_inner(release.rundown)
    }

    /// # Safety
    ///
    /// Old-epoch submission admission is closed and producer/effect rundown is
    /// complete, so no `DispatchPermit` or runner can remain live. The exact
    /// row lifecycle consumed `reset`, and `reset` was minted only after an
    /// exact raw-zero physical-device reset. Any displaced request, outcome,
    /// or completion remains irreversibly quarantined.
    pub unsafe fn reset_pending(
        &mut self,
        row: SlotHandle<K>,
        reset: &TransportReset,
    ) -> Result<ResetRelease<R, K>, TicketRefusal> {
        let row = RowIdentity::from_handle(row);
        let index = self.checked_row(row)?;
        if reset.retired_epoch().domain() != self.epoch.domain() {
            return Err(TicketRefusal::DomainMismatch {
                expected: self.epoch.domain(),
                found: reset.retired_epoch().domain(),
            });
        }
        if reset.retired_epoch() != self.epoch {
            return Err(TicketRefusal::EpochMismatch {
                expected: self.epoch,
                found: reset.retired_epoch(),
            });
        }
        let old = replace(&mut self.slots[index].storage, TicketStorage::Retired);
        let rundown = match old {
            TicketStorage::Reserved(reserved) if reserved.row == row => reserved.rundown,
            TicketStorage::Prepared(prepared) | TicketStorage::MayHaveSubmitted(prepared)
                if prepared.row == row =>
            {
                prepared.rundown
            }
            TicketStorage::LifecyclePending(pending) if pending.row == row => pending.rundown,
            TicketStorage::LifecycleApplying(applying) if applying.row == row => applying.rundown,
            TicketStorage::Reserved(reserved) => {
                let expected = reserved.row.incarnation;
                self.slots[index].storage = TicketStorage::Reserved(reserved);
                return Err(TicketRefusal::RowIncarnationMismatch {
                    expected,
                    found: row.incarnation,
                });
            }
            TicketStorage::Prepared(prepared) => {
                let expected = prepared.row.incarnation;
                self.slots[index].storage = TicketStorage::Prepared(prepared);
                return Err(TicketRefusal::RowIncarnationMismatch {
                    expected,
                    found: row.incarnation,
                });
            }
            TicketStorage::MayHaveSubmitted(prepared) => {
                let expected = prepared.row.incarnation;
                self.slots[index].storage = TicketStorage::MayHaveSubmitted(prepared);
                return Err(TicketRefusal::RowIncarnationMismatch {
                    expected,
                    found: row.incarnation,
                });
            }
            TicketStorage::LifecyclePending(pending) => {
                let expected = pending.row.incarnation;
                self.slots[index].storage = TicketStorage::LifecyclePending(pending);
                return Err(TicketRefusal::RowIncarnationMismatch {
                    expected,
                    found: row.incarnation,
                });
            }
            TicketStorage::LifecycleApplying(applying) => {
                let expected = applying.row.incarnation;
                self.slots[index].storage = TicketStorage::LifecycleApplying(applying);
                return Err(TicketRefusal::RowIncarnationMismatch {
                    expected,
                    found: row.incarnation,
                });
            }
            storage => {
                let found = storage.state();
                self.slots[index].storage = storage;
                return Err(TicketRefusal::NotResettable { found });
            }
        };
        self.slots[index].storage = TicketStorage::ResetPending(row);
        Ok(ResetRelease {
            row,
            rundown,
            kind: PhantomData,
        })
    }

    pub(crate) fn can_reset_pending(&self, row: SlotHandle<K>, reset: &TransportReset) -> bool {
        let row = RowIdentity::from_handle(row);
        let Ok(index) = self.checked_row(row) else {
            return false;
        };
        if reset.retired_epoch() != self.epoch {
            return false;
        }
        match &self.slots[index].storage {
            TicketStorage::Reserved(found) => found.row == row,
            TicketStorage::Prepared(found) | TicketStorage::MayHaveSubmitted(found) => {
                found.row == row
            }
            TicketStorage::LifecyclePending(found) => found.row == row,
            TicketStorage::LifecycleApplying(found) => found.row == row,
            TicketStorage::Empty(_)
            | TicketStorage::ReleasePending(_)
            | TicketStorage::ResetPending(_)
            | TicketStorage::Retired => false,
        }
    }

    pub(crate) fn can_ack_reset(&self, ack: &PendingResetAck<K>) -> bool {
        let Ok(index) = self.checked_row(ack.row) else {
            return false;
        };
        matches!(
            self.slots[index].storage,
            TicketStorage::ResetPending(found) if found == ack.row
        )
    }

    /// Safety: `can_ack_reset` passed under the sole owner borrow and all
    /// reset payload and rundown finalization effects are complete.
    pub(crate) unsafe fn ack_reset_validated(&mut self, ack: PendingResetAck<K>) {
        self.slots[ack.row.index as usize].storage = TicketStorage::Retired;
    }

    pub fn ack_reset(
        &mut self,
        ack: FinalizedResetAck<K>,
    ) -> Result<(), RefusedAction<FinalizedResetAck<K>>> {
        let row = ack.row;
        let index = match self.checked_row(row) {
            Ok(index) => index,
            Err(reason) => {
                return Err(RefusedAction {
                    reason,
                    action: ack,
                })
            }
        };
        match self.slots[index].storage {
            TicketStorage::ResetPending(found) if found == row => {
                self.slots[index].storage = TicketStorage::Retired;
                Ok(())
            }
            TicketStorage::ResetPending(found) => Err(RefusedAction {
                reason: TicketRefusal::RowIncarnationMismatch {
                    expected: found.incarnation,
                    found: row.incarnation,
                },
                action: ack,
            }),
            ref storage => Err(RefusedAction {
                reason: TicketRefusal::WrongState {
                    expected: TicketState::ResetPending,
                    found: storage.state(),
                },
                action: ack,
            }),
        }
    }

    fn classify_definite_not_enqueued(
        identity: TicketIdentity,
        request: PreparedControl,
        error: E,
    ) -> TicketControl<E> {
        match identity.expected {
            ExpectedReply::UnitNoData => {
                TicketControl::Unit(unsafe { request.definite_not_enqueued(error) })
            }
            ExpectedReply::ContextNoData => {
                TicketControl::Context(unsafe { request.nodata_definite_not_enqueued(error) })
            }
            ExpectedReply::MapInfo => TicketControl::Map {
                completion: unsafe { request.definite_not_enqueued(error) },
                map_info: None,
            },
        }
    }

    fn classify_ambiguous(
        identity: TicketIdentity,
        request: PreparedControl,
        reason: AbandonReason,
    ) -> TicketControl<E> {
        match identity.expected {
            ExpectedReply::UnitNoData => TicketControl::Unit(request.ambiguous_exact(reason)),
            ExpectedReply::ContextNoData => {
                TicketControl::Context(request.ambiguous_nodata_exact(reason))
            }
            ExpectedReply::MapInfo => TicketControl::Map {
                completion: request.ambiguous_exact(reason),
                map_info: None,
            },
        }
    }

    fn classify_rejection(
        identity: TicketIdentity,
        request: PreparedControl,
        rejection: HostRejection,
    ) -> TicketControl<E> {
        match identity.expected {
            ExpectedReply::UnitNoData => {
                TicketControl::Unit(unsafe { request.completed_rejection(rejection) })
            }
            ExpectedReply::ContextNoData => {
                TicketControl::Context(unsafe { request.completed_nodata_rejection(rejection) })
            }
            ExpectedReply::MapInfo => TicketControl::Map {
                completion: unsafe { request.completed_rejection(rejection) },
                map_info: None,
            },
        }
    }

    fn normalize(
        identity: TicketIdentity,
        request: PreparedControl,
        outcome: RunnerOutcome<E>,
    ) -> TicketControl<E> {
        match outcome {
            RunnerOutcome::DefiniteNotEnqueued(error) => {
                Self::classify_definite_not_enqueued(identity, request, error)
            }
            RunnerOutcome::Ambiguous(reason) => Self::classify_ambiguous(identity, request, reason),
            RunnerOutcome::HostResponse {
                response_type,
                written_length,
                map_info,
            } => {
                let header = size_of::<VirtioGpuCtrlHdr>();
                let map = size_of::<VirtioGpuRespMapInfo>();
                if let Ok(rejection) = HostRejection::from_response_type(response_type) {
                    return if written_length == header {
                        Self::classify_rejection(identity, request, rejection)
                    } else {
                        Self::classify_ambiguous(
                            identity,
                            request,
                            AbandonReason::MalformedResponse,
                        )
                    };
                }
                match identity.expected {
                    ExpectedReply::UnitNoData
                        if response_type == VIRTIO_GPU_RESP_OK_NODATA
                            && written_length == header =>
                    {
                        TicketControl::Unit(unsafe { request.completed_ok() })
                    }
                    ExpectedReply::ContextNoData
                        if response_type == VIRTIO_GPU_RESP_OK_NODATA
                            && written_length == header =>
                    {
                        TicketControl::Context(unsafe { request.completed_nodata() })
                    }
                    ExpectedReply::MapInfo
                        if response_type == VIRTIO_GPU_RESP_OK_MAP_INFO
                            && written_length == map =>
                    {
                        TicketControl::Map {
                            completion: unsafe { request.completed_ok() },
                            map_info: Some(map_info),
                        }
                    }
                    _ => Self::classify_ambiguous(
                        identity,
                        request,
                        AbandonReason::MalformedResponse,
                    ),
                }
            }
        }
    }

    fn verb_accepts_subject(verb: ControlVerb, subject: ControlSubject) -> bool {
        K::accepts(verb, subject)
    }

    fn checked_row(&self, row: RowIdentity) -> Result<usize, TicketRefusal> {
        if row.table != self.table {
            return Err(TicketRefusal::TableMismatch {
                expected: self.table,
                found: row.table,
            });
        }
        if row.epoch.domain() != self.epoch.domain() {
            return Err(TicketRefusal::DomainMismatch {
                expected: self.epoch.domain(),
                found: row.epoch.domain(),
            });
        }
        if row.epoch != self.epoch {
            return Err(TicketRefusal::EpochMismatch {
                expected: self.epoch,
                found: row.epoch,
            });
        }
        let index = row.index as usize;
        if index >= self.slots.len() {
            return Err(TicketRefusal::IndexOutOfRange {
                index: row.index,
                capacity: self.slots.len() as u32,
            });
        }
        Ok(index)
    }

    fn checked_key(
        &self,
        key: TicketKey,
        expected: TicketState,
    ) -> Result<(usize, TicketIdentity), TicketRefusal> {
        let index = self.checked_row(key.row)?;
        let (found_identity, found_state) = match &self.slots[index].storage {
            TicketStorage::Prepared(prepared) => (Some(prepared.identity()), TicketState::Prepared),
            TicketStorage::MayHaveSubmitted(prepared) => {
                (Some(prepared.identity()), TicketState::MayHaveSubmitted)
            }
            TicketStorage::LifecyclePending(pending) => {
                let state = if pending.poisoned {
                    TicketState::LifecyclePoisoned
                } else {
                    TicketState::LifecyclePending
                };
                (Some(pending.identity()), state)
            }
            storage => (None, storage.state()),
        };
        if found_state != expected {
            return Err(TicketRefusal::WrongState {
                expected,
                found: found_state,
            });
        }
        let Some(found_identity) = found_identity else {
            return Err(TicketRefusal::WrongState {
                expected,
                found: found_state,
            });
        };
        if found_identity.row.incarnation != key.row.incarnation {
            return Err(TicketRefusal::RowIncarnationMismatch {
                expected: found_identity.row.incarnation,
                found: key.row.incarnation,
            });
        }
        if found_identity.sequence != key.sequence {
            return Err(TicketRefusal::ControlSequenceMismatch {
                expected: found_identity.sequence,
                found: key.sequence,
            });
        }
        Ok((index, found_identity))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::context_attachment::{AttachmentFinishEffect, ContextAttachmentLifecycle};
    use crate::context_lifecycle::{
        ContextFinishEffect, ContextLease, LeasedAttachmentReservation, TransportContextLifecycle,
    };
    use crate::control_owner_slots::{
        ContextSlotKind, PairSlotKind, ResourceSlotKind, SlotTableRoot, StableSlot, StableSlots,
        WindowSlotKind,
    };
    use crate::control_ownership::{
        AttachmentReservation, ContextReservation, ResourceFinishEffect, ResourceLifecycle,
        ResourceReservation, TransportAttachment, TransportContext, TransportDomainId,
        TransportResource, TransportWindow, WindowFinishEffect, WindowLifecycle,
    };
    use core::cell::Cell;
    use core::num::NonZeroU64;
    use helios_protocol::virtio_gpu::{
        VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER,
        VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID, VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID,
        VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY, VIRTIO_GPU_RESP_ERR_UNSPEC,
    };
    use std::rc::Rc;

    struct Rundown {
        id: u64,
        drops: Rc<Cell<u32>>,
    }

    impl Rundown {
        fn new(id: u64, drops: &Rc<Cell<u32>>) -> Self {
            Self {
                id,
                drops: Rc::clone(drops),
            }
        }
    }

    impl Drop for Rundown {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    fn epoch(domain: u64, generation: u64) -> TransportEpoch {
        let domain = TransportDomainId::test_from_raw(domain).unwrap();
        TransportEpoch::test_from_raw(domain, generation).unwrap()
    }

    fn table(raw: u64) -> SlotTableRoot {
        unsafe { SlotTableRoot::new(raw).unwrap() }
    }

    fn request(epoch: TransportEpoch, verb: ControlVerb, sequence: u64) -> PreparedControl {
        let resource = TransportResource::from_raw(epoch, 1).unwrap();
        let context = TransportContext::from_raw(epoch, 1).unwrap();
        let subject = match verb {
            ControlVerb::ContextCreate | ControlVerb::ContextDestroy => {
                ControlSubject::Context(context)
            }
            ControlVerb::Attach | ControlVerb::Detach => ControlSubject::Attachment(
                TransportAttachment::from_raw(resource, context, NonZeroU64::MIN),
            ),
            ControlVerb::Map | ControlVerb::Unmap => {
                ControlSubject::Window(TransportWindow::new(resource, 0, 4096).unwrap())
            }
            ControlVerb::Create | ControlVerb::Unref => ControlSubject::Resource(resource),
        };
        PreparedControl::from_parts(verb, subject, NonZeroU64::new(sequence).unwrap())
    }

    fn leased_attachment(attachment: TransportAttachment) -> LeasedAttachmentReservation {
        LeasedAttachmentReservation::test_for_parts(
            AttachmentReservation::test_for_attachment(attachment),
            ContextLease::test_for_attachment(attachment, NonZeroU64::MIN),
        )
    }

    fn prepare<K: TicketRowKind>(
        tickets: &mut ControlTickets<'_, Rundown, u8, K>,
        row: SlotHandle<K>,
        epoch: TransportEpoch,
        verb: ControlVerb,
        sequence: u64,
        drops: &Rc<Cell<u32>>,
    ) -> PreparedTicket<K> {
        let reservation = unsafe { tickets.reserve(row, Rundown::new(sequence, drops)) }.unwrap();
        let reservation = unsafe { reservation.begin_lifecycle() };
        unsafe { tickets.install(reservation, request(epoch, verb, sequence)) }.unwrap()
    }

    fn finish<K: TicketRowKind>(
        tickets: &mut ControlTickets<'_, Rundown, u8, K>,
        action: LifecycleAction<K>,
    ) {
        let (_, release) = unsafe {
            tickets.apply_once(action, |completion| {
                let _completion = completion;
                Ok(())
            })
        }
        .unwrap();
        let (rundown, pending) = release.into_parts();
        let _id = rundown.id;
        drop(rundown);
        let ack = unsafe { pending.assume_released() };
        tickets.ack_release(ack).unwrap();
    }

    fn ack_rundown<K: TicketRowKind>(
        tickets: &mut ControlTickets<'_, Rundown, u8, K>,
        release: RundownRelease<Rundown, K>,
    ) {
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        tickets
            .ack_release(unsafe { pending.assume_released() })
            .unwrap();
    }

    fn run_verb<K: TicketRowKind>(raw: u64, verb: ControlVerb, drops: &Rc<Cell<u32>>) {
        let epoch = epoch(raw, 1);
        let root = table(raw);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows =
            unsafe { StableSlots::<(), K>::new(root.id(), epoch, &mut row_storage).unwrap() };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, K>::new(root.id(), epoch, &mut ticket_storage).unwrap()
        };
        let prepared = prepare(&mut tickets, row, epoch, verb, 1, drops);
        let permit = tickets.begin_dispatch(prepared).unwrap();
        let observed = unsafe {
            permit.run_once(|wire| {
                assert_eq!(wire.verb(), verb);
                assert_eq!(wire.sequence(), 1);
                if verb == ControlVerb::Map {
                    RunnerOutcome::HostResponse {
                        response_type: VIRTIO_GPU_RESP_OK_MAP_INFO,
                        written_length: size_of::<VirtioGpuRespMapInfo>(),
                        map_info: 0x13,
                    }
                } else {
                    RunnerOutcome::HostResponse {
                        response_type: VIRTIO_GPU_RESP_OK_NODATA,
                        written_length: size_of::<VirtioGpuCtrlHdr>(),
                        map_info: 0,
                    }
                }
            })
        };
        let action = tickets.finish_observed(observed).unwrap();
        {
            let pending = tickets.pending_control(&action).unwrap();
            assert_eq!(pending.verb(), verb);
            match (pending.expected(), pending.observation()) {
                (ExpectedReply::MapInfo, ObservationView::MapCompleted(Ok(0x13))) => {}
                (ExpectedReply::ContextNoData, ObservationView::ContextCompleted(Ok(()))) => {}
                (ExpectedReply::UnitNoData, ObservationView::UnitCompleted(Ok(()))) => {}
                other => panic!("unexpected reply mapping: {other:?}"),
            }
        }
        finish(&mut tickets, action);
        assert_eq!(tickets.state_at(0), Some(TicketState::Empty));
    }

    #[test]
    fn every_verb_derives_its_reply_and_map_retains_payload() {
        let drops = Rc::new(Cell::new(0));
        run_verb::<ResourceSlotKind>(101, ControlVerb::Create, &drops);
        run_verb::<ResourceSlotKind>(102, ControlVerb::Unref, &drops);
        run_verb::<ResourceSlotKind>(103, ControlVerb::Attach, &drops);
        run_verb::<ResourceSlotKind>(104, ControlVerb::Detach, &drops);
        run_verb::<PairSlotKind>(105, ControlVerb::Attach, &drops);
        run_verb::<PairSlotKind>(106, ControlVerb::Detach, &drops);
        run_verb::<WindowSlotKind>(107, ControlVerb::Map, &drops);
        run_verb::<WindowSlotKind>(108, ControlVerb::Unmap, &drops);
        run_verb::<ContextSlotKind>(111, ControlVerb::ContextCreate, &drops);
        run_verb::<ContextSlotKind>(112, ControlVerb::ContextDestroy, &drops);
        assert_eq!(drops.get(), 10);
    }

    #[test]
    fn malformed_shapes_are_terminal_and_documented_errors_are_exact() {
        let epoch = epoch(2, 1);
        let root = table(2);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), WindowSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, WindowSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let malformed = [
            (
                VIRTIO_GPU_RESP_OK_MAP_INFO,
                size_of::<VirtioGpuCtrlHdr>() - 1,
            ),
            (VIRTIO_GPU_RESP_OK_NODATA, size_of::<VirtioGpuCtrlHdr>()),
            (VIRTIO_GPU_RESP_OK_MAP_INFO, size_of::<VirtioGpuCtrlHdr>()),
            (
                VIRTIO_GPU_RESP_OK_MAP_INFO,
                size_of::<VirtioGpuRespMapInfo>() - 1,
            ),
            (
                VIRTIO_GPU_RESP_OK_MAP_INFO,
                size_of::<VirtioGpuRespMapInfo>() + 1,
            ),
            (
                VIRTIO_GPU_RESP_ERR_UNSPEC,
                size_of::<VirtioGpuRespMapInfo>(),
            ),
            (0xdead_beef, size_of::<VirtioGpuRespMapInfo>()),
        ];
        for (at, (response_type, written_length)) in malformed.into_iter().enumerate() {
            let sequence = at as u64 + 1;
            let prepared = prepare(&mut tickets, row, epoch, ControlVerb::Map, sequence, &drops);
            let permit = tickets.begin_dispatch(prepared).unwrap();
            let observed = unsafe {
                permit.run_once(|_| RunnerOutcome::HostResponse {
                    response_type,
                    written_length,
                    map_info: 7,
                })
            };
            let action = tickets.finish_observed(observed).unwrap();
            assert!(matches!(
                tickets.pending_control(&action).unwrap().observation(),
                ObservationView::Ambiguous(AbandonReason::MalformedResponse)
            ));
            finish(&mut tickets, action);
        }

        let documented_errors = [
            VIRTIO_GPU_RESP_ERR_UNSPEC,
            VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY,
            VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID,
            VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID,
            VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID,
            VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER,
        ];
        for (at, response_type) in documented_errors.into_iter().enumerate() {
            let prepared = prepare(
                &mut tickets,
                row,
                epoch,
                ControlVerb::Map,
                at as u64 + malformed.len() as u64 + 1,
                &drops,
            );
            let permit = tickets.begin_dispatch(prepared).unwrap();
            let observed = unsafe {
                permit.run_once(|_| RunnerOutcome::HostResponse {
                    response_type,
                    written_length: size_of::<VirtioGpuCtrlHdr>(),
                    map_info: 0,
                })
            };
            let action = tickets.finish_observed(observed).unwrap();
            let expected = HostRejection::from_response_type(response_type).unwrap();
            assert!(matches!(
                tickets.pending_control(&action).unwrap().observation(),
                ObservationView::MapCompleted(Err(found)) if found == expected
            ));
            finish(&mut tickets, action);
        }
        assert_eq!(
            drops.get(),
            (malformed.len() + documented_errors.len()) as u32
        );
    }

    #[test]
    fn prepared_and_post_arm_dne_both_finish_without_a_retry_edge() {
        let epoch = epoch(3, 1);
        let root = table(3);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));

        let prepared = prepare(&mut tickets, row, epoch, ControlVerb::Create, 1, &drops);
        let action = tickets.cancel_prepared(prepared, 7).unwrap();
        assert!(matches!(
            tickets.pending_control(&action).unwrap().observation(),
            ObservationView::DefiniteNotEnqueued(7)
        ));
        finish(&mut tickets, action);

        let prepared = prepare(&mut tickets, row, epoch, ControlVerb::Create, 2, &drops);
        let permit = tickets.begin_dispatch(prepared).unwrap();
        let observed = unsafe { permit.run_once(|_| RunnerOutcome::DefiniteNotEnqueued(8)) };
        let action = tickets.finish_observed(observed).unwrap();
        assert!(matches!(
            tickets.pending_control(&action).unwrap().observation(),
            ObservationView::DefiniteNotEnqueued(8)
        ));
        assert!(matches!(
            tickets.begin_dispatch(PreparedTicket {
                key: action.key,
                kind: PhantomData,
            }),
            Err(RefusedAction {
                reason: TicketRefusal::WrongState {
                    found: TicketState::LifecyclePending,
                    ..
                },
                ..
            })
        ));
        finish(&mut tickets, action);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn install_rejects_foreign_row_kind_and_subject_epoch_without_spending_custody() {
        let drops = Rc::new(Cell::new(0));

        {
            let epoch = epoch(109, 1);
            let root = table(109);
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage)
                    .unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                    root.id(),
                    epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let resource = TransportResource::from_raw(epoch, 1).unwrap();
            let window = TransportWindow::new(resource, 0, 4096).unwrap();
            let reservation = unsafe { tickets.reserve(row, Rundown::new(1, &drops)) }.unwrap();
            let reservation = unsafe { reservation.begin_lifecycle() };
            let admission = unsafe { WindowLifecycle::reserve(window, ()) };
            let (mut lifecycle, request) = admission.into_parts();
            let refusal = match unsafe { tickets.install(reservation, request) } {
                Err(refusal) => refusal,
                Ok(_) => panic!("window request entered a resource ticket row"),
            };
            assert_eq!(refusal.reason(), TicketRefusal::ControlSubjectMismatch);
            let (post_begin, request) = refusal.into_parts();
            let finished = lifecycle
                .finish_map(unsafe { request.definite_not_enqueued::<u8>(1) })
                .unwrap();
            let (_, terminal) = finished.into_parts();
            lifecycle.consume_unmapped(terminal.unwrap()).unwrap();
            let reservation = unsafe { post_begin.assume_lifecycle_reconciled() };
            let release = tickets.cancel_reservation(reservation).unwrap();
            ack_rundown(&mut tickets, release);
        }

        {
            let table_epoch = epoch(110, 1);
            let request_epoch = epoch(110, 2);
            let root = table(110);
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), ResourceSlotKind>::new(root.id(), table_epoch, &mut row_storage)
                    .unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                    root.id(),
                    table_epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let resource = TransportResource::from_raw(request_epoch, 1).unwrap();
            let reservation = unsafe { tickets.reserve(row, Rundown::new(2, &drops)) }.unwrap();
            let reservation = unsafe { reservation.begin_lifecycle() };
            let mut lifecycle =
                ResourceLifecycle::new(ResourceReservation::test_for_resource(resource), ());
            let request = lifecycle.begin_create(resource).unwrap().into_request();
            let refusal = match unsafe { tickets.install(reservation, request) } {
                Err(refusal) => refusal,
                Ok(_) => panic!("foreign-epoch request entered the ticket row"),
            };
            assert_eq!(
                refusal.reason(),
                TicketRefusal::SubjectEpochMismatch {
                    expected: table_epoch,
                    found: request_epoch,
                }
            );
            let (post_begin, request) = refusal.into_parts();
            let finished = lifecycle
                .finish_create(unsafe { request.definite_not_enqueued::<u8>(2) })
                .unwrap();
            let (_, terminal, _) = finished.into_parts();
            lifecycle.consume_terminal(terminal.unwrap()).unwrap();
            let reservation = unsafe { post_begin.assume_lifecycle_reconciled() };
            let release = tickets.cancel_reservation(reservation).unwrap();
            ack_rundown(&mut tickets, release);
        }

        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn lifecycle_refusal_restores_exact_table_owned_completion() {
        let epoch = epoch(11, 1);
        let root = table(11);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let resource = TransportResource::from_raw(epoch, 1).unwrap();
        let reservation = unsafe { tickets.reserve(row, Rundown::new(1, &drops)) }.unwrap();
        let reservation = unsafe { reservation.begin_lifecycle() };
        let mut lifecycle =
            ResourceLifecycle::new(ResourceReservation::test_for_resource(resource), ());
        let mut wrong_lifecycle =
            ResourceLifecycle::new(ResourceReservation::test_for_resource(resource), ());
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        let prepared = unsafe { tickets.install(reservation, request) }.unwrap();
        let action = tickets.cancel_prepared(prepared, 23).unwrap();
        let refusal = match unsafe {
            tickets.apply_once(action, |completion| {
                completion.apply_unit(|completion| {
                    wrong_lifecycle
                        .finish_create(completion)
                        .map(|_| ())
                        .map_err(|refused| refused.into_completion())
                })
            })
        } {
            Err(refusal) => refusal,
            Ok(_) => panic!("refused lifecycle was acknowledged"),
        };
        assert!(matches!(&refusal, ApplyRefusal::LifecycleRefused { .. }));
        let action = refusal.into_lifecycle_action().unwrap();
        assert!(matches!(
            tickets.pending_control(&action).unwrap().observation(),
            ObservationView::DefiniteNotEnqueued(23)
        ));
        let (finished, release) = unsafe {
            tickets.apply_once(action, |completion| {
                completion.apply_unit(|completion| {
                    lifecycle
                        .finish_create(completion)
                        .map_err(|refused| refused.into_completion())
                })
            })
        }
        .unwrap();
        assert!(matches!(
            finished.effect(),
            ResourceFinishEffect::CreateDefiniteNotEnqueued(23)
        ));
        let (_, destroy, _) = finished.into_parts();
        lifecycle.consume_terminal(destroy.unwrap()).unwrap();
        ack_rundown(&mut tickets, release);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn typed_ticket_completions_compose_with_context_pair_and_window_lifecycles() {
        let drops = Rc::new(Cell::new(0));

        {
            let epoch = epoch(20, 1);
            let root = table(20);
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage)
                    .unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                    root.id(),
                    epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let resource = TransportResource::from_raw(epoch, 1).unwrap();
            let context = TransportContext::from_raw(epoch, 1).unwrap();
            let attachment = TransportAttachment::from_raw(resource, context, NonZeroU64::MIN);
            let mut lifecycle =
                ResourceLifecycle::new(ResourceReservation::test_for_resource(resource), ());
            let create = lifecycle.begin_create(resource).unwrap().into_request();
            let created = lifecycle
                .finish_create(unsafe { create.completed_ok::<u8>() })
                .unwrap();
            assert!(matches!(
                created.effect(),
                ResourceFinishEffect::CreateCompleted
            ));
            let reservation = unsafe { tickets.reserve(row, Rundown::new(20, &drops)) }.unwrap();
            let reservation = unsafe { reservation.begin_lifecycle() };
            let request = lifecycle
                .begin_attach(leased_attachment(attachment))
                .unwrap()
                .into_request();
            let prepared = unsafe { tickets.install(reservation, request) }.unwrap();
            let action = tickets.cancel_prepared(prepared, 20).unwrap();
            let (finished, release) = unsafe {
                tickets.apply_once(action, |completion| {
                    completion.apply_unit(|completion| {
                        lifecycle
                            .finish_attach(completion)
                            .map_err(|refused| refused.into_completion())
                    })
                })
            }
            .unwrap();
            assert!(matches!(
                finished.effect(),
                ResourceFinishEffect::AttachDefiniteNotEnqueued(20)
            ));
            let (_, _, attachment_release) = finished.into_parts();
            assert!(attachment_release.is_some());
            ack_rundown(&mut tickets, release);
        }

        {
            let epoch = epoch(21, 1);
            let root = table(21);
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), ContextSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, ContextSlotKind>::new(
                    root.id(),
                    epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let context = TransportContext::from_raw(epoch, 1).unwrap();
            let reservation = unsafe { tickets.reserve(row, Rundown::new(21, &drops)) }.unwrap();
            let reservation = unsafe { reservation.begin_lifecycle() };
            let mut lifecycle =
                TransportContextLifecycle::new(ContextReservation::test_for_context(context), ());
            let request = lifecycle.begin_create(context).unwrap().into_request();
            let prepared = unsafe { tickets.install(reservation, request) }.unwrap();
            let action = tickets.cancel_prepared(prepared, 21).unwrap();
            let wrong_context = TransportContext::from_raw(epoch, 2).unwrap();
            let mut wrong_lifecycle = TransportContextLifecycle::new(
                ContextReservation::test_for_context(wrong_context),
                (),
            );
            let refusal = match unsafe {
                tickets.apply_once(action, |completion| {
                    completion.apply_context(|completion| {
                        wrong_lifecycle
                            .finish_create(completion)
                            .map_err(|refused| refused.into_completion())
                    })
                })
            } {
                Err(refusal) => refusal,
                Ok(_) => panic!("foreign context lifecycle accepted completion"),
            };
            let action = refusal.into_lifecycle_action().unwrap();
            assert!(matches!(
                tickets.pending_control(&action).unwrap().observation(),
                ObservationView::DefiniteNotEnqueued(21)
            ));
            let wrong_terminal = wrong_lifecycle.cancel_reservation(wrong_context).unwrap();
            let _wrong_released = wrong_lifecycle.consume_terminal(wrong_terminal).unwrap();
            let (finished, release) = unsafe {
                tickets.apply_once(action, |completion| {
                    completion.apply_context(|completion| {
                        lifecycle
                            .finish_create(completion)
                            .map_err(|refused| refused.into_completion())
                    })
                })
            }
            .unwrap();
            assert!(matches!(
                finished.effect(),
                ContextFinishEffect::CreateDefiniteNotEnqueued(21)
            ));
            let (_, terminal) = finished.into_parts();
            let _released = lifecycle.consume_terminal(terminal.unwrap()).unwrap();
            ack_rundown(&mut tickets, release);
        }

        {
            let epoch = epoch(22, 1);
            let root = table(22);
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), PairSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, PairSlotKind>::new(
                    root.id(),
                    epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let resource = TransportResource::from_raw(epoch, 1).unwrap();
            let context = TransportContext::from_raw(epoch, 1).unwrap();
            let attachment = TransportAttachment::from_raw(resource, context, NonZeroU64::MIN);
            let reservation = unsafe { tickets.reserve(row, Rundown::new(22, &drops)) }.unwrap();
            let reservation = unsafe { reservation.begin_lifecycle() };
            let admission =
                ContextAttachmentLifecycle::reserve(leased_attachment(attachment), ()).unwrap();
            let (mut lifecycle, request) = admission.into_parts();
            let prepared = unsafe { tickets.install(reservation, request) }.unwrap();
            let action = tickets.cancel_prepared(prepared, 22).unwrap();
            let (finished, release) = unsafe {
                tickets.apply_once(action, |completion| {
                    completion.apply_unit(|completion| {
                        lifecycle
                            .finish_attach(completion)
                            .map_err(|refused| refused.into_completion())
                    })
                })
            }
            .unwrap();
            assert!(matches!(
                finished.effect(),
                AttachmentFinishEffect::AttachDefiniteNotEnqueued(22)
            ));
            let (_, terminal) = finished.into_parts();
            let _released = lifecycle.consume_released(terminal.unwrap()).unwrap();
            ack_rundown(&mut tickets, release);
        }

        {
            let epoch = epoch(23, 1);
            let root = table(23);
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), WindowSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, WindowSlotKind>::new(
                    root.id(),
                    epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let resource = TransportResource::from_raw(epoch, 1).unwrap();
            let window = TransportWindow::new(resource, 0, 4096).unwrap();
            let reservation = unsafe { tickets.reserve(row, Rundown::new(23, &drops)) }.unwrap();
            let reservation = unsafe { reservation.begin_lifecycle() };
            let admission = unsafe { WindowLifecycle::reserve(window, ()) };
            let (mut lifecycle, request) = admission.into_parts();
            let prepared = unsafe { tickets.install(reservation, request) }.unwrap();
            let permit = tickets.begin_dispatch(prepared).unwrap();
            let observed = unsafe {
                permit.run_once(|_| RunnerOutcome::HostResponse {
                    response_type: VIRTIO_GPU_RESP_OK_MAP_INFO,
                    written_length: size_of::<VirtioGpuRespMapInfo>(),
                    map_info: 0x27,
                })
            };
            let action = tickets.finish_observed(observed).unwrap();
            let wrong_window = TransportWindow::new(resource, 4096, 4096).unwrap();
            let wrong_admission = unsafe { WindowLifecycle::reserve(wrong_window, ()) };
            let (mut wrong_lifecycle, _wrong_request) = wrong_admission.into_parts();
            let refusal = match unsafe {
                tickets.apply_once(action, |completion| {
                    completion.apply_map(|completion| {
                        wrong_lifecycle
                            .finish_map(completion)
                            .map_err(|refused| refused.into_completion())
                    })
                })
            } {
                Err(refusal) => refusal,
                Ok(_) => panic!("foreign window lifecycle accepted completion"),
            };
            let action = refusal.into_lifecycle_action().unwrap();
            assert!(matches!(
                tickets.pending_control(&action).unwrap().observation(),
                ObservationView::MapCompleted(Ok(0x27))
            ));
            let wrong_terminal = wrong_lifecycle
                .transport_reset(wrong_window, &TransportReset::test_for_epoch(epoch))
                .unwrap();
            wrong_lifecycle.consume_unmapped(wrong_terminal).unwrap();
            let (result, release) = unsafe {
                tickets.apply_once(action, |completion| {
                    completion.apply_map(|completion| {
                        lifecycle
                            .finish_map(completion)
                            .map_err(|refused| refused.into_completion())
                    })
                })
            }
            .unwrap();
            assert!(matches!(
                (result.0).effect(),
                WindowFinishEffect::MapCompleted
            ));
            assert_eq!(result.1, Some(0x27));
            let terminal = lifecycle
                .transport_reset(window, &TransportReset::test_for_epoch(epoch))
                .unwrap();
            lifecycle.consume_unmapped(terminal).unwrap();
            ack_rundown(&mut tickets, release);
        }

        assert_eq!(drops.get(), 4);
    }

    #[test]
    fn completed_out_of_lock_observation_is_revoked_by_reset() {
        let epoch = epoch(4, 1);
        let root = table(4);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let prepared = prepare(&mut tickets, row, epoch, ControlVerb::Create, 1, &drops);
        let permit = tickets.begin_dispatch(prepared).unwrap();
        let called = Cell::new(false);
        let observed = unsafe {
            permit.run_once(|_| {
                called.set(true);
                RunnerOutcome::Ambiguous(AbandonReason::Timeout)
            })
        };
        assert!(called.get());
        let reset = TransportReset::test_for_epoch(epoch);
        let release = unsafe { tickets.reset_pending(row, &reset) }.unwrap();
        let refusal = match tickets.finish_observed(observed) {
            Err(refusal) => refusal,
            Ok(_) => panic!("reset-revoked observation reached the lifecycle"),
        };
        assert!(matches!(
            refusal.reason(),
            TicketRefusal::WrongState {
                found: TicketState::ResetPending,
                ..
            }
        ));
        drop(refusal.into_action());
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        let ack = unsafe { pending.assume_released() };
        tickets.ack_reset(ack).unwrap();
        assert_eq!(tickets.state_at(0), Some(TicketState::Retired));
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn reset_revokes_an_outstanding_lifecycle_action_and_normal_ack() {
        let epoch = epoch(5, 1);
        let root = table(5);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let prepared = prepare(&mut tickets, row, epoch, ControlVerb::Create, 1, &drops);
        let action = tickets.cancel_prepared(prepared, 1).unwrap();
        let release =
            unsafe { tickets.reset_pending(row, &TransportReset::test_for_epoch(epoch)) }.unwrap();
        assert!(matches!(
            tickets.pending_control(&action),
            Err(TicketRefusal::WrongState {
                found: TicketState::ResetPending,
                ..
            })
        ));
        let refusal = match unsafe {
            tickets.apply_once(action, |completion| {
                let _completion = completion;
                Ok(())
            })
        } {
            Err(refusal) => refusal,
            Ok(_) => panic!("reset-revoked lifecycle action finished"),
        };
        assert!(matches!(
            refusal.reason(),
            Some(TicketRefusal::WrongState {
                found: TicketState::ResetPending,
                ..
            })
        ));
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        tickets
            .ack_reset(unsafe { pending.assume_released() })
            .unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn sequence_replay_is_refused_but_a_new_row_incarnation_restarts_at_one() {
        let epoch = epoch(6, 1);
        let root = table(6);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let first_row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let first = prepare(
            &mut tickets,
            first_row,
            epoch,
            ControlVerb::Create,
            1,
            &drops,
        );
        let action = tickets.cancel_prepared(first, 1).unwrap();
        finish(&mut tickets, action);

        let reservation = unsafe { tickets.reserve(first_row, Rundown::new(2, &drops)) }.unwrap();
        let reservation = unsafe { reservation.begin_lifecycle() };
        let refusal =
            match unsafe { tickets.install(reservation, request(epoch, ControlVerb::Create, 1)) } {
                Err(refusal) => refusal,
                Ok(_) => panic!("sequence replay admitted"),
            };
        assert_eq!(
            refusal.reason(),
            TicketRefusal::SequenceReusedOrWentBackward {
                high_water: 1,
                found: 1,
            }
        );
        let (reservation, _request) = refusal.into_parts();
        let reservation = unsafe { reservation.assume_lifecycle_reconciled() };
        let release = tickets.cancel_reservation(reservation).unwrap();
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        tickets
            .ack_release(unsafe { pending.assume_released() })
            .unwrap();

        let tombstone = rows
            .mark_tombstone(unsafe { first_row.assume_terminal() })
            .unwrap();
        let (payload, pending) = rows.extract(tombstone).unwrap().into_parts();
        let _ = payload;
        rows.ack_finalized(unsafe { pending.assume_payload_finalized() })
            .unwrap();
        let second_row = rows.insert(()).unwrap();
        assert_eq!(second_row.incarnation(), first_row.incarnation() + 1);
        let second = prepare(
            &mut tickets,
            second_row,
            epoch,
            ControlVerb::Create,
            1,
            &drops,
        );
        let action = tickets.cancel_prepared(second, 2).unwrap();
        finish(&mut tickets, action);
        assert_eq!(drops.get(), 3);
    }

    #[test]
    fn post_begin_install_refusal_requires_lifecycle_reconciliation() {
        let epoch = epoch(24, 1);
        let root = table(24);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));

        let prepared = prepare(&mut tickets, row, epoch, ControlVerb::Create, 1, &drops);
        let action = tickets.cancel_prepared(prepared, 1).unwrap();
        finish(&mut tickets, action);

        let resource = TransportResource::from_raw(epoch, 1).unwrap();
        let reservation = unsafe { tickets.reserve(row, Rundown::new(2, &drops)) }.unwrap();
        let reservation = unsafe { reservation.begin_lifecycle() };
        let mut lifecycle =
            ResourceLifecycle::new(ResourceReservation::test_for_resource(resource), ());
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        let refusal = match unsafe { tickets.install(reservation, request) } {
            Err(refusal) => refusal,
            Ok(_) => panic!("replayed sequence was installed"),
        };
        assert_eq!(
            refusal.reason(),
            TicketRefusal::SequenceReusedOrWentBackward {
                high_water: 1,
                found: 1,
            }
        );
        let (post_begin, request) = refusal.into_parts();
        let finished = lifecycle
            .finish_create(unsafe { request.definite_not_enqueued::<u8>(2) })
            .unwrap();
        let (_, destroy, _) = finished.into_parts();
        lifecycle.consume_terminal(destroy.unwrap()).unwrap();
        let reservation = unsafe { post_begin.assume_lifecycle_reconciled() };
        let release = tickets.cancel_reservation(reservation).unwrap();
        ack_rundown(&mut tickets, release);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn begin_refusal_is_the_only_direct_path_back_to_prebegin_custody() {
        let epoch = epoch(121, 1);
        let root = table(121);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let resource = TransportResource::from_raw(epoch, 1).unwrap();
        let foreign = TransportResource::from_raw(epoch, 2).unwrap();
        let mut lifecycle =
            ResourceLifecycle::new(ResourceReservation::test_for_resource(resource), ());

        let reservation = unsafe { tickets.reserve(row, Rundown::new(1, &drops)) }.unwrap();
        let post_begin = unsafe { reservation.begin_lifecycle() };
        assert!(lifecycle.begin_create(foreign).is_err());
        let reservation = unsafe { post_begin.assume_lifecycle_not_begun() };
        let release = tickets.cancel_reservation(reservation).unwrap();
        ack_rundown(&mut tickets, release);
        assert_eq!(tickets.state_at(0), Some(TicketState::Empty));
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn reset_retires_reserved_and_post_begin_custody_without_reopening_cancel() {
        let epoch = epoch(122, 1);
        let root = table(122);
        let mut row_storage = [StableSlot::vacant(), StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let prebegin_row = rows.insert(()).unwrap();
        let post_begin_row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty(), ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let first_resource = TransportResource::from_raw(epoch, 1).unwrap();
        let second_resource = TransportResource::from_raw(epoch, 2).unwrap();
        let mut first_lifecycle =
            ResourceLifecycle::new(ResourceReservation::test_for_resource(first_resource), ());
        let mut second_lifecycle =
            ResourceLifecycle::new(ResourceReservation::test_for_resource(second_resource), ());
        let prebegin = unsafe { tickets.reserve(prebegin_row, Rundown::new(1, &drops)) }.unwrap();
        let post_begin =
            unsafe { tickets.reserve(post_begin_row, Rundown::new(2, &drops)) }.unwrap();
        let post_begin = unsafe { post_begin.begin_lifecycle() };
        let reset = TransportReset::test_for_epoch(epoch);

        let first_terminal = first_lifecycle
            .transport_reset(first_resource, &reset)
            .unwrap();
        let first_release = unsafe { tickets.reset_pending(prebegin_row, &reset) }.unwrap();
        let refusal = match tickets.cancel_reservation(prebegin) {
            Err(refusal) => refusal,
            Ok(_) => panic!("reset-revoked pre-begin custody was cancelled"),
        };
        assert_eq!(
            refusal.reason(),
            TicketRefusal::WrongState {
                expected: TicketState::Reserved,
                found: TicketState::ResetPending,
            }
        );
        drop(refusal.into_action());

        let second_terminal = second_lifecycle
            .transport_reset(second_resource, &reset)
            .unwrap();
        let second_release = unsafe { tickets.reset_pending(post_begin_row, &reset) }.unwrap();
        drop(post_begin);

        for release in [first_release, second_release] {
            let (rundown, pending) = release.into_parts();
            drop(rundown);
            tickets
                .ack_reset(unsafe { pending.assume_released() })
                .unwrap();
        }
        let (first_destroy, _) = first_terminal.into_parts();
        first_lifecycle.consume_terminal(first_destroy).unwrap();
        let (second_destroy, _) = second_terminal.into_parts();
        second_lifecycle.consume_terminal(second_destroy).unwrap();
        assert_eq!(tickets.state_at(0), Some(TicketState::Retired));
        assert_eq!(tickets.state_at(1), Some(TicketState::Retired));
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn identical_controls_cannot_redirect_observation_or_action_replay_across_rows() {
        let epoch = epoch(123, 1);
        let root = table(123);
        let mut row_storage = [StableSlot::vacant(), StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let first_row = rows.insert(()).unwrap();
        let second_row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty(), ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let first = prepare(
            &mut tickets,
            first_row,
            epoch,
            ControlVerb::Create,
            1,
            &drops,
        );
        let second = prepare(
            &mut tickets,
            second_row,
            epoch,
            ControlVerb::Create,
            1,
            &drops,
        );
        let first = tickets.begin_dispatch(first).unwrap();
        let second = tickets.begin_dispatch(second).unwrap();
        let first_observed = unsafe {
            first.run_once(|_| RunnerOutcome::HostResponse {
                response_type: VIRTIO_GPU_RESP_OK_NODATA,
                written_length: size_of::<VirtioGpuCtrlHdr>(),
                map_info: 0,
            })
        };
        let replayed_observation = ObservedDispatch {
            key: first_observed.key,
            outcome: ManuallyDrop::new(RunnerOutcome::<u8>::HostResponse {
                response_type: VIRTIO_GPU_RESP_OK_NODATA,
                written_length: size_of::<VirtioGpuCtrlHdr>(),
                map_info: 0,
            }),
            kind: PhantomData,
        };
        let second_observed = unsafe {
            second.run_once(|_| RunnerOutcome::HostResponse {
                response_type: VIRTIO_GPU_RESP_OK_NODATA,
                written_length: size_of::<VirtioGpuCtrlHdr>(),
                map_info: 0,
            })
        };

        let first_action = tickets.finish_observed(first_observed).unwrap();
        let replayed_action = LifecycleAction {
            key: first_action.key,
            kind: PhantomData,
        };
        let refusal = match tickets.finish_observed(replayed_observation) {
            Err(refusal) => refusal,
            Ok(_) => panic!("replayed observation reached another row"),
        };
        assert!(matches!(
            refusal.reason(),
            TicketRefusal::WrongState {
                found: TicketState::LifecyclePending,
                ..
            }
        ));
        drop(refusal.into_action());
        assert_eq!(tickets.state_at(1), Some(TicketState::MayHaveSubmitted));

        let second_action = tickets.finish_observed(second_observed).unwrap();
        finish(&mut tickets, first_action);
        assert!(matches!(
            tickets.pending_control(&replayed_action),
            Err(TicketRefusal::WrongState {
                found: TicketState::Empty,
                ..
            })
        ));
        assert_eq!(tickets.state_at(1), Some(TicketState::LifecyclePending));
        drop(replayed_action);
        finish(&mut tickets, second_action);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn reset_is_domain_exact_and_recovers_a_quarantined_apply_mismatch() {
        let current_epoch = epoch(25, 1);
        let root = table(25);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), current_epoch, &mut row_storage)
                .unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                current_epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let resource = TransportResource::from_raw(current_epoch, 1).unwrap();
        let reservation = unsafe { tickets.reserve(row, Rundown::new(1, &drops)) }.unwrap();
        let reservation = unsafe { reservation.begin_lifecycle() };
        let mut lifecycle =
            ResourceLifecycle::new(ResourceReservation::test_for_resource(resource), ());
        let request = lifecycle.begin_create(resource).unwrap().into_request();
        let prepared = unsafe { tickets.install(reservation, request) }.unwrap();
        let action = tickets.cancel_prepared(prepared, 1).unwrap();
        let refusal = match unsafe {
            tickets.apply_once(action, |completion| {
                let TicketCompletion {
                    origin, control, ..
                } = completion;
                match control {
                    TicketControl::Unit(completion) => Err::<(), _>(TicketCompletion {
                        origin,
                        control: TicketControl::Map {
                            completion,
                            map_info: None,
                        },
                        kind: PhantomData,
                    }),
                    control => Err(TicketCompletion {
                        origin,
                        control,
                        kind: PhantomData,
                    }),
                }
            })
        } {
            Err(refusal) => refusal,
            Ok(_) => panic!("mismatched completion was accepted"),
        };
        assert!(matches!(&refusal, ApplyRefusal::CompletionMismatch));
        assert_eq!(tickets.state_at(0), Some(TicketState::LifecyclePoisoned));

        let foreign = TransportReset::test_for_epoch(epoch(26, 1));
        assert!(matches!(
            unsafe { tickets.reset_pending(row, &foreign) },
            Err(TicketRefusal::DomainMismatch { .. })
        ));
        assert_eq!(tickets.state_at(0), Some(TicketState::LifecyclePoisoned));

        let reset = TransportReset::test_for_epoch(current_epoch);
        let resource_reset = lifecycle.transport_reset(resource, &reset).unwrap();
        let release = unsafe { tickets.reset_pending(row, &reset) }.unwrap();
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        let replay = FinalizedResetAck {
            row: pending.row,
            kind: PhantomData,
        };
        tickets
            .ack_reset(unsafe { pending.assume_released() })
            .unwrap();
        let replay = tickets.ack_reset(replay).unwrap_err();
        assert_eq!(
            replay.reason(),
            TicketRefusal::WrongState {
                expected: TicketState::ResetPending,
                found: TicketState::Retired,
            }
        );
        drop(replay.into_action());
        let (destroy, _) = resource_reset.into_parts();
        lifecycle.consume_terminal(destroy).unwrap();
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn maximum_sequence_is_spent_and_never_wraps_or_reopens() {
        let epoch = epoch(10, 1);
        let root = table(10);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let prepared = prepare(
            &mut tickets,
            row,
            epoch,
            ControlVerb::Create,
            u64::MAX,
            &drops,
        );
        let action = tickets.cancel_prepared(prepared, 1).unwrap();
        finish(&mut tickets, action);

        let reservation = unsafe { tickets.reserve(row, Rundown::new(2, &drops)) }.unwrap();
        let reservation = unsafe { reservation.begin_lifecycle() };
        let refusal = match unsafe {
            tickets.install(reservation, request(epoch, ControlVerb::Create, u64::MAX))
        } {
            Err(refusal) => refusal,
            Ok(_) => panic!("maximum control sequence reopened"),
        };
        assert_eq!(refusal.reason(), TicketRefusal::SequenceExhausted);
        let (reservation, _request) = refusal.into_parts();
        let reservation = unsafe { reservation.assume_lifecycle_reconciled() };
        let release = tickets.cancel_reservation(reservation).unwrap();
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        tickets
            .ack_release(unsafe { pending.assume_released() })
            .unwrap();
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn dropping_storage_refusals_and_release_actions_quarantines_rundown() {
        let epoch = epoch(7, 1);
        let root = table(7);
        let drops = Rc::new(Cell::new(0));
        {
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage)
                    .unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                    root.id(),
                    epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let reservation = unsafe { tickets.reserve(row, Rundown::new(1, &drops)) }.unwrap();
            let refusal = match unsafe { tickets.reserve(row, Rundown::new(2, &drops)) } {
                Err(refusal) => refusal,
                Ok(_) => panic!("second reservation admitted"),
            };
            drop(refusal);
            assert_eq!(drops.get(), 0);
            drop(tickets.cancel_reservation(reservation).unwrap());
            assert_eq!(tickets.state_at(0), Some(TicketState::ReleasePending));
        }
        assert_eq!(drops.get(), 0);
    }

    #[test]
    fn dropping_each_external_ticket_stage_strands_table_custody() {
        let drops = Rc::new(Cell::new(0));
        for (at, raw) in [113_u64, 114, 115, 116].into_iter().enumerate() {
            let epoch = epoch(raw, 1);
            let root = table(raw);
            let mut row_storage = [StableSlot::vacant()];
            let mut rows = unsafe {
                StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage)
                    .unwrap()
            };
            let row = rows.insert(()).unwrap();
            let mut ticket_storage = [ControlTicketSlot::empty()];
            let mut tickets = unsafe {
                ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                    root.id(),
                    epoch,
                    &mut ticket_storage,
                )
                .unwrap()
            };
            let prepared = prepare(&mut tickets, row, epoch, ControlVerb::Create, 1, &drops);
            match at {
                0 => {
                    drop(prepared);
                    assert_eq!(tickets.state_at(0), Some(TicketState::Prepared));
                }
                1 => {
                    drop(tickets.begin_dispatch(prepared).unwrap());
                    assert_eq!(tickets.state_at(0), Some(TicketState::MayHaveSubmitted));
                }
                2 => {
                    let permit = tickets.begin_dispatch(prepared).unwrap();
                    let observed = unsafe {
                        permit.run_once(|_| RunnerOutcome::<u8>::Ambiguous(AbandonReason::Timeout))
                    };
                    drop(observed);
                    assert_eq!(tickets.state_at(0), Some(TicketState::MayHaveSubmitted));
                }
                3 => {
                    drop(tickets.cancel_prepared(prepared, 1).unwrap());
                    assert_eq!(tickets.state_at(0), Some(TicketState::LifecyclePending));
                }
                _ => unreachable!(),
            }
            assert_eq!(drops.get(), 0);
        }
        assert_eq!(drops.get(), 0);
    }

    #[test]
    fn finalized_release_ack_replay_never_reopens_or_clears_a_successor() {
        let epoch = epoch(117, 1);
        let root = table(117);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root.id(), epoch, &mut row_storage).unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root.id(),
                epoch,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));

        let first = prepare(&mut tickets, row, epoch, ControlVerb::Create, 1, &drops);
        let first = tickets.cancel_prepared(first, 1).unwrap();
        let (_, release) = unsafe { tickets.apply_once(first, |_| Ok(())) }.unwrap();
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        let replay = FinalizedReleaseAck {
            release: pending.release,
            kind: PhantomData,
        };
        tickets
            .ack_release(unsafe { pending.assume_released() })
            .unwrap();
        let replay = tickets.ack_release(replay).unwrap_err().into_action();
        assert_eq!(tickets.state_at(0), Some(TicketState::Empty));

        let second = prepare(&mut tickets, row, epoch, ControlVerb::Create, 2, &drops);
        let second = tickets.cancel_prepared(second, 2).unwrap();
        let (_, release) = unsafe { tickets.apply_once(second, |_| Ok(())) }.unwrap();
        let (rundown, pending) = release.into_parts();
        drop(rundown);
        let refusal = tickets.ack_release(replay).unwrap_err();
        assert_eq!(
            refusal.reason(),
            TicketRefusal::ControlSequenceMismatch {
                expected: 2,
                found: 1,
            }
        );
        drop(refusal.into_action());
        assert_eq!(tickets.state_at(0), Some(TicketState::ReleasePending));
        tickets
            .ack_release(unsafe { pending.assume_released() })
            .unwrap();
        assert_eq!(tickets.state_at(0), Some(TicketState::Empty));
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn foreign_table_and_epoch_refusals_return_rundown_without_mutation() {
        let epoch_a = epoch(8, 1);
        let epoch_b = epoch(9, 1);
        let root_a = table(8);
        let root_b = table(9);
        let mut row_storage = [StableSlot::vacant()];
        let mut rows = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root_a.id(), epoch_a, &mut row_storage)
                .unwrap()
        };
        let row = rows.insert(()).unwrap();
        let mut row_b_storage = [StableSlot::vacant()];
        let mut rows_b = unsafe {
            StableSlots::<(), ResourceSlotKind>::new(root_b.id(), epoch_b, &mut row_b_storage)
                .unwrap()
        };
        let _row_b = rows_b.insert(()).unwrap();
        let mut ticket_storage = [ControlTicketSlot::<Rundown, u8, ResourceSlotKind>::empty()];
        let mut tickets = unsafe {
            ControlTickets::<Rundown, u8, ResourceSlotKind>::new(
                root_b.id(),
                epoch_b,
                &mut ticket_storage,
            )
            .unwrap()
        };
        let drops = Rc::new(Cell::new(0));
        let refusal = match unsafe { tickets.reserve(row, Rundown::new(1, &drops)) } {
            Err(refusal) => refusal,
            Ok(_) => panic!("foreign row admitted"),
        };
        assert!(matches!(
            refusal.reason(),
            TicketRefusal::TableMismatch { .. }
        ));
        drop(refusal.into_rundown());
        assert_eq!(drops.get(), 1);
        assert_eq!(tickets.state_at(0), Some(TicketState::Empty));
    }

    #[test]
    fn external_lookup_actions_stay_compact_and_kind_bound() {
        assert_eq!(size_of::<PreparedTicket<ResourceSlotKind>>(), 48);
        assert_eq!(size_of::<LifecycleAction<ResourceSlotKind>>(), 48);
        assert_eq!(size_of::<DispatchPermit<ResourceSlotKind>>(), 104);
        assert_eq!(size_of::<ObservedDispatch<u8, ResourceSlotKind>>(), 72);
        assert_eq!(size_of::<TicketCompletion<u8, ResourceSlotKind>>(), 120);
        assert_eq!(
            size_of::<ControlTicketSlot<usize, u8, ResourceSlotKind>>(),
            152
        );

        fn resource_only(_: TicketCompletion<u8, ResourceSlotKind>) {}
        fn pair_only(_: TicketCompletion<u8, PairSlotKind>) {}
        let _ = resource_only as fn(TicketCompletion<u8, ResourceSlotKind>);
        let _ = pair_only as fn(TicketCompletion<u8, PairSlotKind>);
    }
}
