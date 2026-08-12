//! Dormant HPS2 D2 platform plane: exact HWA2 admission, fenced replacement,
//! and reset-bounded backing custody for source 0 / plane 0.

#![allow(
    dead_code,
    reason = "D2 is deliberately dormant until the later atomic D3/D4/D9 activation"
)]

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::committed_mode::PowerSubject;
use helios_kmd_logic::direct_scanout_admission::{
    validate_direct_scanout_binding as validate_binding_model, OperationFacts, PlaneFacts,
};
use helios_kmd_logic::direct_scanout_lifetime::{
    BackendBinding, Binding, CompletionKey, DrainReason, Effect, Event, Lifecycle, PendingPhase,
    PlaneState, Transition,
};
use helios_protocol::diagnostics::HeliosGraphicsEtwPayloadV1;
use helios_protocol::{
    HeliosAdapterMatch, HELIOS_PACKAGE_GENERATION, VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
};
use wdk_sys::{HANDLE, NTSTATUS, STATUS_DEVICE_NOT_READY, STATUS_INVALID_PARAMETER, STATUS_SUCCESS};

use crate::adapter::AdapterContext;
use crate::irql::PassiveLevel;
use crate::sync::SpinLock;
use crate::virtio::ctrl::{
    FencedScanoutPublish, FencedScanoutSetOutcome, ScanoutBindIdentity,
};
use crate::virtio::VirtioError;

use super::committed_mode::{CommittedModeRead, CommittedModeWriteRefusal};
use super::create_allocation::DirectScanoutAllocationFacts;
use super::vidpn::CommittedVidPnFacts;

const SOURCE_ID: u32 = 0;
const PLANE_INDEX: u32 = 0;
const MAILBOX_EMPTY: u32 = 0;
const MAILBOX_WRITING: u32 = 1;
const MAILBOX_READY: u32 = 2;
const MAILBOX_READING: u32 = 3;
const QUARANTINE_SLOTS: usize = 4;

const _: () = assert!(super::vidpn::NUM_VIDPN_SOURCES == 1);

static ADMISSION_REFUSALS: AtomicU32 = AtomicU32::new(0);
static MAILBOX_REFUSALS: AtomicU32 = AtomicU32::new(0);
static PLANE_REFUSALS: AtomicU32 = AtomicU32::new(0);
static RESET_REFUSALS: AtomicU32 = AtomicU32::new(0);

