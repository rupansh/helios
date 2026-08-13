//! Helios HTS1 translation-session and HQA1 outer-context-attach ABI —
//! the bytes that establish *which* pre-existing host Venus namespace an outer
//! D3D context is allowed to submit into (HELIOS_PRESENT_SYNC_RETIREMENT.md
//! sections 10.1, 10.2, 10.4, 10.7, 17.1).
//!
//! # The boundary these bytes cross
//!
//! Two distinct boundaries, one file, because they name the same session:
//!
//!   - **HTS1 INIT / reply** — user mode (Mesa `vn_instance`, on its *raw* KMT
//!     device's HVC1 control context) -> KMD -> host virgl/Venus, carried as a
//!     finite `HNR2` control Render with a C51 event-backed reply published into
//!     one checked-out slot of the session's role-1 `HVM1` pool. INIT creates
//!     exactly one host context and one `VkInstance` and returns the session
//!     generation, the 128-bit capability, the bounded endpoint capacity, and
//!     package/capset confirmation (section 10.4, lines 1188-1213).
//!   - **HQA1** — the *complete* create-context private driver data the D3D UMD
//!     passes to `pfnCreateContextCb` (D3D11, physical) or
//!     `pfnCreateContextVirtualCb` (D3D12, virtual), read once by the KMD in
//!     `DxgkDdiCreateContext` (section 10.4, lines 1215-1241).
//!
//! `D3DDDICB_RENDER::pPrivateDriverData` stays reserved-zero: no Render, Submit,
//! Present, allocation-open, or display callback may attach a session or
//! reinterpret HQA1 (section 10.4, lines 1327-1333).
//!
//! # Producer status
//!
//! **Re-measured 2026-08-13.** Mesa A1/A2 produce HTS1/HVC1/HNR2 through
//! ordinary KMT calls. KMD K5/K6 consume those records, and K11 now owns the
//! per-session stock-Venus context plus the finite role-1 HVR1 reply. The K11
//! source and mutation gates do not by themselves establish target execution;
//! that requires the correlated multi-session runtime probe.
//!
//! KMD also parses HQA1 once at context creation and retains direct strong
//! session/endpoint references. The probe exercises that consumer, but the D3D
//! producers remain Mesa A3/A4 work: no ordinary outer submission is admitted
//! merely because HTS1 INIT succeeds.
//!
//! # What supersedes what
//!
//! This module supersedes the *context/session establishment* verbs of the
//! System-class stack: [`crate::escape`]'s `HELIOS_ESCAPE_CTX_CREATE` /
//! `CTX_DESTROY` payloads and [`crate::ioctl`]'s `IOCTL_HELIOS_CTX_*` carrier.
//! Section 17.1 deletes both of those files outright, but their consumers
//! (`kmd_render`, `umd`, `umd12`) have not migrated yet, so this phase is purely
//! additive and both legacy modules remain in the tree.
//!
//! # Prohibitions this file is required to keep true
//!
//! **HQA1 contains no pointer, no KMT handle, no PID, no allocation/resource
//! identity, no host object ID, and no synchronization object.** Every field is
//! either a generation counter, an unpredictable admission nonce, a
//! session-local endpoint ordinal, a class/flags scalar, or reserved-zero. In
//! particular [`HeliosQueueAttachV1::endpoint_id`] is **not** a host
//! `INFO_RING_IDX`: section 10.7 (line 1723) states that no host context or ring
//! ID is ever returned to user mode, so the endpoint ordinal is a session-local
//! name the KMD maps internally to the endpoint's unique nonzero ring.
//!
//! **HQC1 has no wire payload and therefore no struct here.** It is an ordinary
//! unshared WDDM monitored fence created through
//! `pfnCreateSynchronizationObject2Cb` (section 10.6, lines 1469-1470; C61); it
//! is an OS synchronization object, never bytes on this or any other wire, and
//! it is never exposed as an API fence.
//!
//! # Capabilities are nonces, not identity (invariant 16)
//!
//! Section 10.1 invariant 16: *"HTS1 capabilities are unpredictable admission
//! nonces, not resource IDs or a security boundary against code already
//! executing in the same process. They are invalidated before reset/removal
//! wakeups, never persist across a process/package generation, and are never
//! reused within that generation."* Three consequences are load-bearing here:
//!
//!   1. A capability never names, indexes, or resolves anything. It is compared
//!      for equality against the one session the raw device already owns, once,
//!      at context creation — and after that the live KMD context object is the
//!      identity (invariant 13; section 10.4, lines 1237-1243).
//!   2. Because it is explicitly *not* an in-process security boundary, plain
//!      equality is the correct comparison; no constant-time primitive is
//!      implied (and `core` has none). It is still never echoed into a refusal
//!      value or a diagnostic record — see [`CapabilityRefusal`].
//!   3. This file defines the capability **field and its validation rules only**.
//!      It neither generates nor contains a capability value: the KMD supplies a
//!      CSPRNG-generated 128-bit value (section 10.4, line 1199), and
//!      [`HeliosSessionCapability::INVALID`] is the all-zero *absent/invalidated*
//!      sentinel, never a usable capability.
//!
//! # Layout rules
//!
//! Every wire struct is `#[repr(C)]`, little-endian, pointer-free, and
//! padding-free (explicit reserved fields, never implicit padding), so it derives
//! `Pod`/`Zeroable` and its C mirror cannot drift. Every size, alignment, and
//! field offset in the section-10.4 table is a `const` assertion at the bottom
//! of this file: a wrong offset breaks the build, not a test.
//!
//! ⚠ The mirror is `protocol/include/helios_translation_session.h`, and the
//! phrase "the C mirror in Mesa/QEMU" that stood here was wrong twice over:
//! Mesa does not `#include` it (only `helios_wddm.h`, via `vn_helios_hwa2.h`),
//! and QEMU mirrors nothing from this module at all. Its `_Static_assert`s are
//! evaluated only by `tools/retirement-gates.sh`.
//!
//! Every validator is a total function returning a `Result` with a *named*
//! reason per rejection — never a `bool`, never a panic. Reserved fields are
//! validated as zero; an unknown or mismatched magic, version, size, or
//! generation is a hard reject (section 10.4, lines 1362-1366; the failure table
//! at lines 2860-2861).
//!
//! # The package generation
//!
//! Every validator here that checks a `package generation` field takes the
//! expected value as a **parameter**, never as a compile-time read, so the KMD
//! can pin one generation for an adapter's lifetime and a test can exercise a
//! mismatch. That parameter's one production value is
//! [`crate::HELIOS_PACKAGE_GENERATION`] (section 17.1, final bullet: "one
//! protocol/package generation constant shared by protocol, Mesa, UMD11, UMD12,
//! KMD, QEMU, and installer. Generation mismatch is fatal."). Zero is never a
//! wildcard on either side of the comparison.

use bytemuck::{Pod, Zeroable};

// ── Magics, versions, sizes ─────────────────────────────────────────────────

/// `'HQA1'` — [`HeliosQueueAttachV1::magic`].
/// HELIOS_PRESENT_SYNC_RETIREMENT.md section 10.4, table row `offset 0`.
pub const HELIOS_HQA1_MAGIC: u32 = 0x3141_5148;
/// `HeliosQueueAttachV1` ABI version (section 10.4, table row `offset 4`).
pub const HELIOS_HQA1_ABI_VERSION: u16 = 1;
/// `HeliosQueueAttachV1` structure size (section 10.4, table row `offset 6`).
/// The create-context private data must be **exactly** this many bytes: the doc
/// calls HQA1 "the complete create-context private data" (line 1218).
pub const HELIOS_HQA1_SIZE: u16 = 72;

/// `'HTS1'` — [`HeliosTranslationSessionInitV1::magic`].
///
/// ⚠ Section 10.4 specifies the INIT request/reply *contents* (lines 1196-1201)
/// but gives no byte table and no magic for either, unlike HQA1/HOB1/HOS1/HVC1/
/// HNR2/HVM1/HVR1. The tag follows the corpus's own convention (ASCII, little
/// endian) and the request and reply get **distinct** magics for a reason the
/// retired HPS2 ABI proved the hard way — its `HELIOS_PRESENT_REFRESH_MAGIC`
/// shared a decoder with its sibling record, and a shared magic lets one decoder
/// arm accept the other record's bytes.
pub const HELIOS_HTS1_INIT_MAGIC: u32 = 0x3153_5448;
/// `'HTR1'` — [`HeliosTranslationSessionReplyV1::magic`]. Deliberately distinct
/// from [`HELIOS_HTS1_INIT_MAGIC`]; see that constant.
pub const HELIOS_HTS1_REPLY_MAGIC: u32 = 0x3152_5448;
/// HTS1 INIT request/reply ABI version.
pub const HELIOS_HTS1_ABI_VERSION: u16 = 1;
/// [`HeliosTranslationSessionInitV1`] structure size.
pub const HELIOS_HTS1_INIT_SIZE: u16 = 32;
/// [`HeliosTranslationSessionReplyV1`] structure size.
pub const HELIOS_HTS1_REPLY_SIZE: u16 = 56;
/// [`HeliosTranslationEndpointV1`] record size. The descriptor is an array
/// element of a versioned parent, never a standalone message, so it carries no
/// magic of its own.
pub const HELIOS_HTS1_ENDPOINT_SIZE: u16 = 16;

// ── HQA1 flags (section 10.4, table row `offset 64`) ────────────────────────
//
// UNIFIED: HQA1's `offset 64` and HOB1's `offset 44` are the same two-valued
// outer-context arm, and section 10.4 puts the *numbers* on the HOB1 row
// ("exactly one of `D3D11_PHYSICAL=1`, `D3D12_VIRTUAL=2`", line 1267) while
// giving HQA1 only bit positions. So [`crate::wddm`] declares the values and
// these are aliases of them. Two independent declarations of the same wire arm,
// hand-kept equal, is precisely the drift class this crate exists to prevent:
// an HQA1 that attaches a physical context while its HOB1 records claim the
// virtual arm would pass both validators separately.

/// `bit 0` — the outer context is the D3D11 **physical** Render context
/// (`pfnCreateContextCb` + `pfnRenderCb`). *Is* HOB1's `D3D11_PHYSICAL=1`
/// (section 10.4, HOB1 table row `offset 44`), not merely equal to it.
pub const HELIOS_HQA1_FLAG_D3D11_PHYSICAL: u32 = crate::wddm::HELIOS_HOB1_FLAG_D3D11_PHYSICAL;
/// `bit 1` — the outer context is the D3D12 **virtual** Submit context
/// (`pfnCreateContextVirtualCb` + `pfnSubmitCommandCb`). *Is* HOB1's
/// `D3D12_VIRTUAL=2`.
pub const HELIOS_HQA1_FLAG_D3D12_VIRTUAL: u32 = crate::wddm::HELIOS_HOB1_FLAG_D3D12_VIRTUAL;
/// The complete set of defined flag bits. "exactly one set" is the rule; any bit
/// outside this mask is a hard reject, so a future bit cannot be smuggled past
/// an old KMD as a no-op.
pub const HELIOS_HQA1_FLAGS_MASK: u32 =
    HELIOS_HQA1_FLAG_D3D11_PHYSICAL | HELIOS_HQA1_FLAG_D3D12_VIRTUAL;

