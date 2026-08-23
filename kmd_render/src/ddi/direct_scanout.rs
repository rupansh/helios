//! D2 platform plane: exact HWA2 admission, fenced replacement, and
//! reset-bounded backing custody for source 0 / plane 0.

#![allow(
    dead_code,
    reason = "the active D2 path retains audited helpers for refusal and reset edges"
)]

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::committed_mode::PowerSubject;
use helios_kmd_logic::control_ownership::HostRejection;
use helios_kmd_logic::direct_scanout_admission::{
    refusal_code_and_detail, validate_direct_scanout_binding as validate_binding_model,
    OperationFacts, PlaneFacts,
};
use helios_kmd_logic::direct_scanout_lifetime::{
    BackendBinding, Binding, CompletionKey, DrainReason, Effect, Event, Lifecycle, PendingPhase,
    PlaneState, Transition,
};
use helios_protocol::diagnostics::HeliosGraphicsEtwPayloadV1;
use helios_protocol::{
    HeliosAdapterMatch, VirtioGpuCtrlHdr, DXGI_FORMAT_B8G8R8X8_UNORM, HELIOS_PACKAGE_GENERATION,
    VIRTIO_GPU_FLAG_FENCE, VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM, VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
    VIRTIO_GPU_RESP_OK_NODATA,
};
use wdk_sys::{
    HANDLE, NTSTATUS, STATUS_DEVICE_NOT_READY, STATUS_INVALID_PARAMETER, STATUS_SUCCESS,
};

use crate::adapter::AdapterContext;
use crate::irql::PassiveLevel;
use crate::sync::SpinLock;
use crate::virtio::ctrl::{FencedScanoutPublish, FencedScanoutSetOutcome, ScanoutBindIdentity};
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

struct RefusalCounter {
    count: AtomicU32,
    last_reason: AtomicU32,
}

impl RefusalCounter {
    const fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            last_reason: AtomicU32::new(0),
        }
    }
}

static ADMISSION_REFUSALS: RefusalCounter = RefusalCounter::new();
static MAILBOX_REFUSALS: RefusalCounter = RefusalCounter::new();
static PLANE_REFUSALS: RefusalCounter = RefusalCounter::new();
static RESET_REFUSALS: RefusalCounter = RefusalCounter::new();
// The admission refusal codes seen this boot, as a bitset over `code - 0x40`,
// plus the last refusal's diagnostic scalar. See `refusal_code_and_detail`.
static ADMISSION_REASON_SET_LO: AtomicU32 = AtomicU32::new(0);
static ADMISSION_REASON_SET_HI: AtomicU32 = AtomicU32::new(0);
static ADMISSION_REFUSAL_DETAIL: AtomicU32 = AtomicU32::new(0);
// Why the direct-scanout plane poisoned. 18 sites set `poisoned` and none of
// them named itself, so a poisoned plane presented only as
// `SetVidPnSourceVisibility(TRUE) -> DEVICE_NOT_READY` with no cause (measured
// 22.22.344.0). Codes are 1..=18 in file order; POISON_SET is a bitset over
// `code - 1`, because the first poison is the one that matters and the last
// overwrites it.
static POISON_COUNT: AtomicU32 = AtomicU32::new(0);
static POISON_FIRST: AtomicU32 = AtomicU32::new(0);
static POISON_SET: AtomicU32 = AtomicU32::new(0);
// The kmd_logic `Refusal` behind the poison, when a refused Transition is the
// cause. `helios_kmd_logic::direct_scanout_lifetime::refusal_code`, 1..=28.
static POISON_REFUSAL: AtomicU32 = AtomicU32::new(0);
// Last 0xf0 completion refusal's raw facts — see complete_queued.
static COMPLETION_RESPONSE_TYPE: AtomicU32 = AtomicU32::new(0);
static COMPLETION_RESPONSE_FLAGS: AtomicU32 = AtomicU32::new(0);
static COMPLETION_FAIL_BITS: AtomicU32 = AtomicU32::new(0);
// Presents actually issued, and their failures. D2FlshN must MOVE for anything
// to be on screen: a bind the host never read is a black desktop.
static SCANOUT_FLUSHES: AtomicU32 = AtomicU32::new(0);
static SCANOUT_FLUSH_FAILURES: AtomicU32 = AtomicU32::new(0);
// Where a present is lost between completion and flush. Each counts one exact
// step, so a gap between two of them names the failing edge.
static COMPLETIONS_TERMINAL: AtomicU32 = AtomicU32::new(0);
static COMPLETIONS_ACCEPTED: AtomicU32 = AtomicU32::new(0);
static FLUSH_REQUESTS_PUBLISHED: AtomicU32 = AtomicU32::new(0);
static FLUSH_REQUESTS_TAKEN: AtomicU32 = AtomicU32::new(0);
static SERVICE_PENDING_RUNS: AtomicU32 = AtomicU32::new(0);