fn record_refusal(counter: &AtomicU32, name: &'static [u8], code: u32) {
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 || count % 64 == 0 {
        crate::diag::record_named_bytes(name, (code << 24) | count.min(0x00ff_ffff));
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DirectScanoutOperation {
    pub immediate_flip: bool,
    pub stereo: bool,
    pub shared_primary_transition: bool,
    pub independent_flip_exclusive: bool,
    pub unsupported_or_reserved_flags: u32,
}

#[derive(Clone, Copy)]
struct DisplayBacking {
    allocation_handle: usize,
    allocation_generation: u64,
    resource_id: u32,
    width: u32,
    height: u32,
    format: u32,
    stride: u32,
    offset: u32,
}

impl DisplayBacking {
    fn matches_allocation(&self, handle: usize, generation: u64, resource_id: u32) -> bool {
        self.allocation_handle == handle
            && self.allocation_generation == generation
            && self.resource_id == resource_id
    }
}

pub(crate) struct ValidatedDirectScanoutBinding {
    binding: Binding<DisplayBacking>,
    mode_generation: u64,
}

struct CandidateMailbox {
    state: AtomicU32,
    value: UnsafeCell<MaybeUninit<ValidatedDirectScanoutBinding>>,
}

// SAFETY: exactly one producer owns WRITING and exactly one consumer owns
// READING. READY is Release-published and acquired before the value is read.
unsafe impl Send for CandidateMailbox {}
unsafe impl Sync for CandidateMailbox {}

impl CandidateMailbox {
    const fn new() -> Self {
        Self {
            state: AtomicU32::new(MAILBOX_EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    fn publish(&self, candidate: ValidatedDirectScanoutBinding) -> Result<(), ()> {
        self.state
            .compare_exchange(
                MAILBOX_EMPTY,
                MAILBOX_WRITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| ())?;
        // SAFETY: this producer exclusively owns WRITING until the Release
        // publication below; no consumer reads before observing READY.
        unsafe { (*self.value.get()).write(candidate) };
        self.state.store(MAILBOX_READY, Ordering::Release);
        Ok(())
    }

    fn take(&self) -> Option<ValidatedDirectScanoutBinding> {
        self.state
            .compare_exchange(
                MAILBOX_READY,
                MAILBOX_READING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()?;
        // SAFETY: READY proved initialization and this consumer exclusively
        // owns READING. `read` moves the one value out exactly once.
        let value = unsafe { (*self.value.get()).assume_init_read() };
        self.state.store(MAILBOX_EMPTY, Ordering::Release);
        Some(value)
    }

    fn take_matching(
        &self,
        handle: usize,
        generation: u64,
        resource_id: u32,
    ) -> MailboxMatch {
        let state = self.state.load(Ordering::Acquire);
        if state == MAILBOX_EMPTY {
            return MailboxMatch::Absent;
        }
        if state != MAILBOX_READY
            || self
                .state
                .compare_exchange(
                    MAILBOX_READY,
                    MAILBOX_READING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return MailboxMatch::Busy;
        }
        // SAFETY: this consumer exclusively owns the initialized READY value.
        let value = unsafe { (*self.value.get()).assume_init_read() };
        if value
            .binding
            .token()
            .matches_allocation(handle, generation, resource_id)
        {
            self.state.store(MAILBOX_EMPTY, Ordering::Release);
            MailboxMatch::Taken(value)
        } else {
            // SAFETY: READING is still exclusively ours; restore the unrelated
            // value before republishing READY.
            unsafe { (*self.value.get()).write(value) };
            self.state.store(MAILBOX_READY, Ordering::Release);
            MailboxMatch::Absent
        }
    }

    fn busy(&self) -> bool {
        matches!(
            self.state.load(Ordering::Acquire),
            MAILBOX_WRITING | MAILBOX_READING
        )
    }

    fn empty(&self) -> bool {
        self.state.load(Ordering::Acquire) == MAILBOX_EMPTY
    }
}

enum MailboxMatch {
    Absent,
    Busy,
    Taken(ValidatedDirectScanoutBinding),
}

struct RuntimeState {
    plane: Option<PlaneState<DisplayBacking>>,
    parking_reserve: Option<Binding<DisplayBacking>>,
    quarantine: [Option<BackendBinding<DisplayBacking>>; QUARANTINE_SLOTS],
    poisoned: bool,
}

impl RuntimeState {
    const fn new() -> Self {
        Self {
            plane: None,
            parking_reserve: None,
            quarantine: [None, None, None, None],
            poisoned: false,
        }
    }

    fn quarantine(&mut self, binding: BackendBinding<DisplayBacking>) {
        if let Some(slot) = self.quarantine.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(binding);
        } else {
            // OwnerTable still holds the real backing/finalizer. Forget only
            // the value token rather than manufacturing an early release when
            // a violated invariant exhausted the bounded platform quarantine.
            core::mem::forget(binding);
            self.poisoned = true;
            record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 0xfe);
        }
    }
}

/// Stable adapter-owned storage for the selected one-source/one-plane profile.
pub(crate) struct DirectScanoutRuntime {
    mailbox: CandidateMailbox,
    state: SpinLock<RuntimeState>,
    next_plane_generation: AtomicU64,
}

impl DirectScanoutRuntime {
    pub(crate) const fn new() -> Self {
        Self {
            mailbox: CandidateMailbox::new(),
            state: SpinLock::new(RuntimeState::new()),
            next_plane_generation: AtomicU64::new(1),
        }
    }

    fn mint_plane_generation(&self) -> Option<u64> {
        self.next_plane_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .ok()
    }
}

/// Bounded, nonpageable D2 admission over the exact OS `hAllocation`.
///
/// This routine performs only handle/object reads, atomics, fixed comparisons,
/// and a short canonical-owner observation. It neither allocates nor waits and
/// grants no plane authority until its result is retained in the mailbox.
pub(crate) unsafe fn validate_direct_scanout_binding(
    adapter: &AdapterContext,
    h_allocation: HANDLE,
    os_source: u32,
    operation: DirectScanoutOperation,
    plane: PlaneFacts,
) -> Result<ValidatedDirectScanoutBinding, NTSTATUS> {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return Err(STATUS_DEVICE_NOT_READY);
    }
    let mode = match adapter.committed_mode.read() {
        Ok(CommittedModeRead::Present(observation)) => observation.mode(),
        _ => {
            record_refusal(&ADMISSION_REFUSALS, b"D2AdmRef", 1);
            return Err(STATUS_DEVICE_NOT_READY);
        }
    };
    // SAFETY: dxgkrnl keeps the exact OS-supplied allocation live for this DDI;
    // the accessor checks the KMD object magic before reading its final record.
    let Some(DirectScanoutAllocationFacts {
        final_hwa2,
        resource_id,
        allocation_generation,
        transport_instance,
    }) = (unsafe { super::create_allocation::direct_scanout_allocation_facts(h_allocation) })
    else {
        record_refusal(&ADMISSION_REFUSALS, b"D2AdmRef", 2);
        return Err(STATUS_INVALID_PARAMETER);
    };
    let current_transport = adapter
        .with_virtio(|gpu| gpu.scanout_transport_instance())
        .unwrap_or(0);
    let owner = adapter.control_owner();
    if transport_instance != current_transport
        || !owner.resource_is_live(resource_id)
        || owner
            .resource_size(resource_id)
            .ok()
            .is_none_or(|size| size < final_hwa2.byte_size)
    {
        record_refusal(&ADMISSION_REFUSALS, b"D2AdmRef", 3);
        return Err(STATUS_DEVICE_NOT_READY);
    }
    let operation = OperationFacts {
        current_mode_generation: mode.generation,
        adapter_match: HeliosAdapterMatch::SameAdapter,
        immediate_flip: operation.immediate_flip,
        stereo: operation.stereo,
        shared_primary_transition: operation.shared_primary_transition,
        independent_flip_exclusive: operation.independent_flip_exclusive,
        unsupported_or_reserved_flags: operation.unsupported_or_reserved_flags,
    };
    if validate_binding_model(&final_hwa2, &mode, os_source, &operation, &plane).is_err() {
        record_refusal(&ADMISSION_REFUSALS, b"D2AdmRef", 4);
        return Err(STATUS_INVALID_PARAMETER);
    }
    let plane0 = final_hwa2.planes[0];
    Ok(ValidatedDirectScanoutBinding {
        binding: Binding::new(
            DisplayBacking {
                allocation_handle: h_allocation as usize,
                allocation_generation,
                resource_id,
                width: final_hwa2.width,
                height: final_hwa2.height,
                format: VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
                stride: plane0.row_pitch,
                offset: plane0.offset as u32,
            },
            allocation_generation,
        ),
        mode_generation: mode.generation,
    })
}

/// Retain a validated candidate without taking a DISPATCH spinlock. The HPD
/// thread performs the control submission at PASSIVE.
pub(crate) fn retain_candidate(
    adapter: &AdapterContext,
    candidate: ValidatedDirectScanoutBinding,
) -> NTSTATUS {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_DEVICE_NOT_READY;
    }
    // Serialize the lock-free mailbox publication with the plane's transition
    // to Draining. No allocation, wait, callback, or virtqueue work occurs in
    // this spinlock hold. Once begin_drain owns the state, a late DDI can no
    // longer leave a READY candidate behind the reset barrier.
    let published = {
        let state = adapter.direct_scanout.state.lock();
        let ready = !state.poisoned
            && state.plane.as_ref().is_some_and(|plane| {
                plane.lifecycle() == Lifecycle::Active
                    && plane.pending_phase() == PendingPhase::None
            });
        ready && adapter.direct_scanout.mailbox.publish(candidate).is_ok()
    };
    if !published {
        record_refusal(&MAILBOX_REFUSALS, b"D2MbxRef", 1);
        return STATUS_DEVICE_NOT_READY;
    }
    adapter.signal_hpd();
    STATUS_SUCCESS
}

/// Replace the current reader with parking for an OS-requested zero-plane
/// binding, then reopen the plane for a later exact replacement.
pub(crate) fn explicit_unbind(passive: PassiveLevel, adapter: &AdapterContext) -> NTSTATUS {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_DEVICE_NOT_READY;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        if !unbind_locked(passive, adapter, DrainReason::ExplicitUnbind, true)
            || !resume_plane(adapter)
        {
            STATUS_DEVICE_NOT_READY
        } else {
            STATUS_SUCCESS
        }
    })
}