// ── Engine class (section 10.4, table row `offset 44`) ──────────────────────
//
// The doc names the vocabulary — "exact graphics/compute/copy class admitted for
// the outer context" — but not the encoding. These are dense 1-based values;
// zero is reserved as "unset" so a zeroed buffer can never classify as a real
// engine, and every unknown value is a hard reject.

/// `graphics` — the endpoint's physical lower VkQueue is a graphics queue.
pub const HELIOS_ENGINE_CLASS_GRAPHICS: u32 = 1;
/// `compute` — the endpoint's physical lower VkQueue is a compute queue.
pub const HELIOS_ENGINE_CLASS_COMPUTE: u32 = 2;
/// `copy` — the endpoint's physical lower VkQueue is a transfer/copy queue.
pub const HELIOS_ENGINE_CLASS_COPY: u32 = 3;

/// The admitted engine class of one physical lower-queue endpoint.
///
/// As an exhaustive value rather than a `u32`, adding a class fails to compile
/// until every match handles it — the same reasoning [`HeliosOuterContextKind`]
/// and [`crate::wddm::HeliosUseIdentity`] apply to their own closed vocabularies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosEngineClass {
    Graphics,
    Compute,
    Copy,
}

impl HeliosEngineClass {
    /// The wire encoding. `const` so [`HeliosQueueAttachV1::new`] stays `const`.
    #[inline]
    pub const fn wire(self) -> u32 {
        match self {
            HeliosEngineClass::Graphics => HELIOS_ENGINE_CLASS_GRAPHICS,
            HeliosEngineClass::Compute => HELIOS_ENGINE_CLASS_COMPUTE,
            HeliosEngineClass::Copy => HELIOS_ENGINE_CLASS_COPY,
        }
    }

    /// Total decode. Zero and every undefined value are refusals, never a
    /// silent default.
    #[inline]
    pub fn from_wire(value: u32) -> Result<Self, EndpointRefusal> {
        match value {
            HELIOS_ENGINE_CLASS_GRAPHICS => Ok(HeliosEngineClass::Graphics),
            HELIOS_ENGINE_CLASS_COMPUTE => Ok(HeliosEngineClass::Compute),
            HELIOS_ENGINE_CLASS_COPY => Ok(HeliosEngineClass::Copy),
            found => Err(EndpointRefusal::UnknownEngineClass { found }),
        }
    }
}

/// Which outer submission model the attached context uses. Decoded from
/// [`HeliosQueueAttachV1::flags`]; "exactly one set" makes this a total,
/// two-valued answer or a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosOuterContextKind {
    /// D3D11: physical context, actual `pfnRenderCb` carrying the HOB1 bytes in
    /// the runtime command window with its normal allocation list.
    D3d11PhysicalRender,
    /// D3D12: virtual context, `pfnSubmitCommandCb` with the 64-byte HOS1
    /// descriptor and HOB1 left at the submitted command GPUVA.
    D3d12VirtualSubmit,
}

impl HeliosOuterContextKind {
    #[inline]
    pub const fn wire(self) -> u32 {
        match self {
            HeliosOuterContextKind::D3d11PhysicalRender => HELIOS_HQA1_FLAG_D3D11_PHYSICAL,
            HeliosOuterContextKind::D3d12VirtualSubmit => HELIOS_HQA1_FLAG_D3D12_VIRTUAL,
        }
    }
}

// ── Bounded limits (section 10.4 lines 1243-1251, 1362-1366; section 10.7) ──
//
// Section 17.1 requires "bounded session/ring/context-local-batch/
// host-dispatch-FIFO limits" to be declared here. Where the doc states a number
// it is used verbatim; where it only says "bounded" in prose, the constant is
// still declared (an unstated bound is an unbounded implementation) and the
// prose line is cited at the declaration. Exhausting any of them is a hard
// failure of session/device/context creation — "bounded session/ring exhaustion
// fails before device exposure" (line 990) and "host-dispatch-FIFO exhaustion,
// or session-capacity exhaustion fails device creation or removes that device"
// (lines 1363-1365). Never a wait, never a spill, never a retry loop.

/// Maximum live HTS1 sessions in one KMD `ProcessContext`.
///
/// PROSE-ONLY BOUND. Section 10.1 invariant 10 (lines 926-930) and section 10.4
/// (lines 1205-1207) say the process object "owns a bounded list of those
/// sessions only while they are live"; section 15 (line 4326) says
/// `DxgkDdiCreateProcess` "allocates one bounded ProcessContext session list".
/// No number is given. One session exists per Mesa `vn_instance` (invariant 12),
/// and a realistic process holds a handful (DXVK, vkd3d, and any native Vulkan
/// instance); 16 leaves an order of magnitude of headroom while keeping the
/// nonpaged list small and fixed. The list is never searched on a submit path,
/// so its size is a capacity decision only.
pub const HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS: u32 = 16;

/// Largest legal host `INFO_RING_IDX`. Hard **wire** ceiling, not policy: the
/// virtio-gpu control header carries `ring_idx` as a single byte
/// ([`crate::virtio_gpu::VirtioGpuCtrlHdr::ring_idx`]), so a host context has
/// rings `0..=255` and ring 0 is the CPU/decode-only control ring (invariant 12).
pub const HELIOS_HTS1_MAX_RING_INDEX: u32 = 255;

/// Maximum physical lower-queue endpoints per HTS1 session — the ceiling on the
/// "bounded endpoint capacity" INIT returns (section 10.4, line 1200).
///
/// PROSE-ONLY BOUND, with a hard ceiling above it: each endpoint owns one
/// unique, non-recycled nonzero ring (invariant 12), so the capacity can never
/// exceed [`HELIOS_HTS1_MAX_RING_INDEX`]. A Vulkan physical device on this
/// substrate exposes single-digit queue counts across all families, so 64 is
/// far above any real device while staying well inside the ring namespace.
pub const HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION: u32 = 64;

/// Maximum outstanding context-local batches (one per unretired
/// context-local batch ID) on one context.
///
/// DOC NUMBER. Section 10.7, lines 1837-1840: "one slot from a context-local
/// generation-checked pool capped at 64 outstanding submissions and 15 MiB
/// total"; restated in section 16.2, lines 3581-3583.
///
/// UNIFIED: that sentence is section 10.7's, so
/// [`crate::native_render::HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS`] declares the
/// number and this is an alias. The two lanes bound *different* pools (this one
/// counts unretired context-local batch IDs on an outer HQA1-attached context;
/// the native one counts checked-out HNR2 reply slots on a raw HVC1 context) but
/// they are the same doc number, and a reader who changes one must change both.
pub const HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES: u32 =
    crate::native_render::HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS;

/// Maximum bytes a single context-local batch may occupy, header through
/// payload.
///
/// DOC NUMBER. Section 10.4, HOB1 table row `offset 48`: "nonzero, at most
/// 15 MiB"; section 10.7, line 1749: "one HNR2 batch of at most 15 MiB". Also
/// the total byte cap of the 64-slot context-local pool (line 1838).
///
/// UNIFIED: a context-local batch on an outer context *is* one HOB1 record, so
/// this is an alias of [`crate::wddm::HELIOS_HOB1_MAX_BYTES`], the constant that
/// the HOB1 header validator actually enforces. Declaring the cap twice would let
/// the session admission and the record validator disagree about which batches
/// are legal.
pub const HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES: u64 = crate::wddm::HELIOS_HOB1_MAX_BYTES;

/// Depth of one physical endpoint's host-dispatch FIFO — the queue of
/// already-scheduler-eligible DMAs to which `DxgkDdiSubmitCommand` assigns
/// arrival-order serials under the short endpoint lock.
///
/// PROSE-ONLY BOUND. Section 10.4, lines 1243-1248: "Each endpoint has a bounded
/// host-dispatch FIFO shared only by outer contexts that vkd3d/DXVK explicitly
/// mapped to the same physical VkQueue"; section 15, lines 4335-4339: it
/// "rejects capacity exhaustion". No number is given. The FIFO is *shared* by
/// several outer contexts, so it is sized at four times one context's outstanding
/// cap ([`HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES`]): a single context can
/// never exhaust it alone, which keeps exhaustion a genuine capacity signal
/// rather than a self-inflicted one.
pub const HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH: u32 =
    4 * HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES;

// ── Capacity admission (section 10.2 line 990, section 10.4 line 1364) ──────
//
// §17.1 requires this file to carry "bounded session/ring/context-local-batch/
// host-dispatch-FIFO limits". A `const` alone is not a bound: §10.2 says
// "bounded session/ring exhaustion fails before device exposure" and §10.4 says
// "host-dispatch-FIFO exhaustion, or session-capacity exhaustion fails device
// creation or removes that device". Those are mechanisms, so each cap gets a
// total admission function with a named refusal here, rather than each of the
// KMD, ICD, and host re-deriving the comparison. ⛔ Every one of them fails
// immediately: "never a wait, never a spill, never a retry loop".

/// Which bounded capacity a [`HeliosCapacityRefusal`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosCapacityLimit {
    /// [`HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS`] — the ProcessContext's bounded
    /// live-session list.
    SessionsPerProcess,
    /// [`HELIOS_HTS1_MAX_RING_INDEX`] — the hard wire ceiling on a host
    /// `INFO_RING_IDX`.
    RingIndex,
    /// [`HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES`] — unretired
    /// context-local batch IDs on one outer context.
    OutstandingContextBatches,
    /// [`HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES`] — bytes in one context-local
    /// batch.
    ContextBatchBytes,
    /// [`HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH`] — already-eligible DMAs
    /// queued on one physical endpoint.
    HostDispatchFifoDepth,
}

/// A bounded capacity was exhausted. Both numbers are carried so a counter or
/// ETW event can say how far over the caller went without a second lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosCapacityRefusal {
    pub limit: HeliosCapacityLimit,
    /// What admitting this request would make the live count / size.
    pub requested: u64,
    /// The cap.
    pub capacity: u64,
}

/// Admit one more HTS1 session in a ProcessContext holding `live` already.
#[inline]
pub const fn admit_new_session(live: u32) -> Result<(), HeliosCapacityRefusal> {
    if live >= HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS {
        return Err(HeliosCapacityRefusal {
            limit: HeliosCapacityLimit::SessionsPerProcess,
            requested: live as u64 + 1,
            capacity: HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS as u64,
        });
    }
    Ok(())
}

/// Admit one host ring index against the wire ceiling.
///
/// Ring 0 is the CPU/decode-only control ring and every queue endpoint owns a
/// unique nonzero ring; which of the two a caller may use is the caller's
/// context, so only the ceiling is checked here.
#[inline]
pub const fn admit_ring_index(ring_index: u32) -> Result<(), HeliosCapacityRefusal> {
    if ring_index > HELIOS_HTS1_MAX_RING_INDEX {
        return Err(HeliosCapacityRefusal {
            limit: HeliosCapacityLimit::RingIndex,
            requested: ring_index as u64,
            capacity: HELIOS_HTS1_MAX_RING_INDEX as u64,
        });
    }
    Ok(())
}