/// Which bind kinds reached the host, and the last REAL bind's exact wire
/// geometry. The host trace names a resource id per SET_SCANOUT_BLOB; nothing
/// guest-side did, so "res 5 read all zeros" could not be attributed to a
/// subject (measured 22.22.347.0: three flushes, three zero reads).
static BIND_REAL: AtomicU32 = AtomicU32::new(0);
static BIND_PARKING: AtomicU32 = AtomicU32::new(0);
static BIND_DISABLE: AtomicU32 = AtomicU32::new(0);
static BIND_LAST_RESOURCE: AtomicU32 = AtomicU32::new(0);
static BIND_LAST_EXTENT: AtomicU32 = AtomicU32::new(0);
static BIND_LAST_STRIDE: AtomicU32 = AtomicU32::new(0);
static BIND_LAST_FORMAT: AtomicU32 = AtomicU32::new(0);
static BIND_LAST_OFFSET: AtomicU32 = AtomicU32::new(0);
/// Why the plane drained: `direct_scanout_lifetime::drain_reason_code`, 1..=7.
/// The FIRST drain is the one that matters — after it the OS stopped offering
/// addresses entirely (D2AdmRef=D4AdrRef=0 with the display already dark).
static DRAIN_COUNT: AtomicU32 = AtomicU32::new(0);
static DRAIN_FIRST: AtomicU32 = AtomicU32::new(0);
static DRAIN_LAST: AtomicU32 = AtomicU32::new(0);
static DRAIN_SET: AtomicU32 = AtomicU32::new(0);

fn note_bind_real(geometry: &DisplayBacking) {
    BIND_REAL.fetch_add(1, Ordering::Relaxed);
    BIND_LAST_RESOURCE.store(geometry.resource_id, Ordering::Relaxed);
    BIND_LAST_EXTENT.store(
        (geometry.width << 16) | (geometry.height & 0xffff),
        Ordering::Relaxed,
    );
    BIND_LAST_STRIDE.store(geometry.stride, Ordering::Relaxed);
    BIND_LAST_FORMAT.store(geometry.format, Ordering::Relaxed);
    BIND_LAST_OFFSET.store(geometry.offset, Ordering::Relaxed);
}

/// The flushed blob as the GUEST sees it, sampled through the canonical map.
/// The host's own readback of every real bind is all-zero while a KMD-painted
/// parking blob reads back exactly (22.22.348.0), so the open question is
/// whether the producer ever writes these bytes at all. Knob `D2PxProbe`.
static PIXEL_PROBE_RUNS: AtomicU32 = AtomicU32::new(0);
static PIXEL_PROBE_ERRORS: AtomicU32 = AtomicU32::new(0);
static PIXEL_PROBE_RESOURCE: AtomicU32 = AtomicU32::new(0);
static PIXEL_PROBE_NONZERO: AtomicU32 = AtomicU32::new(0);
static PIXEL_PROBE_MAX: AtomicU32 = AtomicU32::new(0);
static PIXEL_PROBE_ENABLED: AtomicU32 = AtomicU32::new(0);
/// The same sampler run against the parking blob the KMD has just written
/// itself, so a `D2PxNz=0` on a real primary cannot be read as a broken probe.
static PIXEL_PROBE_PARK: AtomicU32 = AtomicU32::new(0);

/// Cached `D2ParkPaint` byte, refreshed at every `start`. 0 = shipping.
static PARK_PAINT: AtomicU32 = AtomicU32::new(0);

fn note_drain(reason: DrainReason) {
    let code = helios_kmd_logic::direct_scanout_lifetime::drain_reason_code(reason);
    DRAIN_COUNT.fetch_add(1, Ordering::Relaxed);
    let _ = DRAIN_FIRST.compare_exchange(0, code, Ordering::Relaxed, Ordering::Relaxed);
    DRAIN_LAST.store(code, Ordering::Relaxed);
    DRAIN_SET.fetch_or(1u32 << (code - 1), Ordering::Relaxed);
}