struct PublishCapture {
    candidate: UnsafeCell<Option<Binding<DisplayBacking>>>,
    transitions: UnsafeCell<[Option<Transition<DisplayBacking>>; 2]>,
    key: UnsafeCell<Option<CompletionKey>>,
}

impl PublishCapture {
    fn candidate(binding: Binding<DisplayBacking>) -> Self {
        Self {
            candidate: UnsafeCell::new(Some(binding)),
            transitions: UnsafeCell::new([None, None]),
            key: UnsafeCell::new(None),
        }
    }

    fn disable() -> Self {
        Self {
            candidate: UnsafeCell::new(None),
            transitions: UnsafeCell::new([None, None]),
            key: UnsafeCell::new(None),
        }
    }

    fn take_candidate(&self) -> Option<Binding<DisplayBacking>> {
        // SAFETY: the publish callback is nested synchronously inside the one
        // issuing thread; no second callback or observer can access this cell.
        unsafe { (&mut *self.candidate.get()).take() }
    }

    fn store_transition(&self, index: usize, transition: Transition<DisplayBacking>) {
        // SAFETY: same synchronous one-callback ownership as `take_candidate`.
        unsafe { (&mut *self.transitions.get())[index] = Some(transition) };
    }

    fn store_key(&self, key: CompletionKey) {
        // SAFETY: same synchronous one-callback ownership as `take_candidate`.
        unsafe { *self.key.get() = Some(key) };
    }