/// Admit one more context-local batch of `batch_bytes` on a context with
/// `outstanding` unretired batches.
#[inline]
pub const fn admit_context_batch(
    outstanding: u32,
    batch_bytes: u64,
) -> Result<(), HeliosCapacityRefusal> {
    if outstanding >= HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES {
        return Err(HeliosCapacityRefusal {
            limit: HeliosCapacityLimit::OutstandingContextBatches,
            requested: outstanding as u64 + 1,
            capacity: HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES as u64,
        });
    }
    if batch_bytes == 0 || batch_bytes > HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES {
        return Err(HeliosCapacityRefusal {
            limit: HeliosCapacityLimit::ContextBatchBytes,
            requested: batch_bytes,
            capacity: HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES,
        });
    }
    Ok(())
}

/// Admit one more enqueue onto an endpoint's host-dispatch FIFO holding `depth`.
#[inline]
pub const fn admit_host_dispatch_enqueue(depth: u32) -> Result<(), HeliosCapacityRefusal> {
    if depth >= HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH {
        return Err(HeliosCapacityRefusal {
            limit: HeliosCapacityLimit::HostDispatchFifoDepth,
            requested: depth as u64 + 1,
            capacity: HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH as u64,
        });
    }
    Ok(())
}

// ── Generation and capability vocabulary ────────────────────────────────────

/// Which generation a [`GenerationRefusal`] is about. Keeping the field name out
/// of the reason (rather than minting three parallel enums) means a caller
/// logging a refusal always states *which* generation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationField {
    /// The atomic package generation shared by protocol/Mesa/UMD11/UMD12/KMD/
    /// QEMU/installer. A mismatch means mixed binaries (section 10.2, "package
    /// generation" admission row).
    Package,
    /// The HTS1 session generation returned by INIT — nonzero, per session.
    Session,
    /// The UMD-chosen outer-context generation in HQA1 — nonzero, strictly
    /// increasing, never reused within the session.
    Context,
}

/// Why a generation check failed. Total: every reject path has a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationRefusal {
    /// The value is zero. Zero is never a live generation, for any of the three
    /// fields — a zeroed or partially written buffer must not admit anything.
    Zero,
    /// The value does not equal the one the receiving object already holds.
    Mismatch { found: u64, expected: u64 },
    /// A context generation that is not strictly greater than the highest one
    /// this session has already admitted. This subsumes "duplicate": a strictly
    /// increasing sequence can never repeat a value (section 10.4, table row
    /// `offset 56`, and lines 1237-1239 "rejects a zero or duplicate live
    /// context generation").
    NotMonotonic { found: u64, watermark: u64 },
}

/// Validate an exact-match generation (package or session): nonzero and equal.
#[inline]
pub fn check_generation_match(found: u64, expected: u64) -> Result<(), GenerationRefusal> {
    if found == 0 {
        return Err(GenerationRefusal::Zero);
    }
    if expected == 0 {
        // The receiver has no live generation to match against — e.g. a session
        // that was invalidated by reset. Fail closed rather than accepting
        // whatever the caller supplied.
        return Err(GenerationRefusal::Mismatch { found, expected });
    }
    if found != expected {
        return Err(GenerationRefusal::Mismatch { found, expected });
    }
    Ok(())
}

/// Validate a UMD-chosen context generation against the session's high-water
/// mark: nonzero and strictly increasing. `watermark` is the highest context
/// generation this HTS1 session has already admitted (0 = none yet).
///
/// This is the complete *wire* rule. A KMD that also tracks its live contexts
/// separately should still refuse a generation that is live, but under a
/// strictly increasing sequence that check can never fire on its own.
#[inline]
pub fn check_context_generation(found: u64, watermark: u64) -> Result<(), GenerationRefusal> {
    if found == 0 {
        return Err(GenerationRefusal::Zero);
    }
    if found <= watermark {
        return Err(GenerationRefusal::NotMonotonic { found, watermark });
    }
    Ok(())
}

/// The 128-bit HTS1 admission nonce, as two little-endian halves.
///
/// ⚠ Read the module header before using this type. It is **not** a resource ID,
/// **not** a handle, and **not** a security boundary inside a process
/// (invariant 16). It is generated by the KMD from a CSPRNG and returned once
/// through the INIT reply; this crate never generates one and never holds a
/// live value beyond the caller's own storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosSessionCapability {
    pub low: u64,
    pub high: u64,
}

impl HeliosSessionCapability {
    /// The all-zero sentinel: *absent or invalidated*, never a usable
    /// capability. Reset/removal writes this before waking any waiter
    /// (invariant 16; section 13, line 3420 — "invalidates all HQA1
    /// capabilities ... and cancels C51/HQC1 event waiters as device-lost").
    ///
    /// Only the all-zero pair is the sentinel: a CSPRNG may legitimately produce
    /// a zero half, so a single zero half is not by itself invalid.
    pub const INVALID: Self = Self { low: 0, high: 0 };

    #[inline]
    pub const fn from_halves(low: u64, high: u64) -> Self {
        Self { low, high }
    }

    #[inline]
    pub const fn is_invalid(self) -> bool {
        self.low == 0 && self.high == 0
    }

    /// Compare a presented capability against the one the session holds.
    ///
    /// Plain equality is correct here — invariant 16 explicitly denies the
    /// in-process security-boundary reading, so there is no constant-time
    /// requirement (and `core` offers no such primitive to a `no_std` KMD).
    #[inline]
    pub fn check_match(self, expected: Self) -> Result<(), CapabilityRefusal> {
        if expected.is_invalid() {
            return Err(CapabilityRefusal::SessionInvalidated);
        }
        if self.is_invalid() {
            return Err(CapabilityRefusal::Absent);
        }
        if self != expected {
            return Err(CapabilityRefusal::Mismatch);
        }
        Ok(())
    }
}

/// Why a capability check failed.
///
/// ⛔ No variant carries a capability value. A refusal is logged, counted, and
/// sometimes traced; an admission nonce must not be copied into a diagnostic
/// record (section 17.1's diagnostics bullet: the ETW schema "contains no
/// handle, pointer, backing token, or raw `resid`"). The three reasons are
/// distinguishable without the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityRefusal {
    /// The presented capability is the all-zero sentinel.
    Absent,
    /// The session's own capability has been invalidated (reset/removal, or a
    /// generation that no longer exists). Nothing may be admitted into it.
    SessionInvalidated,
    /// A live, nonzero capability that is not this session's.
    Mismatch,
}

// ── Endpoint descriptor ─────────────────────────────────────────────────────

/// One physical lower-queue endpoint of an HTS1 session.
///
/// The KMD returns the session's endpoints to the translator through the private
/// direct table (section 14, lines 3994-4001: "physical endpoint descriptors only
/// through the private direct table"), and the UMD copies the selected one into
/// [`HeliosQueueAttachV1`]. 16 bytes, no magic: it is only ever an element of a
/// versioned parent record, never a standalone message.
///
/// ⚠ `queue_family` and `queue_index` are **diagnostic cross-check only, never
/// identity** (section 10.4, table rows `offset 48`/`offset 52`; the same rule
/// appears for HVC1 in section 10.7, lines 1706-1707). The KMD compares them and
/// rejects a mismatch — it never *looks an endpoint up* by them, and a validator
/// here never uses them to select anything.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub struct HeliosTranslationEndpointV1 {
    /// Session-local endpoint ordinal, `1..=endpoint_capacity`. Nonzero.
    ///
    /// ⛔ NOT a host `INFO_RING_IDX` and never usable as one: no host context or
    /// ring ID is returned to user mode (section 10.7, lines 1723-1724). The KMD
    /// owns the ordinal-to-ring mapping internally.
    pub endpoint_id: u32,
    /// `HELIOS_ENGINE_CLASS_*` — the graphics/compute/copy class admitted for
    /// contexts attached to this endpoint.
    pub engine_class: u32,
    /// Copied Vulkan queue-family ordinal. Diagnostic cross-check only.
    pub queue_family: u32,
    /// Copied Vulkan queue index within that family. Diagnostic cross-check only.
    pub queue_index: u32,
}

/// Why an endpoint descriptor was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointRefusal {
    /// `endpoint_id == 0`. Section 10.4, table row `offset 40`: "nonzero".
    ZeroEndpointId,
    /// `endpoint_id` is outside `1..=capacity` for this session.
    EndpointIdOutOfRange { found: u32, capacity: u32 },
    /// The session's advertised capacity is zero or above
    /// [`HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION`].
    CapacityOutOfRange { found: u32, max: u32 },
    /// `engine_class` is zero or undefined.
    UnknownEngineClass { found: u32 },
    /// Both queue ordinals are `UINT32_MAX`, which is HVC1's reserved encoding
    /// for the **control** context (section 10.7, table rows `offset 24`/
    /// `offset 28`). An endpoint is always a real physical lower VkQueue, so the
    /// control sentinel can never describe one.
    ControlSentinelEndpoint,
}

impl HeliosTranslationEndpointV1 {
    #[inline]
    pub const fn new(
        endpoint_id: u32,
        engine_class: HeliosEngineClass,
        queue_family: u32,
        queue_index: u32,
    ) -> Self {
        Self {
            endpoint_id,
            engine_class: engine_class.wire(),
            queue_family,
            queue_index,
        }
    }

    /// Total validation against the session's advertised endpoint capacity.
    /// Returns the decoded engine class so callers never re-decode the `u32`.
    pub fn validate(&self, endpoint_capacity: u32) -> Result<HeliosEngineClass, EndpointRefusal> {
        if endpoint_capacity == 0 || endpoint_capacity > HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION {
            return Err(EndpointRefusal::CapacityOutOfRange {
                found: endpoint_capacity,
                max: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION,
            });
        }
        if self.endpoint_id == 0 {
            return Err(EndpointRefusal::ZeroEndpointId);
        }
        if self.endpoint_id > endpoint_capacity {
            return Err(EndpointRefusal::EndpointIdOutOfRange {
                found: self.endpoint_id,
                capacity: endpoint_capacity,
            });
        }
        if self.queue_family == u32::MAX && self.queue_index == u32::MAX {
            return Err(EndpointRefusal::ControlSentinelEndpoint);
        }
        HeliosEngineClass::from_wire(self.engine_class)
    }
}

// ── HTS1 INIT request ───────────────────────────────────────────────────────

/// The finite HTS1 `INIT` request the raw KMT device's HVC1 control context
/// sends before any dependent outer context exists (section 10.4, lines
/// 1188-1203).
///
/// It carries only what INIT must confirm: the atomic package generation and the
/// exact Venus capset the caller was built against, plus the endpoint capacity it
/// is asking the session to admit. It names no allocation — the reply-pool slot
/// it will be written into is named by the enclosing HNR2 record's allocation
/// list and reply descriptor, not by these bytes.
///
/// 32 bytes, padding-free.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub struct HeliosTranslationSessionInitV1 {
    /// `== HELIOS_HTS1_INIT_MAGIC`.
    pub magic: u32,
    /// `== HELIOS_HTS1_ABI_VERSION`.
    pub abi_version: u16,
    /// `== HELIOS_HTS1_INIT_SIZE`.
    pub struct_size: u16,
    /// Exact atomic-package generation of the caller's binaries.
    pub package_generation: u64,
    /// Exact Venus capset ID ([`crate::virtio_gpu::VIRTIO_GPU_CAPSET_VENUS`]),
    /// matching HVC1's `capset` field (section 10.7, table row `offset 16`).
    pub capset: u32,
    /// Endpoint capacity the translator is asking for: `1..=`
    /// [`HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION`]. The reply's
    /// `endpoint_capacity` is authoritative and may be smaller.
    pub requested_endpoint_capacity: u32,
    /// Reserved, zero.
    pub reserved: u64,
}

