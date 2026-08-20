//! K5 — HTS1 translation sessions and HQA1 outer-context attach.
//!
//! Every decision with a right and a wrong answer is
//! [`helios_kmd_logic::translation_session`], host-tested; this file does the
//! pointer work, the CSPRNG, the locking and the counters. `helios_protocol`
//! owns the wire records — nothing here re-declares one.
//!
//! # The seam with K6 and K11
//!
//! K5 owns `DxgkDdiCreateContext`'s private-data dispatch, the provisional
//! session the HVC1 **control** context creates, the reply-pool binding, HQA1
//! attach, and the session state every later unit compares against. K6 owns the
//! HVC1 **queue** context, `DxgkDdiRender`'s HNR2 decode, Patch, SubmitCommand
//! and the context-local slot pool. K11 now executes only the finite HTS1 INIT
//! HNR2 Render (§10.4:1206-1211, §17.6:4360-4368) through one private stock-
//! Venus namespace and role-1 HVR1 reply. Queue and allocation-backed execution
//! remain outside this tranche, and source reachability is not target evidence.

use alloc::boxed::Box;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use helios_kmd_logic::translation_session as model;
use helios_protocol::native_render::{
    hvc1_admit_context_flags, parse_hvc1_private_data, Hvc1ContextClass, Hvm1Role,
    HELIOS_HVC1_MAGIC, HELIOS_HVC1_SIZE, HELIOS_NATIVE_RENDER_CAPSET,
};
use helios_protocol::translation_session::{
    parse_create_context_private_data, HeliosSessionCapability, HELIOS_HQA1_MAGIC, HELIOS_HQA1_SIZE,
};
use helios_protocol::HELIOS_PACKAGE_GENERATION;

use crate::adapter::AdapterContext;
use crate::ddi::session_transport::SessionTransport;
use crate::dxgk::*;
use crate::irql::PassiveLevel;
use crate::sync::SpinLock;
use crate::virtio::gpu::DeviceOwner;

// ── Named counters ───────────────────────────────────────────────────────────
//
// Every refusal below increments exactly one of these (CLAUDE.md rule 2).
// [`diag_dump_translation_session_atomics`] mirrors them into the service key
// from `dxgkddi_destroy_device`'s PASSIVE cadence.

/// Control contexts admitted — one provisional HTS1 session each.
pub static TS_SESSION_CREATED: AtomicU32 = AtomicU32::new(0);
/// Sessions freed when their last reference dropped.
pub static TS_SESSION_FREED: AtomicU32 = AtomicU32::new(0);
/// HVC1 records refused, packed `(count << 16) | Hvc1Reject::code()`.
pub static TS_HVC1_REJECT: AtomicU32 = AtomicU32::new(0);
/// `DXGK_CREATECONTEXTFLAGS` carried a bit HVC1 does not admit.
pub static TS_HVC1_FLAGS_REJECT: AtomicU32 = AtomicU32::new(0);
/// ⛔ RETIRED BY K6 AND PERMANENTLY 0. It counted HVC1 **queue** contexts
/// refused because `ddi/native_render.rs` did not exist; it does now, and the
/// arm that wrote this counter is gone. The name stays because registry values
/// persist across boots and a reader comparing two boots must not see it
/// disappear — grade queue contexts by `Nr2QCtx` / `Nr2QCtxRej` instead.
pub static TS_QUEUE_CTX_UNIMPL: AtomicU32 = AtomicU32::new(0);
/// Control Renders refused, packed `(count << 16) | control_render_code()`.
pub static TS_CONTROL_RENDER_REJECT: AtomicU32 = AtomicU32::new(0);
/// Reply slots checked out and given back.
pub static TS_SLOT_RELEASED: AtomicU32 = AtomicU32::new(0);
/// Reply slots that could NOT be given back. **Must read 0**: one stuck slot 0
/// makes every later control Render on the session fail `ControlRenderSlotBusy`,
/// because A1 takes the first idle slot every time.
pub static TS_SLOT_STUCK: AtomicU32 = AtomicU32::new(0);
/// A second control context, or a second session, on one raw KMT device.
pub static TS_SECOND_CONTROL_CTX: AtomicU32 = AtomicU32::new(0);
/// The ProcessContext's bounded session list was full.
pub static TS_SESSION_LIST_FULL: AtomicU32 = AtomicU32::new(0);
/// A create-context arm needed the `hKmdProcess` ProcessContext and had none.
pub static TS_NO_PROCESS: AtomicU32 = AtomicU32::new(0);
/// Session objects that could not be allocated.
pub static TS_ALLOC_FAILED: AtomicU32 = AtomicU32::new(0);
/// HQA1 attaches admitted.
///
/// ⛔ **Unreachable until K6**, and the reason is worth stating: `attach` requires
/// a `Live` session, only `complete_init` makes one `Live`, and `session_init` —
/// its only caller — is K6's seam. Measured on the target 2026-08-10:
/// `TsAttachOk=0`, `TsNoSess=1`, because the lookup finds no live session before
/// the packet is ever validated.
pub static TS_ATTACH_OK: AtomicU32 = AtomicU32::new(0);
/// HQA1 attaches refused, all causes.
///
/// ⚠ Two gradings, and both matter. Until K6 this can move **only** on a
/// malformed packet, because the session lookup fails first for everything else
/// (see [`TS_ATTACH_OK`]). Afterwards, grade it as **must read 0** rather than as
/// the primary refusal path: the ICD's own `HeliosQueueAttachRequestV1::validate`
/// (`protocol/src/translator_dispatch.rs`) refuses a bad endpoint or engine class
/// before an HQA1 exists, so a nonzero value there means a forged or broken ICD.
pub static TS_ATTACH_REJECT: AtomicU32 = AtomicU32::new(0);
/// HQA1 named a (session generation, capability) pair no live session in this
/// ProcessContext holds.
pub static TS_ATTACH_NO_SESSION: AtomicU32 = AtomicU32::new(0);
/// Outer contexts detached at `DxgkDdiDestroyContext` after an exact HQA1
/// admission.
pub static TS_DETACH_OK: AtomicU32 = AtomicU32::new(0);
/// Detach refusals — our own accounting bug. **Must read 0**.
pub static TS_DETACH_REJECT: AtomicU32 = AtomicU32::new(0);
/// Role-1 reply pools bound to a provisional session at open time.
pub static TS_POOL_BOUND: AtomicU32 = AtomicU32::new(0);
/// Role-1 pool bindings refused — a second pool on one session, or a session
/// already draining. The create still succeeds; the mismatch surfaces at the
/// first control Render, which refuses the unbound generation.
pub static TS_POOL_REJECT: AtomicU32 = AtomicU32::new(0);
/// Non-empty create-context private data that matched no known record. Falls
/// through to the legacy arm rather than failing the context — see
/// [`ContextRequest`].
pub static TS_PDD_UNKNOWN: AtomicU32 = AtomicU32::new(0);
/// Sessions moved to `Draining` by reset, removal, or device teardown.
pub static TS_DRAINED: AtomicU32 = AtomicU32::new(0);
/// INIT round trips whose stock-Venus context creation, exact host reply,
/// capability publication, and HVR1 publication all completed.
pub static TS_INIT_OK: AtomicU32 = AtomicU32::new(0);
/// INIT refusals, all causes.
pub static TS_INIT_REJECT: AtomicU32 = AtomicU32::new(0);
/// The CPU could not supply a 128-bit CSPRNG capability, so INIT refused.
/// **Must read 0**: `RDRAND` is present on every x86-64 this package targets.
pub static TS_NO_CSPRNG: AtomicU32 = AtomicU32::new(0);