    fn take_results(
        &self,
    ) -> (
        Option<Binding<DisplayBacking>>,
        [Option<Transition<DisplayBacking>>; 2],
        Option<CompletionKey>,
    ) {
        // SAFETY: the control call (and therefore its nested callback) has
        // returned; the issuing thread again has exclusive access to all cells.
        unsafe {
            (
                (&mut *self.candidate.get()).take(),
                core::mem::replace(&mut *self.transitions.get(), [None, None]),
                (&mut *self.key.get()).take(),
            )
        }
    }
}

#[derive(Clone, Copy)]
enum PublishKind {
    Real,
    Parking,
    DisableZero,
}

fn emit_event(adapter: &AdapterContext, event: Event) {
    let (object_generation, plane_generation, sequence) = match event {
        Event::PlaneCandidate {
            object_generation,
            plane_generation,
            binding_sequence,
        }
        | Event::PlaneLatch {
            object_generation,
            plane_generation,
            binding_sequence,
        }
        | Event::PlaneCancel {
            object_generation,
            plane_generation,
            binding_sequence,
        }
        | Event::PlaneReaderRelease {
            object_generation,
            plane_generation,
            binding_sequence,
        }
        | Event::PlaneUnbind {
            object_generation,
            plane_generation,
            binding_sequence,
        } => (object_generation, plane_generation, binding_sequence),
        Event::DeviceReset | Event::DeviceRemoval => (0, 0, 0),
    };
    let adapter_generation = super::diag_etw::adapter_rundown_snapshot(adapter).epoch;
    super::diag_etw::emit(adapter, event.etw_event_id(), || {
        HeliosGraphicsEtwPayloadV1::new(
            HELIOS_PACKAGE_GENERATION,
            adapter_generation,
            object_generation,
            plane_generation,
            sequence,
            0,
            event.etw_subkind(),
            SOURCE_ID,
            PLANE_INDEX,
        )
    });
}

fn recycle_binding(adapter: &AdapterContext, binding: BackendBinding<DisplayBacking>) {
    match binding {
        BackendBinding::Real(_) => {}
        BackendBinding::Parking(binding) => {
            let mut state = adapter.direct_scanout.state.lock();
            if state.parking_reserve.is_none() {
                state.parking_reserve = Some(binding);
            } else {
                state.quarantine(BackendBinding::Parking(binding));
                state.poisoned = true;
            }
        }
    }
}

fn finish_transition(adapter: &AdapterContext, mut transition: Transition<DisplayBacking>) {
    for event in transition.events.into_iter().flatten() {
        emit_event(adapter, event);
    }
    if let Some(binding) = transition.release_candidate.take() {
        recycle_binding(adapter, binding);
    }
    if let Some(binding) = transition.release_backend.take() {
        recycle_binding(adapter, binding);
    }
}

/// Physical reset/removal has already retired the canonical OwnerTable rows.
/// Barrier releases therefore discard only these values-only plane tokens;
/// recycling a parking token here would resurrect authority for a dead resource.
fn finish_barrier_transition(
    adapter: &AdapterContext,
    mut transition: Transition<DisplayBacking>,
) {
    for event in transition.events.into_iter().flatten() {
        emit_event(adapter, event);
    }
    let _ = transition.release_candidate.take();
    let _ = transition.release_backend.take();
}

fn identity_key(identity: &ScanoutBindIdentity) -> CompletionKey {
    CompletionKey::new(
        identity.instance(),
        identity.fence_id(),
        identity.sequence(),
    )
}