fn record_refusal(counter: &RefusalCounter, _name: &'static [u8], code: u32) {
    // Admission is shared by MPO3 and classic SetVidPn, whose latter entry may
    // run at device DIRQL. Keep the entire transitive refusal path atomics-only;
    // the PASSIVE diagnostics snapshot below owns registry publication.
    counter.last_reason.store(code, Ordering::Relaxed);
    counter.count.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_refusal_counters() {
    for (name, reason_name, counter) in [
        (
            b"D2AdmRef".as_slice(),
            b"D2AdmWhy".as_slice(),
            &ADMISSION_REFUSALS,
        ),
        (
            b"D2MbxRef".as_slice(),
            b"D2MbxWhy".as_slice(),
            &MAILBOX_REFUSALS,
        ),
        (
            b"D2PlnRef".as_slice(),
            b"D2PlnWhy".as_slice(),
            &PLANE_REFUSALS,
        ),
        (
            b"D2RstRef".as_slice(),
            b"D2RstWhy".as_slice(),
            &RESET_REFUSALS,
        ),
    ] {
        crate::diag::record_named_bytes(name, counter.count.load(Ordering::Relaxed));
        crate::diag::record_named_bytes(reason_name, counter.last_reason.load(Ordering::Relaxed));
    }
    crate::diag::record_named_bytes(b"D2PsnN", POISON_COUNT.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PsnWh1", POISON_FIRST.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PsnSet", POISON_SET.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PsnRfs", POISON_REFUSAL.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(
        b"D2AdmSL",
        ADMISSION_REASON_SET_LO.load(Ordering::Relaxed),
    );
    crate::diag::record_named_bytes(
        b"D2AdmSH",
        ADMISSION_REASON_SET_HI.load(Ordering::Relaxed),
    );
    crate::diag::record_named_bytes(
        b"D2AdmDat",
        ADMISSION_REFUSAL_DETAIL.load(Ordering::Relaxed),
    );
    crate::diag::record_named_bytes(
        b"D2CmpTyp",
        COMPLETION_RESPONSE_TYPE.load(Ordering::Relaxed),
    );
    crate::diag::record_named_bytes(
        b"D2CmpFlg",
        COMPLETION_RESPONSE_FLAGS.load(Ordering::Relaxed),
    );
    crate::diag::record_named_bytes(b"D2CmpBit", COMPLETION_FAIL_BITS.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2FlshN", SCANOUT_FLUSHES.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2FlshE", SCANOUT_FLUSH_FAILURES.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2CmpTrm", COMPLETIONS_TERMINAL.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2CmpAcc", COMPLETIONS_ACCEPTED.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(
        b"D2FlqPub",
        FLUSH_REQUESTS_PUBLISHED.load(Ordering::Relaxed),
    );
    crate::diag::record_named_bytes(b"D2FlqTak", FLUSH_REQUESTS_TAKEN.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2SvcRun", SERVICE_PENDING_RUNS.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnReal", BIND_REAL.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnPark", BIND_PARKING.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnDis", BIND_DISABLE.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnRid", BIND_LAST_RESOURCE.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnWH", BIND_LAST_EXTENT.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnPch", BIND_LAST_STRIDE.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnFmt", BIND_LAST_FORMAT.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2BnOff", BIND_LAST_OFFSET.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2DrnN", DRAIN_COUNT.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2DrnWh1", DRAIN_FIRST.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2DrnLst", DRAIN_LAST.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2DrnSet", DRAIN_SET.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PxN", PIXEL_PROBE_RUNS.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PxErr", PIXEL_PROBE_ERRORS.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PxRid", PIXEL_PROBE_RESOURCE.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PxNz", PIXEL_PROBE_NONZERO.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PxMax", PIXEL_PROBE_MAX.load(Ordering::Relaxed));
    crate::diag::record_named_bytes(b"D2PxPark", PIXEL_PROBE_PARK.load(Ordering::Relaxed));
}

pub(crate) fn reset_refusal_counters() {
    for counter in [
        &ADMISSION_REFUSALS,
        &MAILBOX_REFUSALS,
        &PLANE_REFUSALS,
        &RESET_REFUSALS,
    ] {
        counter.count.store(0, Ordering::Relaxed);
        counter.last_reason.store(0, Ordering::Relaxed);
    }
    // Same reason as the loop above (R505): a value that merely EXISTS in the
    // service key must not read as one that moved this boot.
    for counter in [
        &ADMISSION_REASON_SET_LO,
        &ADMISSION_REASON_SET_HI,
        &ADMISSION_REFUSAL_DETAIL,
        &POISON_COUNT,
        &POISON_FIRST,
        &POISON_SET,
        &POISON_REFUSAL,
        &BIND_REAL,
        &BIND_PARKING,
        &BIND_DISABLE,
        &BIND_LAST_RESOURCE,
        &BIND_LAST_EXTENT,
        &BIND_LAST_STRIDE,
        &BIND_LAST_FORMAT,
        &BIND_LAST_OFFSET,
        &DRAIN_COUNT,
        &DRAIN_FIRST,
        &DRAIN_LAST,
        &DRAIN_SET,
        &PIXEL_PROBE_RUNS,
        &PIXEL_PROBE_ERRORS,
        &PIXEL_PROBE_RESOURCE,
        &PIXEL_PROBE_NONZERO,
        &PIXEL_PROBE_MAX,
        &PIXEL_PROBE_PARK,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DirectScanoutOperation {
    pub immediate_flip: bool,
    pub stereo: bool,
    pub shared_primary_transition: bool,
    pub independent_flip_exclusive: bool,
    /// Classic `ModeChange` flip — see `OperationFacts::mode_change`.
    pub mode_change: bool,
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
    transport_instance: u64,
}

/// Exact classic/DMA source switch retained by the fixed interrupt queue before
/// the DDI acknowledges it.  This is move-only because the embedded `Binding`
/// is the candidate-custody token.
pub(crate) struct QueuedDirectScanoutBinding {
    candidate: ValidatedDirectScanoutBinding,
    source_id: u32,
    primary_segment: u32,
    primary_address: u64,
    operation_flags: u32,
}

impl QueuedDirectScanoutBinding {
    pub(crate) fn from_exact_os_transition(
        candidate: ValidatedDirectScanoutBinding,
        source_id: u32,
        primary_segment: u32,
        primary_address: u64,
        operation_flags: u32,
    ) -> Self {
        Self {
            candidate,
            source_id,
            primary_segment,
            primary_address,
            operation_flags,
        }
    }

    pub(crate) fn resource_id(&self) -> u32 {
        self.candidate.binding.token().resource_id
    }

    pub(crate) fn width(&self) -> u32 {
        self.candidate.binding.token().width
    }

    pub(crate) fn height(&self) -> u32 {
        self.candidate.binding.token().height
    }

    pub(crate) fn format(&self) -> u32 {
        self.candidate.binding.token().format
    }

    pub(crate) fn stride(&self) -> u32 {
        self.candidate.binding.token().stride
    }

    pub(crate) fn offset(&self) -> u32 {
        self.candidate.binding.token().offset
    }

    pub(crate) fn transport_instance(&self) -> u64 {
        self.candidate.transport_instance
    }

    pub(crate) fn matches_exact_allocation(
        &self,
        handle: usize,
        generation: u64,
        resource_id: u32,
    ) -> bool {
        self.candidate
            .binding
            .token()
            .matches_allocation(handle, generation, resource_id)
    }

    fn into_parts(self) -> (Binding<DisplayBacking>, u64, u64, u32, u32, u64, u32) {
        (
            self.candidate.binding,
            self.candidate.mode_generation,
            self.candidate.transport_instance,
            self.source_id,
            self.primary_segment,
            self.primary_address,
            self.operation_flags,
        )
    }
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

    fn take_matching(&self, handle: usize, generation: u64, resource_id: u32) -> MailboxMatch {
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
    /// Poison the plane and name the site that did it.
    ///
    /// Atomics only: several callers hold the state spinlock at DISPATCH, where
    /// `diag::record` (a registry write) is illegal. The PASSIVE snapshot in
    /// `record_refusal_counters` publishes these.
    /// [`Self::poison`] plus the `kmd_logic` refusal that caused it.
    fn poison_refused<T>(&mut self, code: u32, transition: &Transition<T>) {
        if let Some(refusal) = transition.refusal {
            POISON_REFUSAL.store(
                helios_kmd_logic::direct_scanout_lifetime::refusal_code(refusal),
                Ordering::Relaxed,
            );
        }
        self.poison(code);
    }

    fn poison(&mut self, code: u32) {
        self.poisoned = true;
        POISON_COUNT.fetch_add(1, Ordering::Relaxed);
        let _ = POISON_FIRST.compare_exchange(0, code, Ordering::Relaxed, Ordering::Relaxed);
        POISON_SET.fetch_or(1u32 << (code - 1), Ordering::Relaxed);
    }

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
            self.poison(1);
            record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 0xfe);
        }
    }
}

/// Stable adapter-owned storage for the selected one-source/one-plane profile.
pub(crate) struct DirectScanoutRuntime {
    mailbox: CandidateMailbox,
    state: SpinLock<RuntimeState>,
    next_plane_generation: AtomicU64,
    /// Zero closes admission. A nonzero value is the exact live transport epoch
    /// published only after parking and PlaneState are fully initialized. DIRQL
    /// validation reads this atomically instead of taking `virtio_lock`.
    active_transport_instance: AtomicU64,
    /// The presentation the host still owes us a read of: `resource_id |
    /// width << 32 | height << 48`, 0 when none. Published by the DPC (which
    /// may not issue a control request) and consumed by the PASSIVE worker.
    /// One word so a bind cannot be paired with another bind's geometry.
    flush_request: AtomicU64,
}

impl DirectScanoutRuntime {
    pub(crate) const fn new() -> Self {
        Self {
            mailbox: CandidateMailbox::new(),
            state: SpinLock::new(RuntimeState::new()),
            next_plane_generation: AtomicU64::new(1),
            active_transport_instance: AtomicU64::new(0),
            flush_request: AtomicU64::new(0),
        }
    }

    /// Latest-wins: a superseded frame is not worth a flush of its own, and the
    /// newer request already covers the same scanout.
    fn request_flush(&self, resource_id: u32, width: u32, height: u32) {
        if resource_id == 0
            || width == 0
            || height == 0
            || width > u16::MAX as u32
            || height > u16::MAX as u32
        {
            return;
        }
        let packed = resource_id as u64 | ((width as u64) << 32) | ((height as u64) << 48);
        self.flush_request.store(packed, Ordering::Release);
        FLUSH_REQUESTS_PUBLISHED.fetch_add(1, Ordering::Relaxed);
    }

    fn take_flush_request(&self) -> Option<(u32, u32, u32)> {
        match self.flush_request.swap(0, Ordering::AcqRel) {
            0 => None,
            packed => Some({
                FLUSH_REQUESTS_TAKEN.fetch_add(1, Ordering::Relaxed);
                (
                    packed as u32,
                    ((packed >> 32) & 0xffff) as u32,
                    ((packed >> 48) & 0xffff) as u32,
                )
            }),
        }
    }

    fn mint_plane_generation(&self) -> Option<u64> {
        self.next_plane_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .ok()
    }

    pub(crate) fn active_transport_instance(&self) -> u64 {
        self.active_transport_instance.load(Ordering::Acquire)
    }
}

/// Bounded, nonpageable D2 admission over the exact OS `hAllocation`.
///
/// This routine performs only handle/object reads, atomics, fixed comparisons,
/// and immutable canonical-allocation observations. It neither allocates nor
/// waits and grants no plane authority until its result is retained in the
/// mailbox or the D4 fixed queue.
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
        backing_size,
    }) = (unsafe { super::create_allocation::direct_scanout_allocation_facts(h_allocation) })
    else {
        record_refusal(&ADMISSION_REFUSALS, b"D2AdmRef", 2);
        return Err(STATUS_INVALID_PARAMETER);
    };
    let current_transport = adapter.direct_scanout.active_transport_instance();
    if current_transport == 0
        || transport_instance != current_transport
        || backing_size < final_hwa2.byte_size
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
        mode_change: operation.mode_change,
        unsupported_or_reserved_flags: operation.unsupported_or_reserved_flags,
    };
    let verdict = validate_binding_model(&final_hwa2, &mode, os_source, &operation, &plane);
    if let Err(refusal) = verdict {
        // Every arm now carries its own code. The `_ => 0x7F` bucket this
        // replaces swallowed 32 of the 34 DMA-flip admissions on 22.22.341.0
        // and named none of them, which is the whole black-desktop question.
        let (code, detail) = refusal_code_and_detail(refusal);
        record_refusal(&ADMISSION_REFUSALS, b"D2AdmRef", code);
        // Codes are 0x40..=0x7B, so the SET of reasons seen fits in two words.
        // `D2AdmWhy` is last-value and cannot tell one repeated refusal from a
        // mixture; these two can.
        let bit = code - 0x40;
        if bit < 32 {
            ADMISSION_REASON_SET_LO.fetch_or(1u32 << bit, Ordering::Relaxed);
        } else {
            ADMISSION_REASON_SET_HI.fetch_or(1u32 << (bit - 32), Ordering::Relaxed);
        }
        ADMISSION_REFUSAL_DETAIL.store(detail, Ordering::Relaxed);
        return Err(STATUS_INVALID_PARAMETER);
    }
    let plane0 = final_hwa2.planes[0];
    // The admitted set is exactly {87 Bgra8, 88 Bgrx8}; map to the matching
    // SET_SCANOUT_BLOB wire format instead of claiming alpha on an XR24 frame.
    // A literal match, not the kmd_logic converter: this path is on D4's
    // audited DIRQL call surface, which pins its qualified-call set.
    let wire_format = if final_hwa2.dxgi_format == DXGI_FORMAT_B8G8R8X8_UNORM {
        VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM
    } else {
        VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM
    };
    Ok(ValidatedDirectScanoutBinding {
        binding: Binding::new(
            DisplayBacking {
                allocation_handle: h_allocation as usize,
                allocation_generation,
                resource_id,
                width: final_hwa2.width,
                height: final_hwa2.height,
                format: wire_format,
                stride: plane0.row_pitch,
                offset: plane0.offset as u32,
            },
            allocation_generation,
        ),
        mode_generation: mode.generation,
        transport_instance,
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

/// DPC continuation for one fixed DIRQL/DISPATCH D4 descriptor.
///
/// Queue removal already proved the exact descriptor token. This routine
/// validates the complete response provenance, moves the pre-retained backing
/// into the ordinary D2 PlaneState, and performs no allocation, wait, control
/// request, or cleanup callback. An ambiguous response quarantines the real
/// backing and poisons the plane generation instead of guessing whether QEMU
/// changed its persistent reader.
pub(crate) fn complete_queued(
    adapter: &AdapterContext,
    completion: crate::virtio::gpu::DirectQueueCompletion,
) {
    let resource_id = completion.work.resource_id();
    let work_instance = completion.work.transport_instance();
    let response = (completion.written_length as usize == core::mem::size_of::<VirtioGpuCtrlHdr>())
        .then(|| {
            // SAFETY: the fixed byte array contains exactly one complete response
            // when the used length matches; the array has no alignment promise.
            unsafe {
                core::ptr::read_unaligned(completion.response.as_ptr().cast::<VirtioGpuCtrlHdr>())
            }
        });
    let exact_fence = response.is_some_and(|header| {
        header.flags == VIRTIO_GPU_FLAG_FENCE
            && header.fence_id == completion.fence_id
            && header.ctx_id == 0
            && header.ring_idx == 0
            && header.padding == [0; 3]
    });
    let accepted =
        exact_fence && response.is_some_and(|header| header.type_ == VIRTIO_GPU_RESP_OK_NODATA);
    let rejected = exact_fence
        && response.is_some_and(|header| HostRejection::from_response_type(header.type_).is_ok());
    let (
        binding,
        mode_generation,
        candidate_instance,
        source_id,
        _primary_segment,
        primary_address,
        _operation_flags,
    ) = completion.work.into_parts();

    let current_mode = match adapter.committed_mode.read() {
        Ok(CommittedModeRead::Present(observation)) => Some(observation.mode()),
        _ => None,
    };
    let current_mode_generation = current_mode.map_or(0, |mode| mode.generation);
    let provenance_exact = accepted || rejected;
    let identity_exact_except_generation = completion.instance != 0
        && completion.instance == work_instance
        && completion.instance == candidate_instance
        && completion.sequence != 0
        && completion.fence_id != 0
        && completion.resource_id == resource_id
        && source_id == SOURCE_ID;
    let identity_exact = identity_exact_except_generation
        && current_mode_generation != 0
        && current_mode_generation == mode_generation;
    // The stored generation moves on EVERY policy write (visibility/power
    // rewrite mode.generation, and storage reconstructs it from high_water),
    // so a bind in flight across the modeset's own visibility-TRUE is stale by
    // construction (D2CmpBit=0xFF7 on all four boot flips, 22.22.334.0).
    // Staleness is not corruption: when the CURRENT mode still names this
    // exact source and geometry, the OS still wants this frame — accept it as
    // current. A geometry/source change means the frame really is outdated:
    // retain the backing (QEMU may still read it) without poisoning.
    let stale_generation_only = provenance_exact
        && identity_exact_except_generation
        && current_mode_generation != 0
        && current_mode_generation != mode_generation;
    let bound_geometry = *binding.token();
    let flush_width = bound_geometry.width;
    let flush_height = bound_geometry.height;
    let geometry_current = current_mode.is_some_and(|mode| {
        mode.active
            && mode.source_id == SOURCE_ID
            && mode.source_width == binding.token().width
            && mode.source_height == binding.token().height
    });
    if stale_generation_only && !geometry_current {
        let mut state = adapter.direct_scanout.state.lock();
        state.quarantine(BackendBinding::Real(binding));
        drop(state);
        record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 0xf1);
        return;
    }
    let identity_exact = identity_exact || (stale_generation_only && geometry_current);
    if !provenance_exact || !identity_exact {
        // DIRQL-safe facts for the 0xf0 refusal: the response type, its
        // flags/length, and one bit per sub-check (0 = the failing one).
        // Published by record_refusal_counters as D2CmpTyp/D2CmpFlg/D2CmpBit.
        COMPLETION_RESPONSE_TYPE
            .store(response.map_or(0, |header| header.type_), Ordering::Relaxed);
        COMPLETION_RESPONSE_FLAGS.store(
            response.map_or(0, |header| header.flags) | ((completion.written_length as u32) << 16),
            Ordering::Relaxed,
        );
        let bits = (response.is_some() as u32)
            | ((exact_fence as u32) << 1)
            | ((accepted as u32) << 2)
            | ((rejected as u32) << 3)
            | (((completion.instance != 0) as u32) << 4)
            | (((completion.instance == work_instance) as u32) << 5)
            | (((completion.instance == candidate_instance) as u32) << 6)
            | (((completion.sequence != 0) as u32) << 7)
            | (((completion.fence_id != 0) as u32) << 8)
            | (((completion.resource_id == resource_id) as u32) << 9)
            | (((source_id == SOURCE_ID) as u32) << 10)
            | (((current_mode_generation != 0) as u32) << 11)
            | (((current_mode_generation == mode_generation) as u32) << 12);
        COMPLETION_FAIL_BITS.store(bits, Ordering::Relaxed);
        let mut state = adapter.direct_scanout.state.lock();
        state.quarantine(BackendBinding::Real(binding));
        state.poison(2);
        drop(state);
        record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 0xf0);
        return;
    }

    let key = CompletionKey::new(
        completion.instance,
        completion.fence_id,
        completion.sequence,
    );
    let mut transitions: [Option<Transition<DisplayBacking>>; 3] = [None, None, None];
    let terminal = {
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            state.quarantine(BackendBinding::Real(binding));
            false
        } else if state.plane.as_ref().is_none_or(|plane| {
            plane.lifecycle() != Lifecycle::Active
                || plane.pending_phase() != PendingPhase::None
                || plane.transport_epoch() != completion.instance
        }) {
            state.quarantine(BackendBinding::Real(binding));
            state.poison(3);
            false
        } else if let Some(plane) = state.plane.as_mut() {
            let mut retained = plane.retain_real_candidate(binding, completion.sequence);
            if retained.effect != Effect::CandidateRetained {
                if let Some(candidate) = retained.release_candidate.take() {
                    state.quarantine(candidate);
                }
                state.poison(4);
                transitions[0] = Some(retained);
                false
            } else {
                transitions[0] = Some(retained);
                let submitted = plane.submit_candidate(key);
                let submitted_ok = submitted.effect == Effect::CandidateSubmitted;
                transitions[1] = Some(submitted);
                if !submitted_ok {
                    state.poison(5);
                    false
                } else {
                    let completed = plane.complete_replacement(key, accepted);
                    let terminal = (accepted && completed.effect == Effect::ReplacementLatched)
                        || (!accepted && completed.effect == Effect::ReplacementFailed);
                    if !terminal {
                        state.poison(6);
                    }
                    transitions[2] = Some(completed);
                    terminal
                }
            }
        } else {
            // The predicate above already classifies `None` as a poisoned
            // completion. Keep this arm explicit anyway: a future predicate
            // refactor must quarantine custody instead of turning an internal
            // mismatch into a kernel panic.
            state.quarantine(BackendBinding::Real(binding));
            state.poison(7);
            false
        }
    };
    for transition in transitions.into_iter().flatten() {
        finish_transition(adapter, transition);
    }
    if !terminal {
        record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 0xf1);
        return;
    }
    COMPLETIONS_TERMINAL.fetch_add(1, Ordering::Relaxed);
    if accepted {
        COMPLETIONS_ACCEPTED.fetch_add(1, Ordering::Relaxed);
        note_bind_real(&bound_geometry);
        adapter
            .last_primary_address
            .store(primary_address, Ordering::Release);
        // QEMU reads a blob scanout only on RESOURCE_FLUSH, and this routine may
        // not issue a control request, so hand the present to the PASSIVE worker.
        adapter
            .direct_scanout
            .request_flush(resource_id, flush_width, flush_height);
        adapter.signal_hpd();
    }
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
                state.poison(8);
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
fn finish_barrier_transition(adapter: &AdapterContext, mut transition: Transition<DisplayBacking>) {
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
        // The descriptor is minted, so this command is on the wire whatever the
        // plane bookkeeping below decides.
        match kind {
            PublishKind::Real => note_bind_real(&geometry),
            PublishKind::Parking => {
                BIND_PARKING.fetch_add(1, Ordering::Relaxed);
            }
            PublishKind::DisableZero => {
                BIND_DISABLE.fetch_add(1, Ordering::Relaxed);
            }
        }
        let key = CompletionKey::new(published.instance, published.fence_id, published.sequence);
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
            state.poison(9);
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
            state.poison(10);
            return;
        };
        match kind {
            PublishKind::Real | PublishKind::Parking => {
                let Some(binding) = capture.take_candidate() else {
                    state.poison(11);
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
                    state.poison_refused(12, &retained);
                    capture.store_transition(0, retained);
                    return;
                }
                capture.store_transition(0, retained);
                let submitted = plane.submit_candidate(key);
                if submitted.effect != Effect::CandidateSubmitted {
                    state.poison_refused(13, &submitted);
                }
                capture.store_transition(1, submitted);
            }
            PublishKind::DisableZero => {
                let submitted = plane.submit_disable_zero(key);
                if submitted.effect != Effect::DisableZeroSubmitted {
                    state.poison_refused(14, &submitted);
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
            state.poison(15);
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
        state.poison(16);
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
            PublishKind::Real | PublishKind::Parking => plane.complete_replacement(key, success),
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
    let bound = terminal && success;
    // Knob-only: parking is otherwise never flushed, so a painted parking image
    // would never be read. Issued inside the lifecycle lock, which the shipping
    // present path deliberately avoids — acceptable for a diagnostic that is
    // off by default and only runs on a drain.
    if bound
        && matches!(kind, PublishKind::Parking)
        && PARK_PAINT.load(Ordering::Relaxed) != 0
    {
        let _ = crate::virtio::ctrl::resource_flush(
            passive,
            adapter,
            geometry.resource_id,
            geometry.width,
            geometry.height,
        );
    }
    bound
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
        state.plane.as_ref().map_or(0, PlaneState::transport_epoch)
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
    SERVICE_PENDING_RUNS.fetch_add(1, Ordering::Relaxed);
    adapter.with_scanout_lifecycle(passive, |_guard| service_pending_locked(passive, adapter));
    issue_pending_flush(passive, adapter);
}

/// Present what the last accepted bind left owed.
///
/// Outside the lifecycle lock on purpose: this is a whole-surface present of an
/// already-latched binding, and holding that lock across a control round-trip
/// would serialize the next flip behind the host.
fn issue_pending_flush(passive: PassiveLevel, adapter: &AdapterContext) {
    let Some((resource_id, width, height)) = adapter.direct_scanout.take_flush_request() else {
        return;
    };
    if crate::virtio::ctrl::resource_flush(passive, adapter, resource_id, width, height).is_ok() {
        SCANOUT_FLUSHES.fetch_add(1, Ordering::Relaxed);
    } else {
        SCANOUT_FLUSH_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    sample_flushed_blob(passive, adapter, resource_id);
}

/// Knob-only: read back what the GUEST sees in the blob the host was just told
/// to present.
///
/// Deliberately never unmaps. The window mapping is idempotent per resource
/// (`map_blob_prepare` returns the live one), so the probe costs at most one
/// window slot per scanned-out resource, and unmapping could pull the mapping
/// out from under a UMD that owns the same backing.
fn sample_flushed_blob(passive: PassiveLevel, adapter: &AdapterContext, resource_id: u32) {
    if PIXEL_PROBE_ENABLED.load(Ordering::Relaxed) == 0 || resource_id == 0 {
        return;
    }
    PIXEL_PROBE_RUNS.fetch_add(1, Ordering::Relaxed);
    PIXEL_PROBE_RESOURCE.store(resource_id, Ordering::Relaxed);
    let Ok(prep) = crate::virtio::ctrl::map_blob_prepare(
        passive,
        adapter,
        crate::virtio::gpu::OwnerFilter::Exactly(None),
        resource_id,
    ) else {
        PIXEL_PROBE_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let Some(sample) = crate::virtio::venus::sample_host_visible_blob(prep) else {
        PIXEL_PROBE_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    PIXEL_PROBE_NONZERO.store(sample.nonzero, Ordering::Relaxed);
    PIXEL_PROBE_MAX.store(sample.max as u32, Ordering::Relaxed);
}

fn issue_parking_locked(passive: PassiveLevel, adapter: &AdapterContext) -> bool {
    let (binding, expected_instance) = {
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            return false;
        }
        let Some(binding) = state.parking_reserve.take() else {
            state.poison(17);
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
    note_drain(reason);
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
                && !plane.pending_binding().is_some_and(BackendBinding::is_real)
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
        // Admission must remain closed while parking/PlaneState construction
        // performs PASSIVE control work. Clear a stale generation before any
        // fallible step so a failed restart cannot leave D4 accepting against
        // an earlier plane.
        adapter
            .direct_scanout
            .active_transport_instance
            .store(0, Ordering::Release);
        let expected_instance = adapter
            .with_virtio(|gpu| gpu.scanout_transport_instance())
            .map_err(|_| VirtioError::DeviceError)?;
        let parking = guard
            .with_venus_client(|client| {
                client.allocate_linear_scanout_image_blob(adapter, width, height)
            })
            .map_err(|_| VirtioError::DeviceError)??;
        let paint = (crate::diag::read_config_dword(crate::diag::knobs::PARK_PAINT, 0)
            & 0xff) as u8;
        PARK_PAINT.store(paint as u32, Ordering::Relaxed);
        PIXEL_PROBE_ENABLED.store(
            crate::diag::read_config_dword(crate::diag::knobs::PIXEL_PROBE, 0),
            Ordering::Relaxed,
        );
        crate::virtio::venus::fill_host_visible_blob(
            passive,
            adapter,
            parking.blob.res_id,
            parking.blob.size,
            paint,
        )?;
        if PIXEL_PROBE_ENABLED.load(Ordering::Relaxed) != 0 {
            // The sampler's positive control: this blob was just written by
            // this driver, through the same map the probe reads.
            let sampled = crate::virtio::ctrl::map_blob_prepare(
                passive,
                adapter,
                crate::virtio::gpu::OwnerFilter::Exactly(None),
                parking.blob.res_id,
            )
            .ok()
            .and_then(crate::virtio::venus::sample_host_visible_blob)
            .map_or(0, |sample| (sample.nonzero << 8) | sample.max as u32);
            PIXEL_PROBE_PARK.store(sampled, Ordering::Relaxed);
        }
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
            state.poison(18);
            return Err(VirtioError::DeviceError);
        }
        state.plane = Some(plane);
        state.parking_reserve = Some(parking);
        state.poisoned = false;
        drop(state);
        adapter
            .direct_scanout
            .active_transport_instance
            .store(expected_instance, Ordering::Release);
        Ok(())
    })
}

pub(crate) fn prepare_reset(passive: PassiveLevel, adapter: &AdapterContext, reason: DrainReason) {
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return;
    }
    adapter
        .direct_scanout
        .active_transport_instance
        .store(0, Ordering::Release);
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
    adapter
        .direct_scanout
        .active_transport_instance
        .store(0, Ordering::Release);
    adapter.with_scanout_lifecycle(passive, |_guard| {
        if adapter.direct_scanout.mailbox.take().is_none() && adapter.direct_scanout.mailbox.busy()
        {
            record_refusal(&RESET_REFUSALS, b"D2RstRef", 2);
        }
        let transition = {
            let mut state = adapter.direct_scanout.state.lock();
            let transition = state.plane.as_mut().map(PlaneState::complete_reset_barrier);
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
    adapter
        .direct_scanout
        .active_transport_instance
        .store(0, Ordering::Release);
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
    if adapter.d4_queue_holds_allocation(h_allocation as usize, generation, resource_id) {
        return false;
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
    // 0x132C = why resume_plane failed: 1 poisoned, 2 non-resumable lifecycle,
    // 3 plane.resume() refused (PendingBusy / real backend / generation).
    // Recorded only after the spinlock is released — diag::record is a
    // registry write and this lock raises to DISPATCH.
    let transition = {
        let mut state = adapter.direct_scanout.state.lock();
        if state.poisoned {
            drop(state);
            crate::diag::record(0x132C_0001);
            return false;
        }
        let Some(plane) = state.plane.as_mut() else {
            return true;
        };
        match plane.lifecycle() {
            Lifecycle::Active => return true,
            Lifecycle::Quiescent(_) | Lifecycle::ResetQuiescent => plane.resume(),
            _ => {
                drop(state);
                crate::diag::record(0x132C_0002);
                return false;
            }
        }
    };
    let resumed = transition.effect == Effect::Resumed;
    if !resumed {
        crate::diag::record(0x132C_0003);
    }
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
                let status =
                    mode_write_status(adapter.committed_mode.publish_active(passive, facts));
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
        if !visible {
            // A source going INVISIBLE is not negotiable. dxgkrnl is removing it
            // from the desktop; an error here makes it roll the whole path back,
            // and it does not retry forever -- S-ring 2026-08-23 caught nine
            // Visible=FALSE calls answered 0x132B00A3 (DEVICE_NOT_READY) and
            // then no active path at all, i.e. the desktop was gone. Record
            // what failed, never refuse it.
            if !unbind_locked(passive, adapter, DrainReason::SourceInvisible, true) {
                record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 0xf2);
            }
            if mode_write_status(
                adapter
                    .committed_mode
                    .transition_visibility(passive, source_id, false),
            ) != STATUS_SUCCESS
            {
                record_refusal(&PLANE_REFUSALS, b"D2PlnRef", 0xf3);
            }
            return STATUS_SUCCESS;
        }
        let status = mode_write_status(
            adapter
                .committed_mode
                .transition_visibility(passive, source_id, true),
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