/// The counter names, as one list, so the collision proof and the writer cannot
/// drift apart.
const COUNTER_NAMES: [&[u8]; 23] = [
    b"TsSessNew",
    b"TsSessFree",
    b"TsHvc1Rej",
    b"TsHvc1Flg",
    b"TsQCtxUnim",
    b"TsCtl2nd",
    b"TsListFull",
    b"TsNoProc",
    b"TsAllocErr",
    b"TsAttachOk",
    b"TsAttachRej",
    b"TsNoSess",
    b"TsDetachOk",
    b"TsDetachRej",
    b"TsPoolBind",
    b"TsPoolRej",
    b"TsPddUnk",
    b"TsDrained",
    b"TsInitOk",
    b"TsInitRej",
    b"TsCtlRej",
    b"TsSlotRel",
    b"TsSlotStuck",
];

/// Compile-time proof that no counter name can be truncated into another's.
/// `diag::record_named_bytes` clamps silently at `MAX_CONFIG_NAME`, so two names
/// sharing that prefix would merge into one registry value.
const _: () = {
    let mut i = 0;
    while i < COUNTER_NAMES.len() {
        assert!(
            COUNTER_NAMES[i].len() <= crate::diag::MAX_CONFIG_NAME,
            "translation-session counter name exceeds MAX_CONFIG_NAME and would merge with another"
        );
        i += 1;
    }
};

/// Mirror the counters into the service key. PASSIVE only.
pub fn diag_dump_translation_session_atomics() {
    let values: [u32; 23] = [
        TS_SESSION_CREATED.load(Ordering::Relaxed),
        TS_SESSION_FREED.load(Ordering::Relaxed),
        TS_HVC1_REJECT.load(Ordering::Relaxed),
        TS_HVC1_FLAGS_REJECT.load(Ordering::Relaxed),
        TS_QUEUE_CTX_UNIMPL.load(Ordering::Relaxed),
        TS_SECOND_CONTROL_CTX.load(Ordering::Relaxed),
        TS_SESSION_LIST_FULL.load(Ordering::Relaxed),
        TS_NO_PROCESS.load(Ordering::Relaxed),
        TS_ALLOC_FAILED.load(Ordering::Relaxed),
        TS_ATTACH_OK.load(Ordering::Relaxed),
        TS_ATTACH_REJECT.load(Ordering::Relaxed),
        TS_ATTACH_NO_SESSION.load(Ordering::Relaxed),
        TS_DETACH_OK.load(Ordering::Relaxed),
        TS_DETACH_REJECT.load(Ordering::Relaxed),
        TS_POOL_BOUND.load(Ordering::Relaxed),
        TS_POOL_REJECT.load(Ordering::Relaxed),
        TS_PDD_UNKNOWN.load(Ordering::Relaxed),
        TS_DRAINED.load(Ordering::Relaxed),
        TS_INIT_OK.load(Ordering::Relaxed),
        TS_INIT_REJECT.load(Ordering::Relaxed),
        TS_CONTROL_RENDER_REJECT.load(Ordering::Relaxed),
        TS_SLOT_RELEASED.load(Ordering::Relaxed),
        TS_SLOT_STUCK.load(Ordering::Relaxed),
    ];
    let mut i = 0;
    while i < COUNTER_NAMES.len() {
        if let (Some(name), Some(value)) = (COUNTER_NAMES.get(i), values.get(i)) {
            crate::diag::record_named_bytes(name, *value);
        }
        i += 1;
    }
    // Two failure counters that must read 0 and have no name slot above; packed
    // so a single value names both without a 21st registry write.
    let packed = (TS_NO_CSPRNG.load(Ordering::Relaxed) << 16)
        | (TS_INIT_REJECT.load(Ordering::Relaxed) & 0xFFFF);
    crate::diag::record_named_bytes(b"TsRngFail", packed);
}

/// Pack a count with a `protocol` reason code into one registry value, so the
/// refusal names both how often and which field.
fn bump_with_code(counter: &AtomicU32, code: u32) {
    let prior = counter.load(Ordering::Relaxed) >> 16;
    let next = prior.saturating_add(1).min(0xFFFF);
    counter.store((next << 16) | (code & 0xFFFF), Ordering::Relaxed);
}

// ── The 128-bit admission capability ─────────────────────────────────────────

/// Draw a 128-bit CSPRNG admission nonce, or `None`.
///
/// `RDRAND` rather than `BCryptGenRandom`: it needs no import library, no
/// load-order dependency on `ksecdd`, and is legal at any IRQL. The all-zero
/// pair is redrawn because it is `HeliosSessionCapability::INVALID`, the
/// invalidated sentinel, and could never be presented back.
fn mint_capability() -> Option<HeliosSessionCapability> {
    if !rdrand_supported() {
        return None;
    }
    let mut attempt = 0;
    while attempt < 8 {
        if let (Some(low), Some(high)) = (rdrand64(), rdrand64()) {
            let capability = HeliosSessionCapability::from_halves(low, high);
            if !capability.is_invalid() {
                return Some(capability);
            }
        }
        attempt += 1;
    }
    None
}

/// CPUID leaf 1, `ECX` bit 30. A plain CPUID instruction, legal at any IRQL.
fn rdrand_supported() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0
}

/// One `RDRAND` with the Intel-recommended 10-retry bound. Callers must have
/// established support via [`rdrand_supported`] first.
fn rdrand64() -> Option<u64> {
    #[target_feature(enable = "rdrand")]
    unsafe fn draw() -> Option<u64> {
        let mut value: u64 = 0;
        let mut tries = 0;
        while tries < 10 {
            if core::arch::x86_64::_rdrand64_step(&mut value) == 1 {
                return Some(value);
            }
            tries += 1;
        }
        None
    }
    // SAFETY: reached only downstream of `rdrand_supported()`.
    unsafe { draw() }
}

/// The HTS1 INIT reply's byte count, as an array bound.
///
/// A `const` plus an assert rather than a literal `56`: [`session_init`] ends in
/// a `copy_from_slice`, which **panics** on a length mismatch — and a panic in a
/// DDI is a silent graphics deadlock. If the record ever grows, this fails the
/// build instead.
const HTS1_REPLY_BYTES: usize =
    helios_protocol::translation_session::HELIOS_HTS1_REPLY_SIZE as usize;
const _: () = assert!(
    HTS1_REPLY_BYTES
        == core::mem::size_of::<
            helios_protocol::translation_session::HeliosTranslationSessionReplyV1,
        >()
);

/// The driver-wide monotone source of nonzero HTS1 session generations.
///
/// Driver-global rather than per-adapter: the doc only requires "exact nonzero",
/// and one counter makes two sessions on two adapters distinguishable too, which
/// a per-adapter counter would not.
static SESSION_GENERATION: SpinLock<model::SessionGenerationSource> =
    SpinLock::new(model::SessionGenerationSource::new());

// ── The session object ───────────────────────────────────────────────────────

/// One live HTS1 session.
///
/// Owned jointly by the raw KMT device that created its HVC1 control context and
/// by every HQA1-attached outer context; freed when the last reference drops
/// (§17.6: "final session destruction waits for raw/outer context, endpoint,
/// C51/HQC1, host job, and reply references").
pub(crate) struct SessionObject {
    /// The short session lock of §10.7:2041-2043 — checkout/publication only,
    /// never held across Render, host completion, decode, or an event wait.
    model: SpinLock<model::TranslationSession>,
    /// The exact `hKmdProcess` object the raw device recorded. Identity is this
    /// pointer, never a PID.
    process: usize,
    /// The adapter the raw device belongs to. §10.4:1218-1220 requires a raw and
    /// a runtime KMD device to have pointer-identical `hKmdProcess` **and**
    /// adapter object — "a mandatory observed target gate rather than an
    /// inference from a PID" — so attach compares both.
    adapter: *const AdapterContext,
    /// Exact raw KMT device that owns the host context in the canonical owner
    /// table. Never serialized, logged, or used for discovery.
    owner: DeviceOwner,
    /// Stable fixed endpoint objects.  Every HQA1 context stores one direct
    /// pointer into this array while its strong session ref keeps the array
    /// alive; Submit never re-discovers an endpoint by generation/capability.
    endpoints: [SessionEndpointObject; model::ENDPOINT_SLOTS],
    /// Next endpoint assigned to an HVC1 queue context.  Endpoints are never
    /// recycled within a session: a late host fence can therefore never name a
    /// newly-created queue merely because its numeric ring was reused.
    next_queue_endpoint: AtomicU32,
    /// K11's one host context/object namespace and exact rundown edge.
    transport: SessionTransport,
    /// Monotonic, session-local HVR1 snapshot source.
    snapshot_generations: SpinLock<model::SessionGenerationSource>,
    refs: AtomicU32,
}