fn issue_fenced_set(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    kind: PublishKind,
    binding: Option<Binding<DisplayBacking>>,
    geometry: DisplayBacking,
    expected_instance: u64,
) -> bool {
    let capture = match binding {
        Some(binding) => PublishCapture::candidate(binding),
        None => PublishCapture::disable(),
    };
    let publish = |published: FencedScanoutPublish| {
        let key = CompletionKey::new(
            published.instance,
            published.fence_id,
            published.sequence,
        );
        capture.store_key(key);
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned
            || published.instance != expected_instance
            || published.resource_id != geometry.resource_id
            || published.sequence == 0
            || published.fence_id == 0
        {
            if let Some(binding) = capture.take_candidate() {
                state.quarantine(match kind {
                    PublishKind::Real => BackendBinding::Real(binding),
                    PublishKind::Parking => BackendBinding::Parking(binding),
                    PublishKind::DisableZero => return,
                });
            }
            state.poisoned = true;
            return;
        }
        let Some(plane) = state.plane.as_mut() else {
            if let Some(binding) = capture.take_candidate() {
                state.quarantine(match kind {
                    PublishKind::Real => BackendBinding::Real(binding),
                    PublishKind::Parking => BackendBinding::Parking(binding),
                    PublishKind::DisableZero => return,
                });
            }
            state.poisoned = true;
            return;
        };
        match kind {
            PublishKind::Real | PublishKind::Parking => {
                let Some(binding) = capture.take_candidate() else {
                    state.poisoned = true;
                    return;
                };
                let mut retained = match kind {
                    PublishKind::Real => plane.retain_real_candidate(binding, published.sequence),
                    PublishKind::Parking => {
                        plane.retain_parking_candidate(binding, published.sequence)
                    }
                    PublishKind::DisableZero => unreachable!(),
                };
                if retained.effect != Effect::CandidateRetained {
                    if let Some(binding) = retained.release_candidate.take() {
                        state.quarantine(binding);
                    }
                    state.poisoned = true;
                    capture.store_transition(0, retained);
                    return;
                }
                capture.store_transition(0, retained);
                let submitted = plane.submit_candidate(key);
                if submitted.effect != Effect::CandidateSubmitted {
                    state.poisoned = true;
                }
                capture.store_transition(1, submitted);
            }
            PublishKind::DisableZero => {
                let submitted = plane.submit_disable_zero(key);
                if submitted.effect != Effect::DisableZeroSubmitted {
                    state.poisoned = true;
                }
                capture.store_transition(0, submitted);
            }
        }
    };
    let outcome = crate::virtio::ctrl::set_scanout_blob_fenced(
        passive,
        adapter,
        geometry.resource_id,
        geometry.width,
        geometry.height,
        geometry.format,
        geometry.stride,
        geometry.offset,
        expected_instance,
        &publish,
    );
    let (remaining, transitions, published_key) = capture.take_results();
    for transition in transitions.into_iter().flatten() {
        finish_transition(adapter, transition);
    }

    let mut retain_remaining_ambiguously = false;
    let exact = match outcome {
        FencedScanoutSetOutcome::Accepted(identity) => Some((identity, true)),
        FencedScanoutSetOutcome::Rejected(identity) => Some((identity, false)),
        FencedScanoutSetOutcome::DefiniteNotEnqueued { error, instance } => {
            let status: NTSTATUS = error.into();
            record_refusal(
                &PLANE_REFUSALS,
                b"D2PlnRef",
                if instance == expected_instance {
                    (status as u32) & 0xff
                } else {
                    0xfd
                },
            );
            None
        }
        FencedScanoutSetOutcome::Ambiguous(identity) => {
            retain_remaining_ambiguously = identity.is_none();
            None
        }
    };
    if let Some(binding) = remaining {
        if retain_remaining_ambiguously || published_key.is_some() {
            let mut state = adapter.direct_scanout.state.lock();
            state.quarantine(match kind {
                PublishKind::Real => BackendBinding::Real(binding),
                PublishKind::Parking => BackendBinding::Parking(binding),
                PublishKind::DisableZero => return false,
            });
            state.poisoned = true;
        } else {
            recycle_binding(
                adapter,
                match kind {
                    PublishKind::Real => BackendBinding::Real(binding),
                    PublishKind::Parking => BackendBinding::Parking(binding),
                    PublishKind::DisableZero => return false,
                },
            );
        }
    }
    let Some((identity, success)) = exact else {
        return false;
    };
    let key = identity_key(&identity);
    if published_key != Some(key) || identity.resource_id() != geometry.resource_id {
        let mut state = adapter.direct_scanout.state.lock();
        state.poisoned = true;
        record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 2);
        return false;
    }
    let transition = {
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            return false;
        }
        let Some(plane) = state.plane.as_mut() else {
            return false;
        };
        match kind {
            PublishKind::Real | PublishKind::Parking => {
                plane.complete_replacement(key, success)
            }
            PublishKind::DisableZero => plane.complete_disable_zero(key, success),
        }
    };
    let terminal = match kind {
        PublishKind::Real => {
            (success && transition.effect == Effect::ReplacementLatched)
                || (!success && transition.effect == Effect::ReplacementFailed)
        }
        PublishKind::Parking => {
            (success && transition.effect == Effect::ParkingLatched)
                || (!success && transition.effect == Effect::ParkingReplacementFailed)
        }
        PublishKind::DisableZero => {
            (success && transition.effect == Effect::DisableZeroCompleted)
                || (!success && transition.effect == Effect::DisableZeroFailed)
        }
    };
    if !terminal {
        record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 3);
    }
    finish_transition(adapter, transition);
    terminal && success
}