/// Why an HTS1 INIT request or reply was refused. Shared by both directions so
/// the KMD and the ICD name the same failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitRefusal {
    /// Magic is not this record's magic (see [`HELIOS_HTS1_INIT_MAGIC`] for why
    /// the two directions do not share one).
    BadMagic { found: u32 },
    /// ABI version is not [`HELIOS_HTS1_ABI_VERSION`]. There is no version
    /// fallback in this package (section 3: "No version or feature fallback").
    BadAbiVersion { found: u16 },
    /// `struct_size` does not equal this record's fixed size.
    BadStructSize { found: u16 },
    /// A reserved field was nonzero.
    ReservedNonZero,
    /// A generation check failed.
    Generation {
        field: GenerationField,
        reason: GenerationRefusal,
    },
    /// The capset is not the one the package was built against.
    CapsetMismatch { found: u32, expected: u32 },
    /// The capability field failed validation.
    Capability(CapabilityRefusal),
    /// The endpoint capacity is zero, or above
    /// [`HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION`].
    EndpointCapacityOutOfRange { found: u32, max: u32 },
    /// The reply granted more endpoints than the request asked for. A session
    /// may grant fewer, never more.
    EndpointCapacityExceedsRequest { granted: u32, requested: u32 },
    /// The supplied buffer is not exactly this record's size.
    PayloadSize { found: usize, expected: usize },
    /// The buffer was the right length but could not be read as this record.
    /// Unreachable after the length check above; it exists so the parse path has
    /// no panicking arm at all.
    PayloadUnreadable,
}

impl HeliosTranslationSessionInitV1 {
    #[inline]
    pub const fn new(
        package_generation: u64,
        capset: u32,
        requested_endpoint_capacity: u32,
    ) -> Self {
        Self {
            magic: HELIOS_HTS1_INIT_MAGIC,
            abi_version: HELIOS_HTS1_ABI_VERSION,
            struct_size: HELIOS_HTS1_INIT_SIZE,
            package_generation,
            capset,
            requested_endpoint_capacity,
            reserved: 0,
        }
    }

    /// Total validation on the receiving (KMD) side. Returns the requested
    /// endpoint capacity, already clamped into the legal range by rejection.
    pub fn validate(
        &self,
        expected_package_generation: u64,
        expected_capset: u32,
    ) -> Result<u32, InitRefusal> {
        if self.magic != HELIOS_HTS1_INIT_MAGIC {
            return Err(InitRefusal::BadMagic { found: self.magic });
        }
        if self.abi_version != HELIOS_HTS1_ABI_VERSION {
            return Err(InitRefusal::BadAbiVersion {
                found: self.abi_version,
            });
        }
        if self.struct_size != HELIOS_HTS1_INIT_SIZE {
            return Err(InitRefusal::BadStructSize {
                found: self.struct_size,
            });
        }
        if self.reserved != 0 {
            return Err(InitRefusal::ReservedNonZero);
        }
        check_generation_match(self.package_generation, expected_package_generation).map_err(
            |reason| InitRefusal::Generation {
                field: GenerationField::Package,
                reason,
            },
        )?;
        if self.capset != expected_capset {
            return Err(InitRefusal::CapsetMismatch {
                found: self.capset,
                expected: expected_capset,
            });
        }
        if self.requested_endpoint_capacity == 0
            || self.requested_endpoint_capacity > HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION
        {
            return Err(InitRefusal::EndpointCapacityOutOfRange {
                found: self.requested_endpoint_capacity,
                max: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION,
            });
        }
        Ok(self.requested_endpoint_capacity)
    }
}

// ── HTS1 INIT reply ─────────────────────────────────────────────────────────

/// The INIT reply payload: what a successful `INIT` returns "through the C51
/// event-backed reply" (section 10.4, lines 1198-1201) — a nonzero session
/// generation, a CSPRNG-generated 128-bit capability, the bounded endpoint
/// capacity, and package/capset confirmation.
///
/// It travels inside one checked-out slot of the session's role-1 HVM1 pool,
/// after the `HVR1` header that `protocol/src/native_render.rs` owns; the HVR1
/// `status` field carries the operation's result, so this payload exists only on
/// success and carries no status of its own.
///
/// ⛔ It carries no host context ID, no ring index, no allocation identity, and
/// no handle — the endpoint descriptors that accompany it are session-local
/// ordinals ([`HeliosTranslationEndpointV1`]).
///
/// 56 bytes, padding-free.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub struct HeliosTranslationSessionReplyV1 {
    /// `== HELIOS_HTS1_REPLY_MAGIC`.
    pub magic: u32,
    /// `== HELIOS_HTS1_ABI_VERSION`.
    pub abi_version: u16,
    /// `== HELIOS_HTS1_REPLY_SIZE`.
    pub struct_size: u16,
    /// Package-generation confirmation: the exact generation the KMD is running.
    pub package_generation: u64,
    /// The new session's generation. Nonzero (section 10.4, line 1199).
    pub session_generation: u64,
    /// Low half of the CSPRNG-generated 128-bit admission capability.
    ///
    /// ⛔ Generated by the KMD, never by this crate. See the module header.
    pub capability_low: u64,
    /// High half of the CSPRNG-generated 128-bit admission capability.
    pub capability_high: u64,
    /// Capset confirmation: the exact Venus capset the host context was created
    /// with.
    pub capset: u32,
    /// The bounded endpoint capacity granted to this session:
    /// `1..=`[`HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION`], and never more than the
    /// request asked for.
    pub endpoint_capacity: u32,
    /// Reserved, zero.
    pub reserved: u64,
}

/// What a validated INIT reply admits: the three facts every later HQA1 is
/// checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosSessionAdmission {
    pub session_generation: u64,
    pub capability: HeliosSessionCapability,
    pub endpoint_capacity: u32,
}

impl HeliosTranslationSessionReplyV1 {
    /// Build a reply. `capability` is supplied by the caller — the KMD's CSPRNG
    /// output. This crate has no RNG and must never acquire one.
    #[inline]
    pub const fn new(
        package_generation: u64,
        session_generation: u64,
        capability: HeliosSessionCapability,
        capset: u32,
        endpoint_capacity: u32,
    ) -> Self {
        Self {
            magic: HELIOS_HTS1_REPLY_MAGIC,
            abi_version: HELIOS_HTS1_ABI_VERSION,
            struct_size: HELIOS_HTS1_REPLY_SIZE,
            package_generation,
            session_generation,
            capability_low: capability.low,
            capability_high: capability.high,
            capset,
            endpoint_capacity,
            reserved: 0,
        }
    }

    #[inline]
    pub const fn capability(&self) -> HeliosSessionCapability {
        HeliosSessionCapability::from_halves(self.capability_low, self.capability_high)
    }

    /// Total validation on the receiving (ICD) side.
    ///
    /// `requested_endpoint_capacity` is the value this translator put in its
    /// [`HeliosTranslationSessionInitV1`]; a reply that grants more than was
    /// asked for is a hard reject rather than a silent windfall.
    pub fn validate(
        &self,
        expected_package_generation: u64,
        expected_capset: u32,
        requested_endpoint_capacity: u32,
    ) -> Result<HeliosSessionAdmission, InitRefusal> {
        if self.magic != HELIOS_HTS1_REPLY_MAGIC {
            return Err(InitRefusal::BadMagic { found: self.magic });
        }
        if self.abi_version != HELIOS_HTS1_ABI_VERSION {
            return Err(InitRefusal::BadAbiVersion {
                found: self.abi_version,
            });
        }
        if self.struct_size != HELIOS_HTS1_REPLY_SIZE {
            return Err(InitRefusal::BadStructSize {
                found: self.struct_size,
            });
        }
        if self.reserved != 0 {
            return Err(InitRefusal::ReservedNonZero);
        }
        check_generation_match(self.package_generation, expected_package_generation).map_err(
            |reason| InitRefusal::Generation {
                field: GenerationField::Package,
                reason,
            },
        )?;
        if self.capset != expected_capset {
            return Err(InitRefusal::CapsetMismatch {
                found: self.capset,
                expected: expected_capset,
            });
        }
        if self.session_generation == 0 {
            return Err(InitRefusal::Generation {
                field: GenerationField::Session,
                reason: GenerationRefusal::Zero,
            });
        }
        let capability = self.capability();
        if capability.is_invalid() {
            return Err(InitRefusal::Capability(CapabilityRefusal::Absent));
        }
        if self.endpoint_capacity == 0
            || self.endpoint_capacity > HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION
        {
            return Err(InitRefusal::EndpointCapacityOutOfRange {
                found: self.endpoint_capacity,
                max: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION,
            });
        }
        if self.endpoint_capacity > requested_endpoint_capacity {
            return Err(InitRefusal::EndpointCapacityExceedsRequest {
                granted: self.endpoint_capacity,
                requested: requested_endpoint_capacity,
            });
        }
        Ok(HeliosSessionAdmission {
            session_generation: self.session_generation,
            capability,
            endpoint_capacity: self.endpoint_capacity,
        })
    }
}

/// Read an INIT reply out of a byte range of the checked-out reply slot.
/// Total: an exact-length check first, so no arm can panic.
pub fn parse_session_reply(bytes: &[u8]) -> Result<HeliosTranslationSessionReplyV1, InitRefusal> {
    let expected = core::mem::size_of::<HeliosTranslationSessionReplyV1>();
    if bytes.len() != expected {
        return Err(InitRefusal::PayloadSize {
            found: bytes.len(),
            expected,
        });
    }
    bytemuck::try_pod_read_unaligned::<HeliosTranslationSessionReplyV1>(bytes)
        .map_err(|_| InitRefusal::PayloadUnreadable)
}

// ── HQA1: outer-context attach ──────────────────────────────────────────────

