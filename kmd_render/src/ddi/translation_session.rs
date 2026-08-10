//! K5 — HTS1 translation sessions and HQA1 outer-context attach.
//!
//! Every decision with a right and a wrong answer is
//! [`helios_kmd_logic::translation_session`], host-tested; this file does the
//! pointer work, the CSPRNG, the locking and the counters. `helios_protocol`
//! owns the wire records — nothing here re-declares one.
//!
//! # The seam with K6
//!
//! K5 owns `DxgkDdiCreateContext`'s private-data dispatch, the provisional
//! session the HVC1 **control** context creates, the reply-pool binding, HQA1
//! attach, and the session state every later unit compares against. K6 owns the
//! HVC1 **queue** context, `DxgkDdiRender`'s HNR2 decode, Patch, SubmitCommand
//! and the context-local slot pool. Because the finite HTS1 INIT *is* an HNR2
//! Render (§10.4:1206-1211, §17.6:4360-4368), [`session_init`] has no caller
//! until K6 exists and is the third state `K4-CONTRACT.md` §8 names: implemented,
//! never exercised.

use alloc::boxed::Box;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use helios_kmd_logic::translation_session as model;
use helios_protocol::native_render::{
    hvc1_admit_context_flags, parse_hvc1_private_data, Hvc1ContextClass, Hvm1Role,
    HELIOS_HVC1_MAGIC, HELIOS_HVC1_SIZE, HELIOS_NATIVE_RENDER_CAPSET,
};
use helios_protocol::translation_session::{
    parse_create_context_private_data, HeliosSessionCapability, HELIOS_HQA1_MAGIC,
    HELIOS_HQA1_SIZE,
};
use helios_protocol::HELIOS_PACKAGE_GENERATION;

use crate::adapter::AdapterContext;
use crate::dxgk::*;
use crate::sync::SpinLock;

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
/// HVC1 **queue** contexts refused because K6 does not exist. Expected NONZERO
/// only once the ICD's normal-loader path (mesa A3) creates them; zero today.
pub static TS_QUEUE_CTX_UNIMPL: AtomicU32 = AtomicU32::new(0);
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
/// Outer contexts detached at `DxgkDdiDestroyContext`. Unreachable until K6, for
/// [`TS_ATTACH_OK`]'s reason — nothing attaches, so nothing detaches.
pub static TS_DETACH_OK: AtomicU32 = AtomicU32::new(0);
/// Detach refusals — our own accounting bug. **Must read 0**, and unreachable
/// until K6.
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
/// INIT round trips completed. **Unreachable in this package, twice over**:
/// [`session_init`] has no caller until K6, and when K6 supplies one it still
/// refuses, because the host Venus context K11 owns does not exist and
/// [`session_init`] therefore grants zero endpoints. Expect 0; expect
/// `TS_INIT_REJECT` to be what moves once K6 lands.
pub static TS_INIT_OK: AtomicU32 = AtomicU32::new(0);
/// INIT refusals, all causes.
pub static TS_INIT_REJECT: AtomicU32 = AtomicU32::new(0);
/// The CPU could not supply a 128-bit CSPRNG capability, so INIT refused.
/// **Must read 0**: `RDRAND` is present on every x86-64 this package targets.
pub static TS_NO_CSPRNG: AtomicU32 = AtomicU32::new(0);