/// One direct fixed endpoint reference retained by an HQA1 context.
pub(crate) struct SessionEndpointObject {
    endpoint_id: u32,
    /// Private `INFO_RING_IDX` reserved with this endpoint before INIT can
    /// publish its capacity. Ring zero remains the session's control timeline.
    ring_index: u32,
}

impl SessionEndpointObject {
    pub(crate) const fn endpoint_id(&self) -> u32 {
        self.endpoint_id
    }

    pub(crate) const fn ring_index(&self) -> u32 {
        self.ring_index
    }
}

// SAFETY: `model` is reachable only through the `SpinLock`, and every other field
// is written once at construction and read-only afterwards. `process`/`adapter`
// are compared for identity; adapter is dereferenced only by PASSIVE teardown
// while the owning WDDM adapter is still live.
unsafe impl Send for SessionObject {}
// SAFETY: as above.
unsafe impl Sync for SessionObject {}

impl SessionObject {
    fn acquire(&self) {
        self.refs.fetch_add(1, Ordering::Relaxed);
    }

    /// Drop one reference; free at zero.
    ///
    /// # Safety
    /// `session` must be a pointer this module minted and the caller must own the
    /// reference it is releasing.
    unsafe fn release(session: *mut SessionObject) {
        // SAFETY: per this function's contract the pointer is live for the
        // duration of this call, because the caller still owns a reference.
        let obj = unsafe { &*session };
        if obj.refs.fetch_sub(1, Ordering::AcqRel) == 1 {
            TS_SESSION_FREED.fetch_add(1, Ordering::Relaxed);
            // SAFETY: the count reached zero, so no other owner can observe it.
            drop(unsafe { Box::from_raw(session) });
        }
    }