/// `HeliosQueueAttachV1` (`HQA1`) — the complete create-context private driver
/// data that attaches one outer D3D context to one existing HTS1 session and
/// physical lower-queue endpoint.
///
/// HELIOS_PRESENT_SYNC_RETIREMENT.md section 10.4, lines 1215-1241. The table is
/// reproduced field-by-field below and every offset is asserted at the bottom of
/// this file:
///
/// | Offset | Size | Field | Rule |
/// |---:|---:|---|---|
/// | 0 | 4 | magic | `0x31415148` (`HQA1`) |
/// | 4 | 2 | ABI version | `1` |
/// | 6 | 2 | structure size | `72` |
/// | 8 | 8 | package generation | exact atomic-package generation |
/// | 16 | 8 | session generation | exact nonzero HTS1 generation |
/// | 24 | 8 | capability low | unpredictable KMD-returned value |
/// | 32 | 8 | capability high | unpredictable KMD-returned value |
/// | 40 | 4 | endpoint ID | exact physical lower VkQueue endpoint, nonzero |
/// | 44 | 4 | engine class | exact graphics/compute/copy class admitted for the outer context |
/// | 48 | 4 | queue family | diagnostic cross-check only; never identity |
/// | 52 | 4 | queue index | diagnostic cross-check only; never identity |
/// | 56 | 8 | context generation | UMD-chosen, nonzero, monotonically increasing and never reused within this HTS1 session |
/// | 64 | 4 | flags | bit 0 D3D11 physical Render, bit 1 D3D12 virtual Submit; exactly one set |
/// | 68 | 4 | reserved | zero |
///
/// # It is read exactly once
///
/// "KMD validates HQA1 once during `DxgkDdiCreateContext`, rejects a zero or
/// duplicate live context generation, stores that generation in the new context,
/// takes a strong direct reference to that session/endpoint, and erases no
/// capability into later DMA." After that the live KMD context object *is* the
/// identity; HOB1/HOS1 repeat the stored generation only as an anti-stale
/// cross-check and no later lookup uses the number (invariant 13).
///
/// # It carries nothing that could be identity
///
/// No pointer, KMT handle, PID, allocation/resource identity, host object ID, or
/// synchronization object — see the module header. The two "diagnostic
/// cross-check" ordinals are compared and never used to select anything, and
/// `endpoint_id` is a session-local ordinal rather than a host ring index.
///
/// 72 bytes, padding-free.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub struct HeliosQueueAttachV1 {
    /// `== HELIOS_HQA1_MAGIC`.
    pub magic: u32,
    /// `== HELIOS_HQA1_ABI_VERSION`.
    pub abi_version: u16,
    /// `== HELIOS_HQA1_SIZE`.
    pub struct_size: u16,
    /// Exact atomic-package generation.
    pub package_generation: u64,
    /// Exact nonzero HTS1 session generation from the INIT reply.
    pub session_generation: u64,
    /// Low half of the session's admission capability.
    pub capability_low: u64,
    /// High half of the session's admission capability.
    pub capability_high: u64,
    /// Exact physical lower-queue endpoint ordinal, nonzero. NOT a ring index.
    pub endpoint_id: u32,
    /// `HELIOS_ENGINE_CLASS_*` admitted for this outer context.
    pub engine_class: u32,
    /// Diagnostic cross-check only; never identity.
    pub queue_family: u32,
    /// Diagnostic cross-check only; never identity.
    pub queue_index: u32,
    /// UMD-chosen, nonzero, monotonically increasing, never reused within this
    /// HTS1 session.
    pub context_generation: u64,
    /// `HELIOS_HQA1_FLAG_*`; exactly one bit set.
    pub flags: u32,
    /// Reserved, zero.
    pub reserved: u32,
}

/// What the KMD already knows when an HQA1 arrives: everything the packet is
/// compared *against*. Building this from the raw device's live session — never
/// from the packet — is what keeps attach a check rather than a lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosAttachExpectation {
    /// The KMD's own atomic package generation.
    pub package_generation: u64,
    /// The live session's generation.
    pub session_generation: u64,
    /// The live session's capability (or [`HeliosSessionCapability::INVALID`]
    /// once reset/removal has invalidated it — which then refuses every attach).
    pub capability: HeliosSessionCapability,
    /// The exact endpoint the packet claims, as the session recorded it. The
    /// caller resolves it by ordinal from its own bounded endpoint array; this
    /// validator never searches.
    pub endpoint: HeliosTranslationEndpointV1,
    /// The session's granted endpoint capacity.
    pub endpoint_capacity: u32,
    /// Highest context generation this session has already admitted (0 = none).
    pub highest_context_generation: u64,
}

/// What a validated HQA1 admits. Everything a caller needs, already decoded, so
/// no caller re-reads the raw `u32`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosAttachAdmission {
    pub kind: HeliosOuterContextKind,
    pub endpoint_id: u32,
    pub engine_class: HeliosEngineClass,
    pub context_generation: u64,
}

/// Why an HQA1 attach was refused. Every rejection in section 10.4 and in the
/// failure table (line 2861) has a name here; nothing returns a bare `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachRefusal {
    /// The create-context private data was not exactly [`HELIOS_HQA1_SIZE`]
    /// bytes. HQA1 is "the complete create-context private data" (line 1218), so
    /// a longer buffer is as malformed as a shorter one.
    PrivateDataSize { found: usize, expected: usize },
    /// The buffer was the right length but could not be read as HQA1.
    /// Unreachable after the length check; present so the parse path has no
    /// panicking arm.
    PrivateDataUnreadable,
    /// Magic is not [`HELIOS_HQA1_MAGIC`].
    BadMagic { found: u32 },
    /// ABI version is not [`HELIOS_HQA1_ABI_VERSION`]. No version fallback
    /// exists in this package (section 3).
    BadAbiVersion { found: u16 },
    /// `struct_size` is not [`HELIOS_HQA1_SIZE`].
    BadStructSize { found: u16 },
    /// The reserved word was nonzero (section 10.4, table row `offset 68`;
    /// injected reserved bytes must fail — section 18, line 4865).
    ReservedNonZero { found: u32 },
    /// No flag bit was set. "exactly one set."
    NoContextKindFlag,
    /// Both defined flag bits were set. "exactly one set."
    AmbiguousContextKindFlags,
    /// A bit outside [`HELIOS_HQA1_FLAGS_MASK`] was set.
    UnknownFlagBits { found: u32 },
    /// A generation check failed.
    Generation {
        field: GenerationField,
        reason: GenerationRefusal,
    },
    /// The capability check failed.
    Capability(CapabilityRefusal),
    /// The endpoint descriptor in the packet, or the session's own capacity,
    /// failed validation.
    Endpoint(EndpointRefusal),
    /// The packet's endpoint ordinal is not the endpoint the caller resolved.
    EndpointMismatch { found: u32, expected: u32 },
    /// The packet's engine class is not the class of that endpoint.
    EngineClassMismatch { found: u32, expected: u32 },
    /// The packet's diagnostic queue-family ordinal disagrees with the
    /// endpoint's. Cross-check: compare and reject, never look up.
    QueueFamilyMismatch { found: u32, expected: u32 },
    /// The packet's diagnostic queue-index ordinal disagrees with the
    /// endpoint's. Cross-check: compare and reject, never look up.
    QueueIndexMismatch { found: u32, expected: u32 },
}

impl HeliosQueueAttachV1 {
    /// Build the packet UMD-side. `capability` and `session_generation` come
    /// from the INIT reply this translator already validated; `endpoint` is the
    /// descriptor for the physical lower queue the bridge selected;
    /// `context_generation` is the UMD's own strictly increasing counter for
    /// this session.
    #[inline]
    pub const fn new(
        package_generation: u64,
        session_generation: u64,
        capability: HeliosSessionCapability,
        endpoint: HeliosTranslationEndpointV1,
        context_generation: u64,
        kind: HeliosOuterContextKind,
    ) -> Self {
        Self {
            magic: HELIOS_HQA1_MAGIC,
            abi_version: HELIOS_HQA1_ABI_VERSION,
            struct_size: HELIOS_HQA1_SIZE,
            package_generation,
            session_generation,
            capability_low: capability.low,
            capability_high: capability.high,
            endpoint_id: endpoint.endpoint_id,
            engine_class: endpoint.engine_class,
            queue_family: endpoint.queue_family,
            queue_index: endpoint.queue_index,
            context_generation,
            flags: kind.wire(),
            reserved: 0,
        }
    }

    #[inline]
    pub const fn capability(&self) -> HeliosSessionCapability {
        HeliosSessionCapability::from_halves(self.capability_low, self.capability_high)
    }

    /// Decode `flags` under the "exactly one set" rule (section 10.4, table row
    /// `offset 64`). Unknown bits are rejected before the count, so a packet
    /// that sets one known bit plus an unknown one can never be admitted as
    /// that known kind.
    pub fn outer_context_kind(&self) -> Result<HeliosOuterContextKind, AttachRefusal> {
        if self.flags & !HELIOS_HQA1_FLAGS_MASK != 0 {
            return Err(AttachRefusal::UnknownFlagBits { found: self.flags });
        }
        match self.flags {
            0 => Err(AttachRefusal::NoContextKindFlag),
            HELIOS_HQA1_FLAG_D3D11_PHYSICAL => Ok(HeliosOuterContextKind::D3d11PhysicalRender),
            HELIOS_HQA1_FLAG_D3D12_VIRTUAL => Ok(HeliosOuterContextKind::D3d12VirtualSubmit),
            _ => Err(AttachRefusal::AmbiguousContextKindFlags),
        }
    }

    /// The one-time attach validation `DxgkDdiCreateContext` performs.
    ///
    /// Total, pure, side-effect-free: it compares the packet against facts the
    /// caller already holds and returns either the decoded admission or a named
    /// refusal. It performs no lookup, resolves no handle, and touches no
    /// session list — the caller resolved `expect.endpoint` from the raw
    /// device's own session before calling (invariant 10: "submit, Present,
    /// allocation open, and display paths never search that list", and attach
    /// itself consults the session only through the owning device reference).
    ///
    /// Order is fail-closed and structural first: shape, then reserved/flags,
    /// then generations, then the capability, then the endpoint cross-checks,
    /// and the monotonic context generation last — so a malformed packet is
    /// rejected before its capability is even compared.
    pub fn validate(
        &self,
        expect: &HeliosAttachExpectation,
    ) -> Result<HeliosAttachAdmission, AttachRefusal> {
        if self.magic != HELIOS_HQA1_MAGIC {
            return Err(AttachRefusal::BadMagic { found: self.magic });
        }
        if self.abi_version != HELIOS_HQA1_ABI_VERSION {
            return Err(AttachRefusal::BadAbiVersion {
                found: self.abi_version,
            });
        }
        if self.struct_size != HELIOS_HQA1_SIZE {
            return Err(AttachRefusal::BadStructSize {
                found: self.struct_size,
            });
        }
        if self.reserved != 0 {
            return Err(AttachRefusal::ReservedNonZero {
                found: self.reserved,
            });
        }
        let kind = self.outer_context_kind()?;

        check_generation_match(self.package_generation, expect.package_generation).map_err(
            |reason| AttachRefusal::Generation {
                field: GenerationField::Package,
                reason,
            },
        )?;
        check_generation_match(self.session_generation, expect.session_generation).map_err(
            |reason| AttachRefusal::Generation {
                field: GenerationField::Session,
                reason,
            },
        )?;

        self.capability()
            .check_match(expect.capability)
            .map_err(AttachRefusal::Capability)?;

        // The session's own descriptor is validated too: a caller that resolved
        // a malformed endpoint must not admit a context onto it.
        let expected_class = expect
            .endpoint
            .validate(expect.endpoint_capacity)
            .map_err(AttachRefusal::Endpoint)?;
        if self.endpoint_id == 0 {
            return Err(AttachRefusal::Endpoint(EndpointRefusal::ZeroEndpointId));
        }
        if self.endpoint_id != expect.endpoint.endpoint_id {
            return Err(AttachRefusal::EndpointMismatch {
                found: self.endpoint_id,
                expected: expect.endpoint.endpoint_id,
            });
        }
        let class =
            HeliosEngineClass::from_wire(self.engine_class).map_err(AttachRefusal::Endpoint)?;
        if class != expected_class {
            return Err(AttachRefusal::EngineClassMismatch {
                found: self.engine_class,
                expected: expect.endpoint.engine_class,
            });
        }
        if self.queue_family != expect.endpoint.queue_family {
            return Err(AttachRefusal::QueueFamilyMismatch {
                found: self.queue_family,
                expected: expect.endpoint.queue_family,
            });
        }
        if self.queue_index != expect.endpoint.queue_index {
            return Err(AttachRefusal::QueueIndexMismatch {
                found: self.queue_index,
                expected: expect.endpoint.queue_index,
            });
        }

        check_context_generation(self.context_generation, expect.highest_context_generation)
            .map_err(|reason| AttachRefusal::Generation {
                field: GenerationField::Context,
                reason,
            })?;

        Ok(HeliosAttachAdmission {
            kind,
            endpoint_id: self.endpoint_id,
            engine_class: class,
            context_generation: self.context_generation,
        })
    }
}