/// The counter names, as one list, so the collision proof and the writer cannot
/// drift apart.
const COUNTER_NAMES: [&[u8]; 20] = [
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
    let values: [u32; 20] = [
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
    refs: AtomicU32,
}

// SAFETY: `model` is reachable only through the `SpinLock`, and every other field
// is written once at construction and read-only afterwards. `process`/`adapter`
// are compared for identity and never dereferenced by this module.
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
                        return Some(ptr);
                    }
                }
            }
            i += 1;
        }
        None
    }

    /// Move every live session to `Draining`. Reset, removal, and process
    /// teardown all land here; §14 requires capability invalidation to precede
    /// the device-lost wakeup, which is what `begin_draining` does first.
    pub(crate) fn drain_all(&self) {
        let slots = self.slots.lock();
        let mut i = 0;
        while i < SESSION_SLOTS {
            if let Some(ptr) = slots.get(i).copied().flatten() {
                // SAFETY: as in `acquire_by_key`.
                let obj = unsafe { ptr.as_ref() };
                obj.model.lock().begin_draining();
                TS_DRAINED.fetch_add(1, Ordering::Relaxed);
            }
            i += 1;
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
    /// An HQA1 outer-context attach onto an existing session.
    HeliosAttach {
        session: NonNull<SessionObject>,
        context_generation: u64,
        endpoint_id: u32,
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
        // CROSS-LANE: K6 (`ddi/native_render.rs`) owns the queue context, its
        // ring and its slot pool. Refusing is correct until then — a record-only
        // translator creates none, so this arm is unreachable from mesa A1/A2.
        TS_QUEUE_CTX_UNIMPL.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_NOT_SUPPORTED);
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

    let Some(session) = new_session(process, adapter) else {
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
fn new_session(process: usize, adapter: *const AdapterContext) -> Option<NonNull<SessionObject>> {
    let obj = Box::new(SessionObject {
            model: SpinLock::new(model::TranslationSession::new_provisional(
            HELIOS_PACKAGE_GENERATION,
            HELIOS_NATIVE_RENDER_CAPSET,
        )),
        process,
        adapter,
        refs: AtomicU32::new(1),
    });
    NonNull::new(Box::into_raw(obj))
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
    let Some(session) =
        list.acquire_by_key(process, adapter, packet.session_generation, packet.capability())
    else {
        TS_ATTACH_NO_SESSION.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    // SAFETY: `acquire_by_key` returned it with a reference held, so it is live.
    let obj = unsafe { session.as_ref() };
    let admission = obj.model.lock().attach(&packet);
    match admission {
        Ok(admission) => {
            TS_ATTACH_OK.fetch_add(1, Ordering::Relaxed);
            Ok(ContextRequest::HeliosAttach {
                session,
                context_generation: admission.context_generation,
                endpoint_id: admission.endpoint_id,
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
    obj.model.lock().begin_draining();
    TS_DRAINED.fetch_add(1, Ordering::Relaxed);
    if let Some(list) = process_list {
        list.remove(session);
    }
    // SAFETY: releasing the device's own reference.
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
) {
    let Some(session) = *device_session.lock() else {
        // Not a raw KMT device: every ordinary D3D device's HVM1 allocations land
        // here and are simply not this session's pool. Not counted.
        return;
    };
    // SAFETY: the device holds a reference for as long as the field is set.
    let obj = unsafe { session.as_ref() };
    match obj
        .model
        .lock()
        .bind_reply_pool(role, byte_size, object_generation)
    {
        Ok(()) => TS_POOL_BOUND.fetch_add(1, Ordering::Relaxed),
        Err(_) => TS_POOL_REJECT.fetch_add(1, Ordering::Relaxed),
    };
}

// ── The INIT seam ────────────────────────────────────────────────────────────

/// Complete the finite HTS1 `INIT` and produce the reply bytes.
///
/// ⚠ CROSS-LANE SEAM, and this unit's stated unreachable surface: INIT arrives as
/// an HNR2 control Render, and `DxgkDdiRender`'s HNR2 decode is **K6**
/// (`ddi/native_render.rs`, absent). K6 calls this with the 32 INIT bytes it
/// copied and writes the returned 56 bytes into the checked-out reply slot.
///
/// ⛔ The one thing this cannot yet do is create the host Venus context and
/// `VkInstance` (§10.4:1207-1208) — that is K11's transport rewrite. It is
/// therefore refused rather than faked: a session that reports a generation
/// without a host context behind it would make every later HQA1 admit onto
/// nothing.
#[allow(dead_code)] // CROSS-LANE: the caller is K6's `ddi/native_render.rs`.
pub(crate) fn session_init(
    session: NonNull<SessionObject>,
    request: &[u8],
) -> Result<[u8; HTS1_REPLY_BYTES], NTSTATUS> {
    use helios_protocol::translation_session::HeliosTranslationSessionInitV1;

    // SAFETY: the caller holds a reference to the session for this call.
    let obj = unsafe { session.as_ref() };
    if request.len() != core::mem::size_of::<HeliosTranslationSessionInitV1>() {
        TS_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_PARAMETER);
    }
    let Ok(record) = bytemuck::try_pod_read_unaligned::<HeliosTranslationSessionInitV1>(request)
    else {
        TS_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_PARAMETER);
    };
    let requested = match obj.model.lock().admit_init(&record) {
        Ok(requested) => requested,
        Err(_) => {
            TS_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        }
    };
    let Some(capability) = mint_capability() else {
        TS_NO_CSPRNG.fetch_add(1, Ordering::Relaxed);
        TS_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_DEVICE_NOT_READY);
    };
    let Ok(generation) = SESSION_GENERATION.lock().mint() else {
        TS_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INSUFFICIENT_RESOURCES);
    };
    // ⛔ The host Venus context/`VkInstance` this INIT is supposed to create is
    // K11's. Until it exists the granted endpoint capacity is zero, which
    // `complete_init` refuses — loudly, rather than admitting a session with no
    // host object behind it.
    let granted = 0u32;
    match obj
        .model
        .lock()
        .complete_init(requested, granted, generation, capability)
    {
        Ok(reply) => {
            TS_INIT_OK.fetch_add(1, Ordering::Relaxed);
            let mut out = [0u8; HTS1_REPLY_BYTES];
            out.copy_from_slice(bytemuck::bytes_of(&reply));
            Ok(out)
        }
        Err(_) => {
            TS_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
            Err(STATUS_DEVICE_NOT_READY)
        }
    }
}