    fn begin_draining_once(&self) {
        let mut session = self.model.lock();
        if session.phase() != model::SessionPhase::Draining {
            session.begin_draining();
            TS_DRAINED.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn teardown(&self, passive: crate::irql::PassiveLevel) {
        self.begin_draining_once();
        // SAFETY: the adapter outlives every WDDM device/context/allocation
        // object associated with it. All callers are PASSIVE lifetime DDIs.
        let Some(adapter) = (unsafe { self.adapter.as_ref() }) else {
            return;
        };
        self.transport.teardown(passive, adapter, self.owner);
    }
}

// ── The ProcessContext's bounded session list ────────────────────────────────

/// Live HTS1 sessions in one KMD `ProcessContext`.
///
/// ⛔ Not a discovery table. Invariant 10 permits exactly one edge: it is
/// consulted **only** by `DxgkDdiCreateContext`, keyed by the unpredictable
/// capability plus session generation, and every admitted context then holds a
/// direct strong reference. Submit, Present, allocation open and display never
/// reach it. The array is fixed at the protocol cap so nothing allocates under
/// the spinlock.
pub(crate) struct ProcessSessionList {
    slots: SpinLock<[Option<NonNull<SessionObject>>; SESSION_SLOTS]>,
    ledger: SpinLock<model::ProcessSessionLedger>,
}

const SESSION_SLOTS: usize =
    helios_protocol::translation_session::HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS as usize;

// SAFETY: both fields are reachable only through their `SpinLock`s, which
// serialize every access behind a `KSPIN_LOCK`.
unsafe impl Send for ProcessSessionList {}
// SAFETY: as above.
unsafe impl Sync for ProcessSessionList {}

impl ProcessSessionList {
    pub(crate) const fn new() -> Self {
        Self {
            slots: SpinLock::new([None; SESSION_SLOTS]),
            ledger: SpinLock::new(model::ProcessSessionLedger::new()),
        }
    }

    fn insert(&self, session: NonNull<SessionObject>) -> bool {
        if self.ledger.lock().admit().is_err() {
            TS_SESSION_LIST_FULL.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mut slots = self.slots.lock();
        let mut i = 0;
        while i < SESSION_SLOTS {
            if let Some(slot) = slots.get_mut(i) {
                if slot.is_none() {
                    *slot = Some(session);
                    return true;
                }
            }
            i += 1;
        }
        // The ledger admitted but no slot is free: the two disagree, which is our
        // own accounting bug. Give the ledger entry back rather than leaking it.
        drop(slots);
        let _ = self.ledger.lock().release();
        TS_SESSION_LIST_FULL.fetch_add(1, Ordering::Relaxed);
        false
    }

    fn remove(&self, session: NonNull<SessionObject>) -> bool {
        let mut slots = self.slots.lock();
        let mut i = 0;
        while i < SESSION_SLOTS {
            if slots.get(i) == Some(&Some(session)) {
                if let Some(slot) = slots.get_mut(i) {
                    *slot = None;
                }
                drop(slots);
                let _ = self.ledger.lock().release();
                return true;
            }
            i += 1;
        }
        false
    }

    /// The one permitted search: create-context, keyed by session generation plus
    /// the unpredictable capability. Returns a session with one reference already
    /// taken, so it cannot be freed between the lookup and the attach.
    fn acquire_by_key(
        &self,
        process: usize,
        adapter: *const AdapterContext,
        session_generation: u64,
        capability: HeliosSessionCapability,
    ) -> Option<NonNull<SessionObject>> {
        if session_generation == 0 || capability.is_invalid() {
            return None;
        }
        let slots = self.slots.lock();
        let mut found = None;
        let mut i = 0;
        while i < SESSION_SLOTS {
            if let Some(ptr) = slots.get(i).copied().flatten() {
                // SAFETY: a slot holds a pointer this module minted, and removal
                // happens under this same lock before the last release.
                let obj = unsafe { ptr.as_ref() };
                // §10.4:1218-1220: pointer-identical `hKmdProcess` AND adapter
                // object. Both are compared; neither is inferred from a PID.
                if obj.process == process && core::ptr::eq(obj.adapter, adapter) {
                    let model = obj.model.lock();
                    // ⛔ The capability is part of the KEY, not just of the later
                    // validation (§10.4:1215-1217: "The key is the unpredictable
                    // capability plus session generation"). Comparing only the
                    // generation would let a packet with a forged capability
                    // *find* the session and be refused by `attach` instead —
                    // the same outcome, but counted as an attach refusal rather
                    // than as no such session, and one lookup closer to the
                    // object than the doc allows.
                    let matches = model.session_generation() == session_generation
                        && model.phase() == model::SessionPhase::Live
                        && model.capability_matches(capability);
                    drop(model);
                    if matches {
                        obj.acquire();
                        found = Some(ptr);
                        break;
                    }
                }
            }
            i += 1;
        }
        drop(slots);
        found
    }

    /// Move every live session to `Draining`. Reset, removal, and process
    /// teardown all land here; §14 requires capability invalidation to precede
    /// the device-lost wakeup, which is what `begin_draining` does first.
    pub(crate) fn drain_all(&self) {
        // Take temporary refs into fixed stack storage, then drop the
        // ProcessContext list lock before any host roundtrip or event wait.
        let mut pending = [None; SESSION_SLOTS];
        let slots = self.slots.lock();
        let mut i = 0;
        while i < SESSION_SLOTS {
            if let Some(ptr) = slots.get(i).copied().flatten() {
                // SAFETY: the device/list reference is live under this lock.
                unsafe { ptr.as_ref() }.acquire();
                pending[i] = Some(ptr);
            }
            i += 1;
        }
        drop(slots);
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        for ptr in pending.into_iter().flatten() {
            let obj = unsafe { ptr.as_ref() };
            obj.teardown(passive);
            unsafe { SessionObject::release(ptr.as_ptr()) };
        }
    }
}

// ── `DxgkDdiCreateContext` private-data dispatch ─────────────────────────────

/// Which create-context arm the private driver data selects.
///
/// Recognition is by exact size **and** magic, and anything else falls through
/// to [`Self::Legacy`] rather than failing the context. The live desktop's D3D
/// and CDD contexts pass no private data at all, and a context this driver
/// refused would take DWM down; an HVC1 whose magic is wrong instead fails the
/// ICD's own `DXGK_CONTEXTINFO` minima check one step later, which is loud
/// enough and cannot regress a running desktop.
pub(crate) enum ContextRequest {
    /// The existing D3D-runtime / CDD context. Its `DXGK_CONTEXTINFO` is
    /// unchanged, including `DmaBufferSegmentSet = 1` — a zero segment set makes
    /// dxgmms2 null-deref in `VidMmInitDmaPool` (measured; `device.rs`), and
    /// §10.7:1981 keeps segment 1 as "the only nonzero `DmaBufferSegmentSet`
    /// choice for existing D3D runtime contexts, while HVC1 selects zero".
    Legacy,
    /// The one HVC1 control context of a raw KMT device: a provisional session.
    HeliosControl,
    /// An HVC1 queue context on the same raw device, holding its own reference
    /// to that device's session.
    HeliosQueue {
        session: NonNull<SessionObject>,
        /// Direct fixed endpoint selected once at context creation.  The queue
        /// family/index in HVC1 remain diagnostics and are never looked up.
        endpoint: NonNull<SessionEndpointObject>,
    },
    /// An HQA1 outer-context attach onto an existing session.
    HeliosAttach {
        session: NonNull<SessionObject>,
        /// The live session generation, for K6's HOS1 gate. Read here, under the
        /// same lock that admitted the attach, so the context cannot be built
        /// against a generation the session no longer has.
        session_generation: u64,
        context_generation: u64,
        endpoint: NonNull<SessionEndpointObject>,
        /// Which HQA1 arm — HOS1 exists only on the D3D12 virtual one.
        kind: helios_protocol::translation_session::HeliosOuterContextKind,
    },
}

/// Classify one `DxgkDdiCreateContext` call and, for the two Helios arms, do the
/// session-side work.
///
/// # Safety
/// `private` / `private_size` must be dxgkrnl's create-context private buffer and
/// its authoritative length. Nothing here writes through the pointer.
pub(crate) unsafe fn classify_context(
    private: *const core::ffi::c_void,
    private_size: u32,
    context_flags: u32,
    process: usize,
    adapter: *const AdapterContext,
    raw_device: usize,
    device_session: &SpinLock<Option<NonNull<SessionObject>>>,
    process_list: Option<&ProcessSessionList>,
) -> Result<ContextRequest, NTSTATUS> {
    if private.is_null() || private_size == 0 {
        return Ok(ContextRequest::Legacy);
    }
    // Copy the caller's bytes out before validating, so nothing downstream can
    // observe a value that changed between check and use.
    if private_size as usize == HELIOS_HVC1_SIZE as usize {
        let mut bytes = [0u8; HELIOS_HVC1_SIZE as usize];
        // SAFETY: non-null and the length is exactly the record size.
        unsafe {
            core::ptr::copy_nonoverlapping(
                private as *const u8,
                bytes.as_mut_ptr(),
                HELIOS_HVC1_SIZE as usize,
            )
        };
        if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == HELIOS_HVC1_MAGIC {
            return admit_hvc1(
                &bytes,
                context_flags,
                process,
                adapter,
                raw_device,
                device_session,
                process_list,
            );
        }
    }
    if private_size as usize == HELIOS_HQA1_SIZE as usize {
        let mut bytes = [0u8; HELIOS_HQA1_SIZE as usize];
        // SAFETY: non-null and the length is exactly the record size.
        unsafe {
            core::ptr::copy_nonoverlapping(
                private as *const u8,
                bytes.as_mut_ptr(),
                HELIOS_HQA1_SIZE as usize,
            )
        };
        if u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) == HELIOS_HQA1_MAGIC {
            return admit_hqa1(&bytes, process, adapter, process_list);
        }
    }
    TS_PDD_UNKNOWN.fetch_add(1, Ordering::Relaxed);
    Ok(ContextRequest::Legacy)
}

fn admit_hvc1(
    bytes: &[u8; HELIOS_HVC1_SIZE as usize],
    context_flags: u32,
    process: usize,
    adapter: *const AdapterContext,
    raw_device: usize,
    device_session: &SpinLock<Option<NonNull<SessionObject>>>,
    process_list: Option<&ProcessSessionList>,
) -> Result<ContextRequest, NTSTATUS> {
    let record = match parse_hvc1_private_data(bytes) {
        Ok(record) => record,
        Err(reason) => {
            bump_with_code(&TS_HVC1_REJECT, reason.code());
            return Err(STATUS_INVALID_PARAMETER);
        }
    };
    let class = match record.validate(HELIOS_PACKAGE_GENERATION, HELIOS_NATIVE_RENDER_CAPSET) {
        Ok(class) => class,
        Err(reason) => {
            bump_with_code(&TS_HVC1_REJECT, reason.code());
            return Err(STATUS_INVALID_PARAMETER);
        }
    };
    if let Err(reason) = hvc1_admit_context_flags(context_flags) {
        bump_with_code(&TS_HVC1_FLAGS_REJECT, reason.code());
        return Err(STATUS_NOT_SUPPORTED);
    }
    if class == Hvc1ContextClass::Queue {
        // K6 (`ddi/native_render.rs`) owns the queue context. It is created on
        // the SAME raw KMT device as the control context
        // (`vn_helios_translation_session.h:71`), so the device's session cell is
        // where its session comes from — there is no lookup, and a queue context
        // on a device with no control context has nothing to belong to.
        // ⛔ THE CELL LOCK IS HELD ACROSS THE DEREFERENCE AND THE ACQUIRE, and
        // the first version of this arm copied the pointer out and dropped the
        // guard first. `dxgkddi_destroy_context` for this device's control
        // context clears the cell under this same lock and then releases the
        // last reference, freeing the object — so a queue create racing it would
        // have taken a spinlock inside, and incremented a refcount inside, freed
        // nonpaged pool. `ProcessSessionList::acquire_by_key` in this file is the
        // correct shape and says why: acquire "so it cannot be freed between the
        // lookup and the attach".
        let cell = device_session.lock();
        let Some(session) = *cell else {
            drop(cell);
            crate::ddi::native_render::NR2_QUEUE_CTX_REJECT.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        };
        // SAFETY: the device's own reference keeps the object alive for as long
        // as the cell is set, and the cell's lock is held for this whole block —
        // so the release path cannot be between the clear and the free here.
        let obj = unsafe { session.as_ref() };
        let endpoint_capacity = {
            let model = obj.model.lock();
            if model.phase() != model::SessionPhase::Live {
                drop(model);
                drop(cell);
                crate::ddi::native_render::NR2_QUEUE_CTX_REJECT.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_DEVICE_REQUEST);
            }
            model.endpoint_capacity()
        };
        // Queue endpoints exist only after the real K11 INIT published their
        // exact capacity.  The bounded update never wraps and never consumes a
        // slot on refusal; endpoint ordinals are not recycled in this session.
        let endpoint_index = match obj.next_queue_endpoint.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |next| (next < endpoint_capacity).then_some(next + 1),
        ) {
            Ok(index) => index as usize,
            Err(_) => {
                crate::ddi::native_render::NR2_QUEUE_CTX_REJECT.fetch_add(1, Ordering::Relaxed);
                drop(cell);
                return Err(STATUS_NOT_SUPPORTED);
            }
        };
        let Some(endpoint) = obj.endpoints.get(endpoint_index) else {
            crate::ddi::native_render::NR2_QUEUE_CTX_REJECT.fetch_add(1, Ordering::Relaxed);
            drop(cell);
            return Err(STATUS_NOT_SUPPORTED);
        };
        obj.acquire();
        drop(cell);
        crate::ddi::native_render::NR2_QUEUE_CTX.fetch_add(1, Ordering::Relaxed);
        return Ok(ContextRequest::HeliosQueue {
            session,
            endpoint: NonNull::from(endpoint),
        });
    }