/// Read HQA1 out of the create-context private driver data.
///
/// Total: the exact-length check comes first, so no arm can panic — a panic in a
/// DDI is a silent graphics deadlock (CLAUDE.md key invariants).
pub fn parse_create_context_private_data(
    private_data: &[u8],
) -> Result<HeliosQueueAttachV1, AttachRefusal> {
    let expected = core::mem::size_of::<HeliosQueueAttachV1>();
    if private_data.len() != expected {
        return Err(AttachRefusal::PrivateDataSize {
            found: private_data.len(),
            expected,
        });
    }
    bytemuck::try_pod_read_unaligned::<HeliosQueueAttachV1>(private_data)
        .map_err(|_| AttachRefusal::PrivateDataUnreadable)
}

// ── Control-opcode classification (section 10.4, lines 1335-1366) ───────────
//
// "Control requests are divided statically" into three classes. This file owns
// the CLASS VOCABULARY and the carrier contract only. The per-opcode allowlist —
// which generated Venus opcode is in which class — is generated in Mesa and in
// the KMD from the same schema and is deliberately NOT duplicated here: "The
// allowlist and maximum input/reply size are part of the package generation"
// (line 1362), and a second hand-maintained copy of it in this crate would be a
// mirror that can drift, which is the class of defect CLAUDE.md's
// `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` entry warns about.

/// Wire encoding of [`HeliosControlOpcodeClass::Pure`].
pub const HELIOS_CONTROL_CLASS_PURE: u32 = 1;
/// Wire encoding of [`HeliosControlOpcodeClass::OuterAllocationBacked`].
pub const HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED: u32 = 2;
/// Wire encoding of [`HeliosControlOpcodeClass::GpuDependentSynchronous`].
pub const HELIOS_CONTROL_CLASS_GPU_DEPENDENT: u32 = 3;

/// The static class of one translated control request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosControlOpcodeClass {
    /// **pure control** — bounded instance/device creation, Vulkan object
    /// creation or metadata that cannot dereference an outer D3D/resource
    /// allocation, capability and memory-requirement queries,
    /// descriptor-independent pipeline compilation, and their finite replies.
    /// May execute on HVC1/ring zero. Its only allocation access is the one
    /// checked-out slot of the session-owned HVM1 reply pool named writable in
    /// that Render — "this one session-scratch exception is not an outer
    /// allocation and cannot contain a host resource ID" (lines 1337-1345).
    Pure,
    /// **outer-allocation-backed** — memory allocation/materialization, bind,
    /// map/cache ownership, transfer, query-result storage, destruction, or
    /// object materialization that can touch an outer D3D/Vulkan resource
    /// allocation. Represented by frontend/deferred handles; real only in the
    /// first actual outer batch whose allocation list/GPUVA names every exact
    /// allocation. "ring zero never receives or resolves such an allocation"
    /// (lines 1346-1353).
    OuterAllocationBacked,
    /// **GPU-dependent synchronous** — queue/device idle, fence status/wait,
    /// query `WAIT`, and teardown. The owning outer UMD must first submit
    /// pending batches, signal that context's private HQC1 value, and perform one
    /// event-backed CPU wait; only after that exact endpoint milestone completes
    /// may HVC1 fetch bounded result bytes (lines 1354-1360).
    GpuDependentSynchronous,
}

impl HeliosControlOpcodeClass {
    #[inline]
    pub const fn wire(self) -> u32 {
        match self {
            HeliosControlOpcodeClass::Pure => HELIOS_CONTROL_CLASS_PURE,
            HeliosControlOpcodeClass::OuterAllocationBacked => {
                HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED
            }
            HeliosControlOpcodeClass::GpuDependentSynchronous => HELIOS_CONTROL_CLASS_GPU_DEPENDENT,
        }
    }

    #[inline]
    pub fn from_wire(value: u32) -> Result<Self, ControlClassRefusal> {
        match value {
            HELIOS_CONTROL_CLASS_PURE => Ok(HeliosControlOpcodeClass::Pure),
            HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED => {
                Ok(HeliosControlOpcodeClass::OuterAllocationBacked)
            }
            HELIOS_CONTROL_CLASS_GPU_DEPENDENT => {
                Ok(HeliosControlOpcodeClass::GpuDependentSynchronous)
            }
            found => Err(ControlClassRefusal::UnknownClass { found }),
        }
    }
}

/// Which carrier a control request arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosControlCarrier {
    /// The raw HVC1 control context on host ring zero — CPU/decode-only, and
    /// never a GPU completion boundary (invariant 12, invariant 15).
    RingZeroControl,
    /// An actual outer batch on an HQA1-attached context, carrying the exact
    /// WDDM allocation list / GPUVA uses.
    OuterBatch,
}

/// Why a control request was refused by classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlClassRefusal {
    /// The declared class value is not one of the three.
    UnknownClass { found: u32 },
    /// The request declares a class other than the one the generated allowlist
    /// assigns to its opcode. "An opcode in the wrong class ... fails device
    /// creation or removes that device" (lines 1363-1365).
    ClassMismatch {
        declared: HeliosControlOpcodeClass,
        allowlisted: HeliosControlOpcodeClass,
    },
    /// An outer-allocation-backed request reached the control ring. Ring zero
    /// never receives or resolves an outer allocation (line 1352).
    OuterAllocationOnControlRing,
    /// A GPU-dependent synchronous request reached the control ring before its
    /// owning outer context's HQC1 milestone completed (lines 1354-1359).
    GpuDependentWithoutOuterMilestone,
    /// A GPU-dependent synchronous request was encoded into an outer batch. Its
    /// bounded result fetch is an HVC1/ring-zero operation performed *after* the
    /// milestone, never batch work.
    GpuDependentOnOuterBatch,
}

/// The classification contract: decide whether one control request may execute
/// on the carrier it arrived on.
///
/// `declared_class` is the class the request itself states; `allowlisted` is the
/// class the *generated* per-opcode allowlist assigns to that opcode (owned by
/// Mesa/KMD generators, never by this crate). They must agree — a request that
/// declares a weaker class than its opcode's is exactly the escalation the
/// static division exists to stop.
///
/// `outer_milestone_joined` states whether the owning outer context's HQC1 value
/// has already completed; it is meaningful only for
/// [`HeliosControlOpcodeClass::GpuDependentSynchronous`].
///
/// ⚠ Only the prohibitions section 10.4 states are enforced. A *pure* request on
/// an outer batch is not prohibited by the doc (the pure class's ring-zero
/// carrier is permissive — "may execute on HVC1/ring zero"), so it is admitted;
/// inventing a prohibition the doc does not state would refuse a legal encoder.
pub fn admit_control_opcode(
    declared_class: u32,
    allowlisted: HeliosControlOpcodeClass,
    carrier: HeliosControlCarrier,
    outer_milestone_joined: bool,
) -> Result<HeliosControlOpcodeClass, ControlClassRefusal> {
    let declared = HeliosControlOpcodeClass::from_wire(declared_class)?;
    if declared != allowlisted {
        return Err(ControlClassRefusal::ClassMismatch {
            declared,
            allowlisted,
        });
    }
    match (declared, carrier) {
        (HeliosControlOpcodeClass::Pure, _) => Ok(declared),
        (HeliosControlOpcodeClass::OuterAllocationBacked, HeliosControlCarrier::OuterBatch) => {
            Ok(declared)
        }
        (
            HeliosControlOpcodeClass::OuterAllocationBacked,
            HeliosControlCarrier::RingZeroControl,
        ) => Err(ControlClassRefusal::OuterAllocationOnControlRing),
        (
            HeliosControlOpcodeClass::GpuDependentSynchronous,
            HeliosControlCarrier::RingZeroControl,
        ) => {
            if outer_milestone_joined {
                Ok(declared)
            } else {
                Err(ControlClassRefusal::GpuDependentWithoutOuterMilestone)
            }
        }
        (HeliosControlOpcodeClass::GpuDependentSynchronous, HeliosControlCarrier::OuterBatch) => {
            Err(ControlClassRefusal::GpuDependentOnOuterBatch)
        }
    }
}

// ── Compile-time layout assertions ──────────────────────────────────────────
//
// Section 17.1 requires size/alignment/offset assertions for every record here.
// A wrong offset must break the BUILD, not a test — the `_pad`/`adopt_resource_id`
// history in `crate::wddm` is the standing example of a field whose meaning
// changed while a size-only assert kept passing.