fn service_pending_locked(passive: PassiveLevel, adapter: &AdapterContext) {
    let Some(candidate) = adapter.direct_scanout.mailbox.take() else {
        return;
    };
    let current_generation = match adapter.committed_mode.read() {
        Ok(CommittedModeRead::Present(observation)) => observation.mode().generation,
        _ => 0,
    };
    if current_generation == 0 || current_generation != candidate.mode_generation {
        record_refusal(&ADMISSION_REFUSALS, b"D2AdmRef", 5);
        return;
    }
    let ready = {
        let state = adapter.direct_scanout.state.lock();
        !state.poisoned
            && state.plane.as_ref().is_some_and(|plane| {
                plane.lifecycle() == Lifecycle::Active
                    && plane.pending_phase() == PendingPhase::None
            })
    };
    if !ready {
        record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 4);
        return;
    }
    let geometry = *candidate.binding.token();
    let expected_instance = {
        let state = adapter.direct_scanout.state.lock();
        state
            .plane
            .as_ref()
            .map_or(0, PlaneState::transport_epoch)
    };
    let _ = issue_fenced_set(
        passive,
        adapter,
        PublishKind::Real,
        Some(candidate.binding),
        geometry,
        expected_instance,
    );
}

pub(crate) fn service_pending(passive: PassiveLevel, adapter: &AdapterContext) {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        service_pending_locked(passive, adapter)
    });
}

fn issue_parking_locked(passive: PassiveLevel, adapter: &AdapterContext) -> bool {
    let (binding, expected_instance) = {
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            return false;
        }
        let Some(binding) = state.parking_reserve.take() else {
            state.poisoned = true;
            record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 5);
            return false;
        };
        let Some(plane) = state.plane.as_ref() else {
            state.parking_reserve = Some(binding);
            return false;
        };
        (binding, plane.transport_epoch())
    };
    let geometry = *binding.token();
    issue_fenced_set(
        passive,
        adapter,
        PublishKind::Parking,
        Some(binding),
        geometry,
        expected_instance,
    )
}

fn issue_disable_locked(passive: PassiveLevel, adapter: &AdapterContext) -> bool {
    let expected_instance = {
        let state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            return false;
        }
        let Some(plane) = state.plane.as_ref() else {
            return false;
        };
        if !matches!(plane.lifecycle(), Lifecycle::Quiescent(_))
            || plane.pending_phase() != PendingPhase::None
            || !plane
                .backend()
                .is_some_and(|backend| backend.binding.is_parking())
        {
            return false;
        }
        plane.transport_epoch()
    };
    issue_fenced_set(
        passive,
        adapter,
        PublishKind::DisableZero,
        None,
        DisplayBacking {
            allocation_handle: 0,
            allocation_generation: 0,
            resource_id: 0,
            width: 0,
            height: 0,
            format: 0,
            stride: 0,
            offset: 0,
        },
        expected_instance,
    )
}

fn unbind_locked(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    reason: DrainReason,
    optional_disable: bool,
) -> bool {
    let (begin, mailbox_candidate) = {
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            return false;
        }
        // RetainCandidate takes this same plane lock before publishing. Taking
        // the mailbox while changing lifecycle therefore gives one atomic
        // cancel-vs-drain edge: a later producer observes Draining and refuses.
        let mailbox_candidate = adapter.direct_scanout.mailbox.take();
        let Some(plane) = state.plane.as_mut() else {
            drop(state);
            drop(mailbox_candidate);
            return true;
        };
        (plane.begin_drain(reason), mailbox_candidate)
    };
    // This values-only candidate had no binding sequence and never reached the
    // host. Drop it outside the plane spinlock; no cancel ETW edge was minted.
    drop(mailbox_candidate);
    finish_transition(adapter, begin);
    let disposition = {
        let state = adapter.direct_scanout.state.lock();
        let Some(plane) = state.plane.as_ref() else {
            return true;
        };
        if state.poisoned {
            return false;
        }
        match (plane.pending_phase(), plane.backend()) {
            (PendingPhase::None, Some(backend)) if backend.binding.is_real() => 1,
            (PendingPhase::None, Some(backend)) if backend.binding.is_parking() => 2,
            (PendingPhase::None, None) => 3,
            _ => 0,
        }
    };
    if disposition == 0 {
        return false;
    }
    if disposition == 1 && !issue_parking_locked(passive, adapter) {
        return false;
    }
    let real_released = {
        let state = adapter.direct_scanout.state.lock();
        state.plane.as_ref().is_none_or(|plane| {
            !plane
                .backend()
                .is_some_and(|backend| backend.binding.is_real())
                && !plane
                    .pending_binding()
                    .is_some_and(BackendBinding::is_real)
        })
    };
    if !real_released {
        return false;
    }
    if optional_disable {
        let _ = issue_disable_locked(passive, adapter);
    }
    true
}