    let Some(list) = process_list else {
        TS_NO_PROCESS.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    // "The raw KMD device permits exactly one control context and one HTS1
    // session" (§10.7:1720-1721).
    if device_session.lock().is_some() {
        TS_SECOND_CONTROL_CTX.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }

    let Some(session) = new_session(process, adapter, raw_device) else {
        TS_ALLOC_FAILED.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_NO_MEMORY);
    };
    if !list.insert(session) {
        // SAFETY: the only reference is the one `new_session` minted.
        unsafe { SessionObject::release(session.as_ptr()) };
        return Err(STATUS_INSUFFICIENT_RESOURCES);
    }
    // The device's own reference. `insert` does not take one: the list holds a
    // borrow of the device's, and `remove` runs before the device releases.
    *device_session.lock() = Some(session);
    TS_SESSION_CREATED.fetch_add(1, Ordering::Relaxed);
    Ok(ContextRequest::HeliosControl)
}

/// Build one provisional session on the heap.
///
/// `#[inline(never)]`: the model carries a 64-entry endpoint array, and the T3
/// lesson (`adapter/mod.rs:283-312`) is that a multi-KiB value built inline on a
/// DDI frame is how this driver overflowed a 24-KiB kernel stack twice.
#[inline(never)]
fn new_session(
    process: usize,
    adapter: *const AdapterContext,
    raw_device: usize,
) -> Option<NonNull<SessionObject>> {
    let owner = DeviceOwner::new(raw_device)?;
    let obj = Box::new(SessionObject {
        model: SpinLock::new(model::TranslationSession::new_provisional(
            HELIOS_PACKAGE_GENERATION,
            HELIOS_NATIVE_RENDER_CAPSET,
        )),
        process,
        adapter,
        owner,
        endpoints: core::array::from_fn(|index| SessionEndpointObject {
            endpoint_id: index as u32 + 1,
            ring_index: index as u32 + 1,
        }),
        next_queue_endpoint: AtomicU32::new(0),
        transport: SessionTransport::new()?,
        snapshot_generations: SpinLock::new(model::SessionGenerationSource::new()),
        refs: AtomicU32::new(1),
    });
    let ptr = NonNull::new(Box::into_raw(obj))?;
    // SAFETY: the Box has reached its final address and is not published until
    // this function returns it to the device/session list.
    unsafe { ptr.as_ref().transport.init_event() };
    Some(ptr)
}