const _: () = {
    // ── HQA1, section 10.4 lines 1220-1235 ──
    assert!(core::mem::size_of::<HeliosQueueAttachV1>() == 72);
    assert!(core::mem::size_of::<HeliosQueueAttachV1>() == HELIOS_HQA1_SIZE as usize);
    assert!(core::mem::align_of::<HeliosQueueAttachV1>() == 8);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, session_generation) == 16);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, capability_low) == 24);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, capability_high) == 32);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, endpoint_id) == 40);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, engine_class) == 44);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, queue_family) == 48);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, queue_index) == 52);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, context_generation) == 56);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, flags) == 64);
    assert!(core::mem::offset_of!(HeliosQueueAttachV1, reserved) == 68);

    // ── HTS1 INIT request ──
    assert!(core::mem::size_of::<HeliosTranslationSessionInitV1>() == 32);
    assert!(
        core::mem::size_of::<HeliosTranslationSessionInitV1>() == HELIOS_HTS1_INIT_SIZE as usize
    );
    assert!(core::mem::align_of::<HeliosTranslationSessionInitV1>() == 8);
    assert!(core::mem::offset_of!(HeliosTranslationSessionInitV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosTranslationSessionInitV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosTranslationSessionInitV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosTranslationSessionInitV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosTranslationSessionInitV1, capset) == 16);
    assert!(
        core::mem::offset_of!(HeliosTranslationSessionInitV1, requested_endpoint_capacity) == 20
    );
    assert!(core::mem::offset_of!(HeliosTranslationSessionInitV1, reserved) == 24);

    // ── HTS1 INIT reply ──
    assert!(core::mem::size_of::<HeliosTranslationSessionReplyV1>() == 56);
    assert!(
        core::mem::size_of::<HeliosTranslationSessionReplyV1>() == HELIOS_HTS1_REPLY_SIZE as usize
    );
    assert!(core::mem::align_of::<HeliosTranslationSessionReplyV1>() == 8);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, session_generation) == 16);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, capability_low) == 24);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, capability_high) == 32);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, capset) == 40);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, endpoint_capacity) == 44);
    assert!(core::mem::offset_of!(HeliosTranslationSessionReplyV1, reserved) == 48);

    // ── Endpoint descriptor ──
    assert!(core::mem::size_of::<HeliosTranslationEndpointV1>() == 16);
    assert!(
        core::mem::size_of::<HeliosTranslationEndpointV1>() == HELIOS_HTS1_ENDPOINT_SIZE as usize
    );
    assert!(core::mem::align_of::<HeliosTranslationEndpointV1>() == 4);
    assert!(core::mem::offset_of!(HeliosTranslationEndpointV1, endpoint_id) == 0);
    assert!(core::mem::offset_of!(HeliosTranslationEndpointV1, engine_class) == 4);
    assert!(core::mem::offset_of!(HeliosTranslationEndpointV1, queue_family) == 8);
    assert!(core::mem::offset_of!(HeliosTranslationEndpointV1, queue_index) == 12);

    // ── Magics really are their ASCII tags in little-endian byte order ──
    assert!(HELIOS_HQA1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HQA1_MAGIC.to_le_bytes()[1] == b'Q');
    assert!(HELIOS_HQA1_MAGIC.to_le_bytes()[2] == b'A');
    assert!(HELIOS_HQA1_MAGIC.to_le_bytes()[3] == b'1');
    assert!(HELIOS_HTS1_INIT_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HTS1_INIT_MAGIC.to_le_bytes()[1] == b'T');
    assert!(HELIOS_HTS1_INIT_MAGIC.to_le_bytes()[2] == b'S');
    assert!(HELIOS_HTS1_INIT_MAGIC.to_le_bytes()[3] == b'1');
    assert!(HELIOS_HTS1_REPLY_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HTS1_REPLY_MAGIC.to_le_bytes()[1] == b'T');
    assert!(HELIOS_HTS1_REPLY_MAGIC.to_le_bytes()[2] == b'R');
    assert!(HELIOS_HTS1_REPLY_MAGIC.to_le_bytes()[3] == b'1');
    // The three record magics must stay mutually distinct: a decoder that tries
    // more than one arm on the same bytes must never accept the wrong record.
    assert!(HELIOS_HQA1_MAGIC != HELIOS_HTS1_INIT_MAGIC);
    assert!(HELIOS_HQA1_MAGIC != HELIOS_HTS1_REPLY_MAGIC);
    assert!(HELIOS_HTS1_INIT_MAGIC != HELIOS_HTS1_REPLY_MAGIC);

    // ── HQA1 flags agree numerically with HOB1's (section 10.4, `offset 44`) ──
    assert!(HELIOS_HQA1_FLAG_D3D11_PHYSICAL == 1);
    assert!(HELIOS_HQA1_FLAG_D3D12_VIRTUAL == 2);
    assert!(HELIOS_HQA1_FLAGS_MASK == 3);

    // ── Bounded limits ──
    // Every endpoint consumes one unique nonzero host ring, and the virtio-gpu
    // header's `ring_idx` is one byte.
    assert!(HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION <= HELIOS_HTS1_MAX_RING_INDEX);
    assert!(HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION > 0);
    assert!(HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS > 0);
    assert!(HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES == 64);
    assert!(HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES == 15 * 1024 * 1024);
    // A single context must not be able to exhaust the shared endpoint FIFO.
    assert!(HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH > HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES);

    // ── Engine classes are dense, 1-based, and never zero ──
    assert!(HELIOS_ENGINE_CLASS_GRAPHICS == 1);
    assert!(HELIOS_ENGINE_CLASS_COMPUTE == 2);
    assert!(HELIOS_ENGINE_CLASS_COPY == 3);
    // ── Control classes likewise ──
    assert!(HELIOS_CONTROL_CLASS_PURE == 1);
    assert!(HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED == 2);
    assert!(HELIOS_CONTROL_CLASS_GPU_DEPENDENT == 3);
};

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: u64 = 0x0000_0001_0000_0007;
    const SESSION: u64 = 0x1234_5678_9abc_def0;
    const CAPSET: u32 = 4; // VIRTIO_GPU_CAPSET_VENUS

    fn capability() -> HeliosSessionCapability {
        HeliosSessionCapability::from_halves(0xa1a2_a3a4_a5a6_a7a8, 0xb1b2_b3b4_b5b6_b7b8)
    }

    fn endpoint() -> HeliosTranslationEndpointV1 {
        HeliosTranslationEndpointV1::new(2, HeliosEngineClass::Graphics, 0, 1)
    }

    fn expectation() -> HeliosAttachExpectation {
        HeliosAttachExpectation {
            package_generation: PKG,
            session_generation: SESSION,
            capability: capability(),
            endpoint: endpoint(),
            endpoint_capacity: 4,
            highest_context_generation: 7,
        }
    }

    fn attach() -> HeliosQueueAttachV1 {
        HeliosQueueAttachV1::new(
            PKG,
            SESSION,
            capability(),
            endpoint(),
            8,
            HeliosOuterContextKind::D3d12VirtualSubmit,
        )
    }

    #[test]
    fn a_well_formed_attach_admits_exactly_once() {
        let a = attach();
        assert_eq!(
            a.validate(&expectation()),
            Ok(HeliosAttachAdmission {
                kind: HeliosOuterContextKind::D3d12VirtualSubmit,
                endpoint_id: 2,
                engine_class: HeliosEngineClass::Graphics,
                context_generation: 8,
            })
        );
        // The same packet cannot be replayed: after admission the session's
        // watermark has moved to 8, and 8 is no longer strictly increasing.
        let mut exp = expectation();
        exp.highest_context_generation = 8;
        assert_eq!(
            a.validate(&exp),
            Err(AttachRefusal::Generation {
                field: GenerationField::Context,
                reason: GenerationRefusal::NotMonotonic {
                    found: 8,
                    watermark: 8
                },
            })
        );
    }

    #[test]
    fn exactly_one_context_kind_flag() {
        let mut a = attach();
        a.flags = 0;
        assert_eq!(
            a.outer_context_kind(),
            Err(AttachRefusal::NoContextKindFlag)
        );
        a.flags = HELIOS_HQA1_FLAG_D3D11_PHYSICAL | HELIOS_HQA1_FLAG_D3D12_VIRTUAL;
        assert_eq!(
            a.outer_context_kind(),
            Err(AttachRefusal::AmbiguousContextKindFlags)
        );
        a.flags = HELIOS_HQA1_FLAG_D3D11_PHYSICAL | 0x8000_0000;
        assert_eq!(
            a.outer_context_kind(),
            Err(AttachRefusal::UnknownFlagBits { found: a.flags })
        );
        a.flags = HELIOS_HQA1_FLAG_D3D11_PHYSICAL;
        assert_eq!(
            a.outer_context_kind(),
            Ok(HeliosOuterContextKind::D3d11PhysicalRender)
        );
    }

    #[test]
    fn shape_reserved_and_generation_injections_all_fail() {
        let good = expectation();

        let mut a = attach();
        a.magic = 0;
        assert_eq!(a.validate(&good), Err(AttachRefusal::BadMagic { found: 0 }));

        let mut a = attach();
        a.abi_version = 2;
        assert_eq!(
            a.validate(&good),
            Err(AttachRefusal::BadAbiVersion { found: 2 })
        );

        let mut a = attach();
        a.struct_size = 64;
        assert_eq!(
            a.validate(&good),
            Err(AttachRefusal::BadStructSize { found: 64 })
        );

        let mut a = attach();
        a.reserved = 1;
        assert_eq!(
            a.validate(&good),
            Err(AttachRefusal::ReservedNonZero { found: 1 })
        );

        let mut a = attach();
        a.package_generation = PKG + 1;
        assert_eq!(
            a.validate(&good),
            Err(AttachRefusal::Generation {
                field: GenerationField::Package,
                reason: GenerationRefusal::Mismatch {
                    found: PKG + 1,
                    expected: PKG
                },
            })
        );

        let mut a = attach();
        a.session_generation = 0;
        assert_eq!(
            a.validate(&good),
            Err(AttachRefusal::Generation {
                field: GenerationField::Session,
                reason: GenerationRefusal::Zero,
            })
        );

        let mut a = attach();
        a.context_generation = 0;
        assert_eq!(
            a.validate(&good),
            Err(AttachRefusal::Generation {
                field: GenerationField::Context,
                reason: GenerationRefusal::Zero,
            })
        );
    }

    #[test]
    fn capability_is_matched_and_invalidation_refuses_everything() {
        let mut a = attach();
        a.capability_low = 0;
        a.capability_high = 0;
        assert_eq!(
            a.validate(&expectation()),
            Err(AttachRefusal::Capability(CapabilityRefusal::Absent))
        );

        let mut a = attach();
        a.capability_high ^= 1;
        assert_eq!(
            a.validate(&expectation()),
            Err(AttachRefusal::Capability(CapabilityRefusal::Mismatch))
        );

        // Reset invalidated the session before waking waiters (invariant 16).
        let mut exp = expectation();
        exp.capability = HeliosSessionCapability::INVALID;
        assert_eq!(
            attach().validate(&exp),
            Err(AttachRefusal::Capability(
                CapabilityRefusal::SessionInvalidated
            ))
        );
    }

    #[test]
    fn endpoint_cross_checks_reject_but_never_select() {
        let exp = expectation();

        let mut a = attach();
        a.endpoint_id = 3;
        assert_eq!(
            a.validate(&exp),
            Err(AttachRefusal::EndpointMismatch {
                found: 3,
                expected: 2
            })
        );

        let mut a = attach();
        a.engine_class = HELIOS_ENGINE_CLASS_COPY;
        assert_eq!(
            a.validate(&exp),
            Err(AttachRefusal::EngineClassMismatch {
                found: HELIOS_ENGINE_CLASS_COPY,
                expected: HELIOS_ENGINE_CLASS_GRAPHICS,
            })
        );

        let mut a = attach();
        a.engine_class = 9;
        assert_eq!(
            a.validate(&exp),
            Err(AttachRefusal::Endpoint(
                EndpointRefusal::UnknownEngineClass { found: 9 }
            ))
        );

        // Diagnostic ordinals are compared, not used to find anything.
        let mut a = attach();
        a.queue_family = 1;
        assert_eq!(
            a.validate(&exp),
            Err(AttachRefusal::QueueFamilyMismatch {
                found: 1,
                expected: 0
            })
        );
        let mut a = attach();
        a.queue_index = 5;
        assert_eq!(
            a.validate(&exp),
            Err(AttachRefusal::QueueIndexMismatch {
                found: 5,
                expected: 1
            })
        );
    }

    #[test]
    fn endpoint_descriptor_bounds_and_control_sentinel() {
        assert_eq!(endpoint().validate(4), Ok(HeliosEngineClass::Graphics));
        assert_eq!(
            endpoint().validate(1),
            Err(EndpointRefusal::EndpointIdOutOfRange {
                found: 2,
                capacity: 1
            })
        );
        assert_eq!(
            endpoint().validate(0),
            Err(EndpointRefusal::CapacityOutOfRange {
                found: 0,
                max: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION
            })
        );
        assert_eq!(
            endpoint().validate(HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION + 1),
            Err(EndpointRefusal::CapacityOutOfRange {
                found: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION + 1,
                max: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION
            })
        );

        let mut zero = endpoint();
        zero.endpoint_id = 0;
        assert_eq!(zero.validate(4), Err(EndpointRefusal::ZeroEndpointId));

        // HVC1's control encoding can never describe a physical endpoint.
        let mut control = endpoint();
        control.queue_family = u32::MAX;
        control.queue_index = u32::MAX;
        assert_eq!(
            control.validate(4),
            Err(EndpointRefusal::ControlSentinelEndpoint)
        );
    }

    #[test]
    fn private_data_must_be_exactly_seventy_two_bytes() {
        let a = attach();
        let bytes = bytemuck::bytes_of(&a);
        assert_eq!(bytes.len(), 72);
        assert_eq!(
            parse_create_context_private_data(bytes).unwrap().magic,
            a.magic
        );
        assert_eq!(
            parse_create_context_private_data(&bytes[..71]),
            Err(AttachRefusal::PrivateDataSize {
                found: 71,
                expected: 72
            })
        );
        // A longer buffer is as malformed as a shorter one: HQA1 is the
        // COMPLETE create-context private data.
        let mut longer = [0u8; 80];
        longer[..72].copy_from_slice(bytes);
        assert_eq!(
            parse_create_context_private_data(&longer),
            Err(AttachRefusal::PrivateDataSize {
                found: 80,
                expected: 72
            })
        );
    }

    #[test]
    fn init_request_and_reply_round_trip_and_reject() {
        let req = HeliosTranslationSessionInitV1::new(PKG, CAPSET, 4);
        assert_eq!(req.validate(PKG, CAPSET), Ok(4));
        assert_eq!(
            req.validate(PKG + 1, CAPSET),
            Err(InitRefusal::Generation {
                field: GenerationField::Package,
                reason: GenerationRefusal::Mismatch {
                    found: PKG,
                    expected: PKG + 1
                },
            })
        );
        assert_eq!(
            req.validate(PKG, 1),
            Err(InitRefusal::CapsetMismatch {
                found: CAPSET,
                expected: 1
            })
        );
        let mut over = req;
        over.requested_endpoint_capacity = HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION + 1;
        assert_eq!(
            over.validate(PKG, CAPSET),
            Err(InitRefusal::EndpointCapacityOutOfRange {
                found: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION + 1,
                max: HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION,
            })
        );

        let reply = HeliosTranslationSessionReplyV1::new(PKG, SESSION, capability(), CAPSET, 3);
        assert_eq!(
            reply.validate(PKG, CAPSET, 4),
            Ok(HeliosSessionAdmission {
                session_generation: SESSION,
                capability: capability(),
                endpoint_capacity: 3,
            })
        );
        // Granting more than was requested is a hard reject.
        assert_eq!(
            reply.validate(PKG, CAPSET, 2),
            Err(InitRefusal::EndpointCapacityExceedsRequest {
                granted: 3,
                requested: 2
            })
        );

        let mut zero_session = reply;
        zero_session.session_generation = 0;
        assert_eq!(
            zero_session.validate(PKG, CAPSET, 4),
            Err(InitRefusal::Generation {
                field: GenerationField::Session,
                reason: GenerationRefusal::Zero,
            })
        );

        let mut no_cap = reply;
        no_cap.capability_low = 0;
        no_cap.capability_high = 0;
        assert_eq!(
            no_cap.validate(PKG, CAPSET, 4),
            Err(InitRefusal::Capability(CapabilityRefusal::Absent))
        );

        let mut reserved = reply;
        reserved.reserved = 1;
        assert_eq!(
            reserved.validate(PKG, CAPSET, 4),
            Err(InitRefusal::ReservedNonZero)
        );

        // The two directions never accept each other's bytes.
        let req_bytes = bytemuck::bytes_of(&req);
        assert_eq!(
            parse_session_reply(req_bytes),
            Err(InitRefusal::PayloadSize {
                found: 32,
                expected: 56
            })
        );
    }

    #[test]
    fn control_classes_enforce_their_carriers() {
        use HeliosControlCarrier::*;
        use HeliosControlOpcodeClass::*;

        // Pure control is what ring zero exists for.
        assert_eq!(
            admit_control_opcode(HELIOS_CONTROL_CLASS_PURE, Pure, RingZeroControl, false),
            Ok(Pure)
        );
        // A declared class that disagrees with the generated allowlist is the
        // escalation the static division exists to stop.
        assert_eq!(
            admit_control_opcode(
                HELIOS_CONTROL_CLASS_PURE,
                OuterAllocationBacked,
                RingZeroControl,
                false
            ),
            Err(ControlClassRefusal::ClassMismatch {
                declared: Pure,
                allowlisted: OuterAllocationBacked
            })
        );
        // Ring zero never receives or resolves an outer allocation.
        assert_eq!(
            admit_control_opcode(
                HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED,
                OuterAllocationBacked,
                RingZeroControl,
                true
            ),
            Err(ControlClassRefusal::OuterAllocationOnControlRing)
        );
        assert_eq!(
            admit_control_opcode(
                HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED,
                OuterAllocationBacked,
                OuterBatch,
                false
            ),
            Ok(OuterAllocationBacked)
        );
        // A GPU-dependent result may be fetched only after the HQC1 join.
        assert_eq!(
            admit_control_opcode(
                HELIOS_CONTROL_CLASS_GPU_DEPENDENT,
                GpuDependentSynchronous,
                RingZeroControl,
                false
            ),
            Err(ControlClassRefusal::GpuDependentWithoutOuterMilestone)
        );
        assert_eq!(
            admit_control_opcode(
                HELIOS_CONTROL_CLASS_GPU_DEPENDENT,
                GpuDependentSynchronous,
                RingZeroControl,
                true
            ),
            Ok(GpuDependentSynchronous)
        );
        assert_eq!(
            admit_control_opcode(
                HELIOS_CONTROL_CLASS_GPU_DEPENDENT,
                GpuDependentSynchronous,
                OuterBatch,
                true
            ),
            Err(ControlClassRefusal::GpuDependentOnOuterBatch)
        );
        assert_eq!(
            admit_control_opcode(0, Pure, RingZeroControl, true),
            Err(ControlClassRefusal::UnknownClass { found: 0 })
        );
    }

    /// Wire records must be byte-identical across producers: pin the exact
    /// little-endian bytes of the two scalars a C mirror is most likely to get
    /// wrong (the packed 4/2/2 header prefix and the 128-bit capability halves).
    #[test]
    fn hqa1_header_and_capability_are_little_endian_at_the_documented_offsets() {
        let a = attach();
        let bytes = bytemuck::bytes_of(&a);
        assert_eq!(&bytes[0..4], b"HQA1");
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 1);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 72);
        let low = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let high = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        assert_eq!(
            HeliosSessionCapability::from_halves(low, high),
            capability()
        );
        assert_eq!(u32::from_le_bytes(bytes[64..68].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[68..72].try_into().unwrap()), 0);
    }

    /// §10.2/§10.4 make exhaustion of each bounded capacity a refusal, so each
    /// one is a function with a named reason rather than a constant a caller is
    /// trusted to compare against.
    #[test]
    fn every_bounded_capacity_refuses_at_its_cap_and_names_it() {
        assert_eq!(admit_new_session(HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS - 1), Ok(()));
        assert_eq!(
            admit_new_session(HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS),
            Err(HeliosCapacityRefusal {
                limit: HeliosCapacityLimit::SessionsPerProcess,
                requested: HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS as u64 + 1,
                capacity: HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS as u64,
            })
        );

        assert_eq!(admit_ring_index(HELIOS_HTS1_MAX_RING_INDEX), Ok(()));
        assert_eq!(
            admit_ring_index(HELIOS_HTS1_MAX_RING_INDEX + 1),
            Err(HeliosCapacityRefusal {
                limit: HeliosCapacityLimit::RingIndex,
                requested: HELIOS_HTS1_MAX_RING_INDEX as u64 + 1,
                capacity: HELIOS_HTS1_MAX_RING_INDEX as u64,
            })
        );

        assert_eq!(admit_context_batch(0, 4096), Ok(()));
        assert_eq!(
            admit_context_batch(HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES, 4096),
            Err(HeliosCapacityRefusal {
                limit: HeliosCapacityLimit::OutstandingContextBatches,
                requested: HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES as u64 + 1,
                capacity: HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES as u64,
            })
        );
        // A zero-byte batch is not a batch, and one byte over the cap is over.
        assert!(admit_context_batch(0, 0).is_err());
        assert_eq!(
            admit_context_batch(0, HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES + 1),
            Err(HeliosCapacityRefusal {
                limit: HeliosCapacityLimit::ContextBatchBytes,
                requested: HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES + 1,
                capacity: HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES,
            })
        );

        assert_eq!(
            admit_host_dispatch_enqueue(HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH - 1),
            Ok(())
        );
        assert_eq!(
            admit_host_dispatch_enqueue(HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH),
            Err(HeliosCapacityRefusal {
                limit: HeliosCapacityLimit::HostDispatchFifoDepth,
                requested: HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH as u64 + 1,
                capacity: HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH as u64,
            })
        );
    }

    /// `protocol/include/helios_translation_session.h` hand-copies every
    /// constant below. Pin the exact literals here so a change on the Rust side
    /// without the matching header edit is caught by a failing test that names
    /// the header, not by a live VM admitting an outer context into the wrong
    /// host namespace. (Offsets and sizes need no test: both sides assert them
    /// at compile time.)
    ///
    /// The two aliased constants are pinned to their *literal* values on
    /// purpose. The header aliases them too — `HELIOS_HQA1_FLAG_*` through
    /// `helios_wddm.h` and `HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES` /
    /// `HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES` through `helios_wddm.h` and
    /// `helios_native_render.h` — so asserting alias-equals-alias would prove
    /// nothing. The literal is what a C compiler ends up with.
    #[test]
    fn c_mirror_carries_these_exact_constants() {
        // protocol/include/helios_translation_session.h
        assert_eq!(crate::HELIOS_PACKAGE_GENERATION, 0x4845_4C49_0000_0002);

        assert_eq!(HELIOS_HQA1_MAGIC, 0x3141_5148);
        assert_eq!(HELIOS_HQA1_ABI_VERSION, 1);
        assert_eq!(HELIOS_HQA1_SIZE, 72);
        assert_eq!(HELIOS_HQA1_FLAG_D3D11_PHYSICAL, 1);
        assert_eq!(HELIOS_HQA1_FLAG_D3D12_VIRTUAL, 2);
        assert_eq!(HELIOS_HQA1_FLAGS_MASK, 3);

        assert_eq!(HELIOS_HTS1_INIT_MAGIC, 0x3153_5448);
        assert_eq!(HELIOS_HTS1_REPLY_MAGIC, 0x3152_5448);
        assert_eq!(HELIOS_HTS1_ABI_VERSION, 1);
        assert_eq!(HELIOS_HTS1_INIT_SIZE, 32);
        assert_eq!(HELIOS_HTS1_REPLY_SIZE, 56);
        assert_eq!(HELIOS_HTS1_ENDPOINT_SIZE, 16);

        assert_eq!(HELIOS_ENGINE_CLASS_GRAPHICS, 1);
        assert_eq!(HELIOS_ENGINE_CLASS_COMPUTE, 2);
        assert_eq!(HELIOS_ENGINE_CLASS_COPY, 3);

        assert_eq!(HELIOS_CONTROL_CLASS_PURE, 1);
        assert_eq!(HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED, 2);
        assert_eq!(HELIOS_CONTROL_CLASS_GPU_DEPENDENT, 3);

        assert_eq!(HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS, 16);
        assert_eq!(HELIOS_HTS1_MAX_RING_INDEX, 255);
        assert_eq!(HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION, 64);
        assert_eq!(HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES, 64);
        assert_eq!(HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES, 15_728_640);
        assert_eq!(HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH, 256);
    }
}