#[inline(never)]
pub(crate) fn start(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    width: u32,
    height: u32,
) -> Result<(), VirtioError> {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return Ok(());
    }
    adapter.with_scanout_lifecycle(passive, |guard| {
        let expected_instance = adapter
            .with_virtio(|gpu| gpu.scanout_transport_instance())
            .map_err(|_| VirtioError::DeviceError)?;
        let parking = guard
            .with_venus_client(|client| {
                client.allocate_linear_scanout_image_blob(adapter, width, height)
            })
            .map_err(|_| VirtioError::DeviceError)??;
        crate::virtio::venus::zero_host_visible_blob(
            passive,
            adapter,
            parking.blob.res_id,
            parking.blob.size,
        )?;
        let Some(plane_generation) = adapter.direct_scanout.mint_plane_generation() else {
            return Err(VirtioError::OutOfMemory);
        };
        let plane = PlaneState::new(plane_generation, expected_instance)
            .map_err(|_| VirtioError::DeviceError)?;
        let parking = Binding::new(
            DisplayBacking {
                allocation_handle: 0,
                allocation_generation: plane_generation,
                resource_id: parking.blob.res_id,
                width,
                height,
                format: VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
                stride: parking.row_pitch,
                offset: parking.plane_offset,
            },
            plane_generation,
        );
        let mut state = adapter.direct_scanout.state.lock();
        let replaceable = state.plane.as_ref().is_none_or(|old| {
            old.lifecycle() == Lifecycle::ResetQuiescent && old.held_reference_count() == 0
        });
        if !replaceable
            || state.parking_reserve.is_some()
            || state.quarantine.iter().any(Option::is_some)
            || !adapter.direct_scanout.mailbox.empty()
        {
            state.quarantine(BackendBinding::Parking(parking));
            state.poisoned = true;
            return Err(VirtioError::DeviceError);
        }
        state.plane = Some(plane);
        state.parking_reserve = Some(parking);
        state.poisoned = false;
        Ok(())
    })
}

pub(crate) fn prepare_reset(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    reason: DrainReason,
) {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        let _ = unbind_locked(passive, adapter, reason, true);
        if adapter.committed_mode.crash_reset_close().is_err() {
            record_refusal(&RESET_REFUSALS, b"D2RstRef", 1);
        }
    });
}

pub(crate) fn complete_verified_reset(passive: PassiveLevel, adapter: &AdapterContext) {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        if adapter.direct_scanout.mailbox.take().is_none()
            && adapter.direct_scanout.mailbox.busy()
        {
            record_refusal(&RESET_REFUSALS, b"D2RstRef", 2);
        }
        let transition = {
            let mut state = adapter.direct_scanout.state.lock();
            let transition = state
                .plane
                .as_mut()
                .map(PlaneState::complete_reset_barrier);
            state.parking_reserve = None;
            for slot in &mut state.quarantine {
                *slot = None;
            }
            state.poisoned = false;
            transition
        };
        if let Some(transition) = transition {
            finish_barrier_transition(adapter, transition);
        }
        if adapter
            .committed_mode
            .complete_reset_barrier(passive)
            .is_err()
        {
            record_refusal(&RESET_REFUSALS, b"D2RstRef", 3);
        }
    });
}

pub(crate) fn complete_removal(passive: PassiveLevel, adapter: &AdapterContext) {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        let transition = {
            let mut state = adapter.direct_scanout.state.lock();
            let transition = state
                .plane
                .as_mut()
                .map(PlaneState::complete_removal_barrier);
            state.parking_reserve = None;
            for slot in &mut state.quarantine {
                *slot = None;
            }
            transition
        };
        if let Some(transition) = transition {
            finish_barrier_transition(adapter, transition);
        }
        if adapter.committed_mode.remove_permanently(passive).is_err() {
            record_refusal(&RESET_REFUSALS, b"D2RstRef", 4);
        }
    });
}