fn admit_hqa1(
    bytes: &[u8; HELIOS_HQA1_SIZE as usize],
    process: usize,
    adapter: *const AdapterContext,
    process_list: Option<&ProcessSessionList>,
) -> Result<ContextRequest, NTSTATUS> {
    let packet = match parse_create_context_private_data(bytes) {
        Ok(packet) => packet,
        Err(_) => {
            TS_ATTACH_REJECT.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        }
    };
    let Some(list) = process_list else {
        TS_NO_PROCESS.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    // The one permitted search, and it is keyed on the unpredictable capability:
    // a wrong pair finds nothing rather than finding the wrong session.
    let Some(session) = list.acquire_by_key(
        process,
        adapter,
        packet.session_generation,
        packet.capability(),
    ) else {
        TS_ATTACH_NO_SESSION.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    // SAFETY: `acquire_by_key` returned it with a reference held, so it is live.
    let obj = unsafe { session.as_ref() };
    // A capability from a physically retired transport cannot attach to a
    // successor merely because its CSPRNG bytes still match. The K11 rundown
    // and canonical pair-use guards stay held through the model attach, closing
    // the reset race without turning the process list into a host-operation
    // lock or adding submit-time discovery.
    let Some(adapter_ref) = (unsafe { adapter.as_ref() }) else {
        unsafe { SessionObject::release(session.as_ptr()) };
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    let Some(admission) =
        obj.transport
            .with_live_on_current_transport(adapter_ref, obj.owner, || {
                obj.model.lock().attach(&packet)
            })
    else {
        crate::ddi::session_transport::K11_STALE_TRANSPORT.fetch_add(1, Ordering::Relaxed);
        obj.teardown(unsafe { crate::irql::PassiveLevel::assume() });
        unsafe { SessionObject::release(session.as_ptr()) };
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    match admission {
        Ok(admission) => {
            let endpoint_index = (admission.endpoint_id - 1) as usize;
            let Some(endpoint) = obj.endpoints.get(endpoint_index) else {
                TS_ATTACH_REJECT.fetch_add(1, Ordering::Relaxed);
                unsafe { SessionObject::release(session.as_ptr()) };
                return Err(STATUS_INVALID_DEVICE_REQUEST);
            };
            // The pure session reserves the complete endpoint/ring namespace
            // before it becomes Live. The direct platform endpoint must project
            // that exact private mapping; neither numeric value is returned in
            // HQA1 or rediscovered when a later Submit arrives.
            if obj.model.lock().ring_index(admission.endpoint_id) != Ok(endpoint.ring_index()) {
                let _ = obj.model.lock().detach(admission.context_generation);
                TS_ATTACH_REJECT.fetch_add(1, Ordering::Relaxed);
                unsafe { SessionObject::release(session.as_ptr()) };
                return Err(STATUS_INVALID_DEVICE_REQUEST);
            }
            TS_ATTACH_OK.fetch_add(1, Ordering::Relaxed);
            Ok(ContextRequest::HeliosAttach {
                session,
                session_generation: obj.model.lock().session_generation(),
                context_generation: admission.context_generation,
                endpoint: NonNull::from(endpoint),
                kind: admission.kind,
            })
        }
        Err(_) => {
            TS_ATTACH_REJECT.fetch_add(1, Ordering::Relaxed);
            // SAFETY: releasing the reference `acquire_by_key` took.
            unsafe { SessionObject::release(session.as_ptr()) };
            Err(STATUS_INVALID_DEVICE_REQUEST)
        }
    }
}

// ── Teardown ─────────────────────────────────────────────────────────────────

/// The control context of a raw device went away: unregister the session and
/// release the device's reference.
///
/// # Safety
/// `session` must be a pointer this module minted and the caller must own the
/// device reference it is releasing.
pub(crate) unsafe fn release_device_session(
    session: NonNull<SessionObject>,
    process_list: Option<&ProcessSessionList>,
) {
    // SAFETY: per this function's contract the caller still holds a reference.
    let obj = unsafe { session.as_ref() };
    // DestroyContext/DestroyDevice are PASSIVE lifetime DDIs. Revoke host work
    // and destroy the namespace before releasing either the device's or the
    // role-1 open's strong reference.
    obj.teardown(unsafe { crate::irql::PassiveLevel::assume() });
    if let Some(list) = process_list {
        list.remove(session);
    }
    // SAFETY: releasing the device's own reference.
    unsafe { SessionObject::release(session.as_ptr()) };
}

/// The exact role-1 device-specific open is closing.  Revoke/drain/destroy the
/// host namespace while its canonical allocation and K2a MDL are still live,
/// then release the strong reference that open took at bind time.
///
/// # Safety
/// `session` is the reference returned by `bind_reply_pool`; `allocation` is
/// the same open object's canonical allocation pointer.
pub(crate) unsafe fn close_reply_pool_binding(session: NonNull<SessionObject>, allocation: usize) {
    let obj = unsafe { session.as_ref() };
    if !obj.transport.binding_matches(allocation) {
        TS_POOL_REJECT.fetch_add(1, Ordering::Relaxed);
    }
    // Even an impossible binding mismatch must revoke the exact session before
    // this open releases its pool reference; mismatch is diagnostic, never an
    // excuse to invert teardown order.
    obj.teardown(unsafe { crate::irql::PassiveLevel::assume() });
    if !(unsafe {
        crate::ddi::create_allocation::release_k11_reply_pool_session(allocation, session)
    }) {
        TS_POOL_REJECT.fetch_add(1, Ordering::Relaxed);
    }
    unsafe { SessionObject::release(session.as_ptr()) };
}

/// An HVC1 queue context went away: release the reference it took at create.
///
/// It never registered anything — the session's list entry and the device's
/// reference belong to the control context — so this is a bare release.
///
/// # Safety
/// `session` must be a pointer this module minted and the caller must own the
/// context reference it is releasing.
pub(crate) unsafe fn release_queue_context(session: NonNull<SessionObject>) {
    // SAFETY: per this function's contract the caller still holds a reference.
    unsafe { SessionObject::release(session.as_ptr()) };
}

/// An HQA1-attached outer context went away.
///
/// # Safety
/// `session` must be a pointer this module minted and the caller must own the
/// context reference it is releasing.
pub(crate) unsafe fn release_attached_context(
    session: NonNull<SessionObject>,
    context_generation: u64,
) {
    // SAFETY: per this function's contract the caller still holds a reference.
    let obj = unsafe { session.as_ref() };
    match obj.model.lock().detach(context_generation) {
        Ok(()) => TS_DETACH_OK.fetch_add(1, Ordering::Relaxed),
        Err(_) => TS_DETACH_REJECT.fetch_add(1, Ordering::Relaxed),
    };
    // SAFETY: releasing the context's own reference.
    unsafe { SessionObject::release(session.as_ptr()) };
}

// ── The role-1 reply pool ────────────────────────────────────────────────────

/// Bind the role-1 HVM1 reply pool a raw device just created to its provisional
/// session (§10.4:1205-1206: "KMD binds that allocation directly to the
/// provisional session through the owning raw-device reference; there is no
/// process/session lookup at Render").
///
/// The caller is `DxgkDdiOpenAllocation`, which is the only allocation DDI that
/// carries `hDevice` — `DXGKARG_CREATEALLOCATION` has none, so the create path
/// cannot bind anything to a device at all.
///
/// A refusal does **not** fail the open. The open DDI deliberately has no failure
/// arm and reintroducing one means reintroducing an unwind for the handles it has
/// already published. The failure surfaces at the session's first control Render
/// instead: `ReplyPoolNotBound` when nothing bound at all, and
/// `ControlRenderPoolGenerationStale` when a second pool was created and the
/// Render names its generation rather than the bound one.
pub(crate) fn bind_reply_pool(
    device_session: &SpinLock<Option<NonNull<SessionObject>>>,
    role: Hvm1Role,
    byte_size: u64,
    object_generation: u64,
    canonical_allocation: usize,
) -> Option<NonNull<SessionObject>> {
    let cell = device_session.lock();
    let Some(session) = *cell else {
        // Not a raw KMT device: every ordinary D3D device's HVM1 allocations land
        // here and are simply not this session's pool. Not counted.
        return None;
    };
    // SAFETY: the device holds a reference for as long as the field is set.
    let obj = unsafe { session.as_ref() };
    // The canonical allocation admits one exact SessionObject, not merely one
    // context id. This closes the cross-session shared-pool case before either
    // the model or host-transport binding mutates.
    if !(unsafe {
        crate::ddi::create_allocation::claim_k11_reply_pool_session(canonical_allocation, session)
    }) {
        drop(cell);
        TS_POOL_REJECT.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let transport_bound = obj.transport.bind_reply_pool(canonical_allocation);
    let model_bound = obj
        .model
        .lock()
        .bind_reply_pool(role, byte_size, object_generation)
        .is_ok();
    if transport_bound && model_bound {
        // The device-specific role-1 open owns this reference until
        // CloseAllocation, which is the exact backing lifetime K11 needs.
        obj.acquire();
        drop(cell);
        TS_POOL_BOUND.fetch_add(1, Ordering::Relaxed);
        return Some(session);
    }
    // Give the canonical allocation claim back. The two state machines should
    // either both advance or neither; an asymmetric advance cannot be rolled
    // back and therefore drains this exact session.
    let _ = unsafe {
        crate::ddi::create_allocation::release_k11_reply_pool_session(canonical_allocation, session)
    };
    if transport_bound != model_bound {
        obj.begin_draining_once();
    }
    drop(cell);
    TS_POOL_REJECT.fetch_add(1, Ordering::Relaxed);
    None
}

// ── The INIT seam ────────────────────────────────────────────────────────────

/// Complete the finite HTS1 `INIT` and produce the reply bytes.
///
/// INIT arrives through K6's HNR2 control Render as exactly 32 copied HTS1
/// bytes. K11 creates a private stock-Venus context and `VkInstance`, validates
/// the real finite host reply, then this function publishes the 56-byte HTS1
/// reply behind an HVR1 header in the exact checked-out role-1 slot. No
/// generation, capability, or endpoint capacity becomes live before that host
/// initialization and publication sequence succeeds.
fn reject_session_init(
    obj: &SessionObject,
    status: NTSTATUS,
) -> Result<[u8; HTS1_REPLY_BYTES], NTSTATUS> {
    // A classified INIT is one-shot: close model admission immediately so no
    // later packet can reuse a half-failed session. The Render caller still
    // owns one checked-out reply slot, so it aborts that exact ownership before
    // `finish_failed_session_init` drains and destroys the host namespace.
    obj.begin_draining_once();
    TS_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
    Err(status)
}

pub(crate) fn session_init(
    session: NonNull<SessionObject>,
    request: &[u8],
    reply_offset: u64,
    reply_capacity_bytes: u64,
    slot_generation: u64,
    batch_token: u64,
) -> Result<[u8; HTS1_REPLY_BYTES], NTSTATUS> {
    use helios_protocol::translation_session::HeliosTranslationSessionInitV1;

    // SAFETY: the caller holds a reference to the session for this call.
    let obj = unsafe { session.as_ref() };
    if request.len() != core::mem::size_of::<HeliosTranslationSessionInitV1>() {
        return reject_session_init(obj, STATUS_INVALID_PARAMETER);
    }
    let Ok(record) = bytemuck::try_pod_read_unaligned::<HeliosTranslationSessionInitV1>(request)
    else {
        return reject_session_init(obj, STATUS_INVALID_PARAMETER);
    };
    // End the short model guard before any rejection tears the session down.
    // A match directly on `obj.model.lock()` could retain its temporary guard
    // through the error arm and deadlock `begin_draining_once`.
    let init_admission = { obj.model.lock().admit_init(&record) };
    let requested = match init_admission {
        Ok(requested) => requested,
        Err(_) => {
            return reject_session_init(obj, STATUS_INVALID_PARAMETER);
        }
    };
    let pool_generation = { obj.model.lock().reply_pool_generation() };
    let Some(expected_pool_generation) = pool_generation else {
        return reject_session_init(obj, STATUS_DEVICE_NOT_READY);
    };
    let Some(adapter) = (unsafe { obj.adapter.as_ref() }) else {
        return reject_session_init(obj, STATUS_DEVICE_NOT_READY);
    };
    let passive = unsafe { crate::irql::PassiveLevel::assume() };
    let initialized = obj.transport.initialize(
        passive,
        adapter,
        obj.owner,
        reply_offset,
        reply_capacity_bytes,
        expected_pool_generation,
        HTS1_REPLY_BYTES as u64,
        |facts, host| {
            let Some(capability) = mint_capability() else {
                TS_NO_CSPRNG.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_DEVICE_NOT_READY);
            };
            let generation = SESSION_GENERATION
                .lock()
                .mint()
                .map_err(|_| STATUS_INSUFFICIENT_RESOURCES)?;
            let snapshot_generation = obj
                .snapshot_generations
                .lock()
                .mint()
                .map_err(|_| STATUS_INSUFFICIENT_RESOURCES)?;
            // K11 grants the finite model-admitted capacity only after the
            // distinct host context, fixed private SHM reply target, and real
            // host vkCreateInstance reply have all completed. K2a has not been
            // attached to the renderer; it is only the HVR1 publication below.
            let reply = obj
                .model
                .lock()
                .complete_init(requested, requested, generation, capability)
                .map_err(|_| STATUS_DEVICE_NOT_READY)?;
            let hvr1 = helios_protocol::native_render::HeliosVenusReplyV1 {
                magic: helios_protocol::native_render::HELIOS_HVR1_MAGIC,
                version: helios_protocol::native_render::HELIOS_HVR1_VERSION,
                header_size: helios_protocol::native_render::HELIOS_HVR1_HEADER_SIZE,
                package_generation: HELIOS_PACKAGE_GENERATION,
                session_generation: generation,
                slot_generation,
                batch_token,
                snapshot_generation,
                opcode: host.opcode,
                status: host.status,
                total_bytes: HTS1_REPLY_BYTES as u64,
                chunk_offset: 0,
                chunk_bytes: HTS1_REPLY_BYTES as u32,
                flags: helios_protocol::native_render::HELIOS_HVR1_FLAG_FINAL,
            };
            let mut out = [0u8; HTS1_REPLY_BYTES];
            out.copy_from_slice(bytemuck::bytes_of(&reply));
            crate::ddi::session_transport::SessionTransport::publish_hvr1(
                facts,
                reply_offset,
                reply_capacity_bytes,
                &hvr1,
                &out,
            )?;
            Ok(out)
        },
    );
    match initialized {
        Ok(reply) => {
            TS_INIT_OK.fetch_add(1, Ordering::Relaxed);
            Ok(reply)
        }
        Err(status) => {
            // No retry may reuse a session whose host initialization began.
            // This also invalidates a model capability if a publication
            // invariant ever failed after `complete_init`.
            reject_session_init(obj, status)
        }
    }
}

// ── K6's window onto the session ─────────────────────────────────────────────
//
// `SessionObject` stays private to this file: K6 holds a `NonNull` and reaches
// the model only through these three, so the session lock's discipline
// (§10.7:2041 — checkout and publication only, never held across a Render, a
// host completion, a decode, or a wait) has exactly one owner.

/// The generation of the role-1 reply pool bound to this session, for K6's
/// "does the one listed allocation belong to THIS session" test.
///
/// # Safety
/// `session` must be live for the call — K6 holds it through the context object
/// that took a reference at create.
pub(crate) fn reply_pool_generation(session: NonNull<SessionObject>) -> Option<u64> {
    // SAFETY: per the contract above.
    let obj = unsafe { session.as_ref() };
    let generation = obj.model.lock().reply_pool_generation();
    generation
}

/// Exact live HTS1 generation carried by a direct context/session edge.
pub(crate) fn execution_session_generation(session: NonNull<SessionObject>) -> Option<u64> {
    let obj = unsafe { session.as_ref() };
    let model = obj.model.lock();
    (model.phase() == model::SessionPhase::Live)
        .then(|| model.session_generation())
        .filter(|generation| *generation != 0)
}

/// Mint the one session-local snapshot generation used to publish a bounded
/// generated reply from the nonzero-ring executor.  The caller already owns a
/// direct session rundown operation; this function performs no discovery and
/// refuses a non-live session or exhausted generation space.
pub(crate) fn mint_execution_snapshot_generation(session: NonNull<SessionObject>) -> Option<u64> {
    let obj = unsafe { session.as_ref() };
    if obj.model.lock().phase() != model::SessionPhase::Live {
        return None;
    }
    obj.snapshot_generations.lock().mint().ok()
}

pub(crate) fn execute_generated_control(
    session: NonNull<SessionObject>,
    facts: crate::ddi::create_allocation::K11ReplyPoolFacts,
    payload: &[u8],
    reply_offset: u64,
    reply_capacity: u64,
    raw_reply_bytes: u64,
    slot_generation: u64,
    batch_token: u64,
    expected_opcode: u32,
) -> Result<(), NTSTATUS> {
    let obj = unsafe { session.as_ref() };
    if obj.model.lock().phase() != model::SessionPhase::Live {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }
    let adapter = unsafe { obj.adapter.as_ref() }.ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
    let snapshot_generation = obj
        .snapshot_generations
        .lock()
        .mint()
        .map_err(|_| STATUS_INSUFFICIENT_RESOURCES)?;
    let raw_offset = reply_offset
        .checked_add(helios_protocol::native_render::HELIOS_HVR1_HEADER_SIZE as u64)
        .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
    obj.transport.execute_generated_control(
        unsafe { crate::irql::PassiveLevel::assume() },
        adapter,
        obj.owner,
        facts,
        payload,
        raw_offset,
        raw_reply_bytes,
        expected_opcode,
    )?;
    let hvr1 = helios_protocol::native_render::HeliosVenusReplyV1 {
        magic: helios_protocol::native_render::HELIOS_HVR1_MAGIC,
        version: helios_protocol::native_render::HELIOS_HVR1_VERSION,
        header_size: helios_protocol::native_render::HELIOS_HVR1_HEADER_SIZE,
        package_generation: HELIOS_PACKAGE_GENERATION,
        session_generation: obj.model.lock().session_generation(),
        slot_generation,
        batch_token,
        snapshot_generation,
        opcode: expected_opcode,
        status: 0,
        total_bytes: raw_reply_bytes,
        chunk_offset: 0,
        chunk_bytes: u32::try_from(raw_reply_bytes).map_err(|_| STATUS_INVALID_DEVICE_REQUEST)?,
        flags: helios_protocol::native_render::HELIOS_HVR1_FLAG_FINAL,
    };
    crate::ddi::session_transport::SessionTransport::publish_hvr1_existing_payload(
        facts,
        reply_offset,
        reply_capacity,
        &hvr1,
        raw_reply_bytes,
    )
}

pub(crate) fn execute_control_no_reply(
    session: NonNull<SessionObject>,
    payload: &[u8],
) -> Result<(), NTSTATUS> {
    let obj = unsafe { session.as_ref() };
    if obj.model.lock().phase() != model::SessionPhase::Live {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }
    let adapter = unsafe { obj.adapter.as_ref() }.ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
    obj.transport.execute_control_no_reply(
        unsafe { crate::irql::PassiveLevel::assume() },
        adapter,
        obj.owner,
        payload,
    )
}

/// Retain the raw device's exact live session for one roles 2-4 HVM1 ordinary
/// open.  The device cell is the only source; there is no process/global
/// search, and a provisional or draining session is not an execution owner.
pub(crate) fn retain_execution_session(
    device_session: &SpinLock<Option<NonNull<SessionObject>>>,
) -> Option<NonNull<SessionObject>> {
    let cell = device_session.lock();
    let session = (*cell)?;
    let obj = unsafe { session.as_ref() };
    if obj.model.lock().phase() != model::SessionPhase::Live {
        return None;
    }
    obj.acquire();
    Some(session)
}

/// Retain the exact live session already carried by an HQA1 context.  This is
/// a direct-object edge, not the process-list capability search used only at
/// context creation.  Allocation opens use the returned reference to keep
/// their one-time host resource attachment alive through reverse teardown.
pub(crate) fn retain_direct_execution_session(
    session: NonNull<SessionObject>,
) -> Option<NonNull<SessionObject>> {
    let obj = unsafe { session.as_ref() };
    if obj.model.lock().phase() != model::SessionPhase::Live {
        return None;
    }
    obj.acquire();
    Some(session)
}

/// Release the strong reference returned by [`retain_execution_session`].
///
/// # Safety
/// The caller must own exactly that reference.
pub(crate) unsafe fn release_execution_session(session: NonNull<SessionObject>) {
    unsafe { SessionObject::release(session.as_ptr()) };
}

pub(crate) fn attach_execution_resource(
    session: NonNull<SessionObject>,
    passive: PassiveLevel,
    resource_id: u32,
    transport_instance: u64,
) -> Result<u32, NTSTATUS> {
    let obj = unsafe { session.as_ref() };
    if obj.model.lock().phase() != model::SessionPhase::Live {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }
    let adapter = unsafe { obj.adapter.as_ref() }.ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
    obj.transport.attach_execution_resource(
        passive,
        adapter,
        obj.owner,
        resource_id,
        transport_instance,
    )
}

pub(crate) fn detach_execution_resource(
    session: NonNull<SessionObject>,
    passive: PassiveLevel,
    resource_id: u32,
) {
    let obj = unsafe { session.as_ref() };
    if let Some(adapter) = unsafe { obj.adapter.as_ref() } {
        obj.transport
            .detach_execution_resource(passive, adapter, resource_id);
    }
}

/// Acquire the direct session/context rundown carried by an async host submit.
pub(crate) fn acquire_execution_operation(
    session: NonNull<SessionObject>,
) -> Option<crate::ddi::session_transport::SessionExecutionOperation> {
    let obj = unsafe { session.as_ref() };
    if obj.model.lock().phase() != model::SessionPhase::Live {
        return None;
    }
    let adapter = unsafe { obj.adapter.as_ref() }?;
    obj.transport.acquire_execution(adapter, obj.owner)
}

/// Run one already-host-completed HVC1 admission while this exact session and
/// canonical transport pair remain current.
///
/// This is the SubmitCommand edge, so it performs no discovery and starts no
/// host work. The context supplies its direct strong session reference; the
/// embedded transport retains both rundown guards through `operation`, which
/// admits the exact context-local fence but performs no OS callback. The caller
/// owns the adapter's fixed K11 completion-rundown guard across this operation
/// and the later exact notification, so reset closes and joins the complete
/// interval before abandoning its scheduler epoch. The transport guards end
/// before `DxgkCbSynchronizeExecution`: a same-context Render may be tearing
/// down this already-terminal host session while the OS callback waits for that
/// Render to return. Everything here is a bounded spinlock/atomic projection
/// legal at DISPATCH_LEVEL.
pub(crate) fn with_current_host_submission<R>(
    session: NonNull<SessionObject>,
    operation: impl FnOnce() -> R,
) -> Option<R> {
    // SAFETY: the HVC1 context owns a strong reference for this call.
    let obj = unsafe { session.as_ref() };
    // SAFETY: the adapter outlives every context on it. No pointer or identity
    // leaves this direct-reference validation edge.
    let Some(adapter) = (unsafe { obj.adapter.as_ref() }) else {
        return None;
    };
    obj.transport
        .with_live_on_current_transport(adapter, obj.owner, || {
            // Check the model only after both rundown guards are held. If
            // teardown already published Draining this work is new and must be
            // refused; if teardown begins after this check it waits for the
            // operation guard and therefore follows the exact completion.
            if obj.model.lock().phase() != model::SessionPhase::Live {
                None
            } else {
                Some(operation())
            }
        })
        .flatten()
}

/// Admit one HNR2 control Render and check out its reply slot.
pub(crate) fn admit_control_render(
    session: NonNull<SessionObject>,
    request: &model::ControlRenderRequest,
) -> Result<model::ControlRenderAdmission, NTSTATUS> {
    // SAFETY: as above.
    let obj = unsafe { session.as_ref() };
    let admitted = obj.model.lock().admit_control_render(request);
    match admitted {
        Ok(admission) => Ok(admission),
        Err(refusal) => {
            bump_with_code(&TS_CONTROL_RENDER_REJECT, control_render_code(refusal));
            // `DxgkDdiRender`'s documented return set is narrow; the reason is in
            // `TsCtlRej`, not in the NTSTATUS.
            Err(STATUS_INVALID_PARAMETER)
        }
    }
}

/// Check out one role-1 reply slot for an exact generated allocation command
/// executing on this session's nonzero endpoint.  The caller has already
/// resolved the reply-pool allocation through the Render allocation list; the
/// session model owns the monotone slot-generation transition.
pub(crate) fn admit_execution_reply(
    session: NonNull<SessionObject>,
    request: &model::ExecutionReplyRequest,
) -> Result<model::ControlRenderAdmission, NTSTATUS> {
    let obj = unsafe { session.as_ref() };
    obj.model
        .lock()
        .admit_execution_reply(request)
        .map_err(|refusal| {
            bump_with_code(&TS_CONTROL_RENDER_REJECT, control_render_code(refusal));
            STATUS_INVALID_PARAMETER
        })
}

/// Retire a checked-out slot after its real HVR1 reply was published.
///
/// ⛔ PUBLISH THEN RETIRE. `retire_slot` requires `Published`, and this path is
/// used only after K11 returned success with actual host/HVR1 bytes. A refusal
/// uses [`abort_control_slot`] and therefore never fabricates publication.
pub(crate) fn release_control_slot(
    session: NonNull<SessionObject>,
    slot_index: usize,
    slot_generation: u64,
) {
    // SAFETY: as above.
    let obj = unsafe { session.as_ref() };
    let mut model = obj.model.lock();
    // The K11 host operation is already terminal before publication.  C51 is
    // the ordinary user-visible ordering edge and is not a KMD-owned timeline,
    // so no SubmissionFenceId value is forged into this model slot.
    if model.publish_slot(slot_index, slot_generation, 0).is_ok()
        && model.retire_slot(slot_index, slot_generation).is_ok()
    {
        drop(model);
        TS_SLOT_RELEASED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    drop(model);
    TS_SLOT_STUCK.fetch_add(1, Ordering::Relaxed);
}

/// Give a refused control Render's exact slot back without marking a reply
/// published. This remains legal after failed INIT closed session admission.
pub(crate) fn abort_control_slot(
    session: NonNull<SessionObject>,
    slot_index: usize,
    slot_generation: u64,
) {
    // SAFETY: the HVC1 context owns a strong reference for this call.
    let obj = unsafe { session.as_ref() };
    if obj
        .model
        .lock()
        .abort_slot(slot_index, slot_generation)
        .is_ok()
    {
        TS_SLOT_RELEASED.fetch_add(1, Ordering::Relaxed);
    } else {
        TS_SLOT_STUCK.fetch_add(1, Ordering::Relaxed);
    }
}

/// Finish a failed INIT after its reply-slot ownership has been cancelled.
/// Admission was already closed by [`reject_session_init`]; this PASSIVE edge
/// drains exact host work and destroys the namespace while the role-1 backing
/// reference is still retained by its ordinary open.
pub(crate) fn finish_failed_session_init(session: NonNull<SessionObject>) {
    // SAFETY: the HVC1 context owns a strong reference for this call.
    let obj = unsafe { session.as_ref() };
    obj.teardown(unsafe { crate::irql::PassiveLevel::assume() });
}

/// A stable code for the control-Render refusals, so `TsCtlRej` names which
/// rule rather than just how often. `SessionRefusal` carries no `code()`; the
/// arms a control Render can actually reach are the ones enumerated here and
/// everything else collapses to `0x0A00`.
fn control_render_code(refusal: model::SessionRefusal) -> u32 {
    use model::SessionRefusal as R;
    match refusal {
        R::SessionDraining => 0x0A01,
        R::ControlRenderNotOnControlContext => 0x0A02,
        R::ControlRenderAllocationWithoutReply { .. } => 0x0A03,
        R::ReplyPoolNotBound => 0x0A04,
        R::ControlRenderAllocationCountNotOne { .. } => 0x0A05,
        R::ControlRenderForeignAllocation => 0x0A06,
        R::ControlRenderPoolGenerationStale { .. } => 0x0A07,
        R::ControlRenderAccessNotWrite { .. } => 0x0A08,
        R::ControlRenderSlotBusy { .. } => 0x0A09,
        R::ControlRenderSlotGenerationStale { .. } => 0x0A0A,
        R::ControlRenderSlotIndexOutOfRange { .. } => 0x0A0B,
        R::ControlRenderSlotGenerationUnknown { .. } => 0x0A0C,
        _ => 0x0A00,
    }
}