fn state_holds_allocation(
    adapter: &AdapterContext,
    handle: usize,
    generation: u64,
    resource_id: u32,
) -> bool {
    let state = adapter.direct_scanout.state.lock();
    let plane_holds = state.plane.as_ref().is_some_and(|plane| {
        plane.backend().is_some_and(|backend| {
            backend
                .binding
                .binding()
                .token()
                .matches_allocation(handle, generation, resource_id)
        }) || plane.pending_binding().is_some_and(|binding| {
            binding
                .binding()
                .token()
                .matches_allocation(handle, generation, resource_id)
        })
    });
    plane_holds
        || state.quarantine.iter().flatten().any(|binding| {
            binding
                .binding()
                .token()
                .matches_allocation(handle, generation, resource_id)
        })
}

/// DestroyAllocation's exact backing barrier. `true` permits canonical
/// DETACH/UNREF; `false` retains the allocation row until verified reset.
pub(crate) fn retire_allocation(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    h_allocation: HANDLE,
    generation: u64,
    resource_id: u32,
) -> bool {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return true;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        match adapter.direct_scanout.mailbox.take_matching(
            h_allocation as usize,
            generation,
            resource_id,
        ) {
            MailboxMatch::Taken(_) | MailboxMatch::Absent => {}
            MailboxMatch::Busy => return false,
        }
        if !state_holds_allocation(adapter, h_allocation as usize, generation, resource_id) {
            return true;
        }
        if !unbind_locked(passive, adapter, DrainReason::AllocationDestroyed, true) {
            return false;
        }
        !state_holds_allocation(adapter, h_allocation as usize, generation, resource_id)
    })
}

fn resume_plane(adapter: &AdapterContext) -> bool {
    let transition = {
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            return false;
        }
        let Some(plane) = state.plane.as_mut() else {
            return true;
        };
        match plane.lifecycle() {
            Lifecycle::Active => return true,
            Lifecycle::Quiescent(_) | Lifecycle::ResetQuiescent => plane.resume(),
            _ => return false,
        }
    };
    let resumed = transition.effect == Effect::Resumed;
    finish_transition(adapter, transition);
    resumed
}

fn mode_write_status(result: Result<u64, CommittedModeWriteRefusal>) -> NTSTATUS {
    if result.is_ok() {
        STATUS_SUCCESS
    } else {
        STATUS_DEVICE_NOT_READY
    }
}

fn committed_mode_allows_scanout(adapter: &AdapterContext) -> bool {
    matches!(
        adapter.committed_mode.read(),
        Ok(CommittedModeRead::Present(observation))
            if observation.mode().active
                && observation.mode().visible
                && observation.mode().powered
    )
}

pub(crate) fn commit_mode(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    facts: CommittedVidPnFacts,
) -> NTSTATUS {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_SUCCESS;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        // Every commit is a mode boundary, including the first and an empty or
        // powered-off commit. Drain before changing the atomic mode record, then
        // resume only when the complete effective policy permits scanout.
        if !unbind_locked(passive, adapter, DrainReason::ModeChange, false) {
            return STATUS_DEVICE_NOT_READY;
        }
        match facts {
            CommittedVidPnFacts::Active(facts) => {
                let status = mode_write_status(adapter.committed_mode.publish_active(passive, facts));
                if status == STATUS_SUCCESS
                    && committed_mode_allows_scanout(adapter)
                    && !resume_plane(adapter)
                {
                    STATUS_DEVICE_NOT_READY
                } else {
                    status
                }
            }
            CommittedVidPnFacts::Empty { source_id } => mode_write_status(
                adapter
                    .committed_mode
                    .publish_empty_commit(passive, source_id),
            ),
        }
    })
}

pub(crate) fn transition_visibility(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    source_id: u32,
    visible: bool,
) -> NTSTATUS {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_SUCCESS;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        if !visible && !unbind_locked(passive, adapter, DrainReason::SourceInvisible, true) {
            return STATUS_DEVICE_NOT_READY;
        }
        let status = mode_write_status(
            adapter
                .committed_mode
                .transition_visibility(passive, source_id, visible),
        );
        if status == STATUS_SUCCESS
            && committed_mode_allows_scanout(adapter)
            && !resume_plane(adapter)
        {
            STATUS_DEVICE_NOT_READY
        } else {
            status
        }
    })
}

pub(crate) fn transition_power(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    subject: PowerSubject,
    powered: bool,
) -> NTSTATUS {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_SUCCESS;
    }
    adapter.with_scanout_lifecycle(passive, |_guard| {
        if !powered && !unbind_locked(passive, adapter, DrainReason::PowerTransition, true) {
            return STATUS_DEVICE_NOT_READY;
        }
        let status = mode_write_status(
            adapter
                .committed_mode
                .transition_power(passive, subject, powered),
        );
        if status == STATUS_SUCCESS
            && committed_mode_allows_scanout(adapter)
            && !resume_plane(adapter)
        {
            STATUS_DEVICE_NOT_READY
        } else {
            status
        }
    })
}
