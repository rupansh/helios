//! Helios native-Vulkan legacy-KMT wire records — the bounded normal-ICD-to-KMD
//! Render and allocation ABI (`HELIOS_PRESENT_SYNC_RETIREMENT.md` section 10.7).
//!
//! # The boundary these bytes cross
//!
//! The *normal* (non-translator) Mesa Venus ICD executes every one of its
//! Vulkan submissions through documented legacy patch-mode KMT Render on one
//! raw KMT device per `vn_instance`. Four record families cross that boundary,
//! and nothing else does — there is no Escape, no IOCTL, no shared ring, and no
//! head/tail polling anywhere in this lane:
//!
//!   ICD --`D3DKMTCreateContext(pPrivateDriverData)`--> `DxgkDdiCreateContext`
//!        [`HeliosVulkanContextV1`] (`HVC1`, section 10.7)
//!   ICD --`D3DKMTRender(pCommand)`----------------> `DxgkDdiRender`
//!        [`HeliosNativeRenderV2`] (`HNR2`) + [`HeliosNativeRenderUse`]
//!        + [`HeliosNativeRenderPatch`] + the finite Venus payload
//!   ICD --`D3DKMTCreateAllocation2(pPrivateDriverData)`--> `DxgkDdiCreateAllocation`
//!        [`HeliosVenusMemoryAllocationV1`] (`HVM1`)
//!   host --role-1 reply pool bytes--> ICD (`D3DKMTLock2` CPU view)
//!        [`HeliosVenusReplyV1`] (`HVR1`) + [`HeliosVenusReplyContinuationV1`]
//!
//! # ⛔ What may never appear in these records
//!
//! **No pointer, no KMT/NT handle, no PID, no host object ID, no name, and no
//! lookup key.** Every field below is either a magic/version/size, a generation,
//! a bounded count/offset/length, an enum from a closed vocabulary, or a
//! diagnostic checksum. That is a property of the layouts, not a convention: a
//! field that could carry one of those things is a defect in this file.
//!
//! **⛔⛔ In particular: no actual virtio-gpu `resource_id` (`resid`) may appear
//! anywhere in this ABI.** This is the single rule most likely to be broken by a
//! writer porting code from [`crate::escape`] or [`crate::wddm`], where raw
//! `resid`s *were* the command identity:
//!
//!   * Mesa stores an **opaque local HVM1 allocation capability/generation** —
//!     the KMD-written [`HeliosVenusMemoryAllocationV1::object_generation`] — and
//!     repeats it in [`HeliosNativeRenderUse::expected_allocation_generation`].
//!     That generation is a staleness check against the KMD's own allocation
//!     object; it is not a renderer identity and resolves to nothing on the host.
//!   * The **generated Venus encoders write ZERO into every host-resource-id
//!     operand.** A [`HeliosNativeRenderPatch`] names the byte position and
//!     encoded width of each such zeroed operand, and **the KMD substitutes the
//!     host resource id itself, guest-side, as it builds the `SUBMIT_3D`.**
//!     That is `docs/retirement/K4-CONTRACT.md` §5 — "the KMD patches the host
//!     resid in from `HeliosNativeRenderPatch`" — and it is mesa lane unit
//!     **A3** plus K6. Sites across `umd/`, `umd12/` and `kmd_render/` already
//!     state it in those words — re-derive with `grep -rn
//!     'patches the host resid' umd/src umd12/src kmd_render/src`, which does
//!     not go stale the way a count would. This banner was the last place that
//!     still said otherwise.
//!
//!     ⛔ **SUPERSEDED BY `docs/retirement/FINDINGS.md` F5.** This bullet used
//!     to end "…so the KMD can rewrite it to a DMA-local capability ordinal at
//!     COMMIT, and QEMU substitutes the renderer-private resource ID only in
//!     its own host-only copy of the stream, after resolving the capability
//!     through HPM1." HPM1 is **DECLINED** — on maintenance grounds, not
//!     deferred — `qemu-helios` is reset to its pre-retirement base, and the
//!     three HPM1 commits survive only on branch `helios/hpm1-parked`. No host
//!     component resolves a capability, so the substitution is guest-side or it
//!     does not happen at all.
//!
//!     ⚖ **The property this trades away, named.** F5's own C55 discussion
//!     requires it be recorded wherever it is relied on, so: *the host is no
//!     longer the authority on resource identity.* Under §C55 no host resource
//!     id could reach guest code at all, so a buggy or hostile guest component
//!     could not name one; with guest-side substitution that structural
//!     impossibility is downgraded to "host virglrenderer validates resource
//!     ids per context" — defence in depth rather than a property of the
//!     design. F5 accepts the trade because the guest is the user's own VM.
//!     C55's *other* benefit, late binding of placement, was already
//!     unrealized and stays so: it pays off only once allocations actually
//!     move, and today's stack pins.
//!     [`HeliosNativeRenderUse::expected_allocation_generation`] is what
//!     refuses a batch whose allocation went stale in the meantime.
//!   * A nonzero host-resource-id operand arriving from user mode is therefore
//!     not "already resolved" — it is a rejected batch.
//!
//! # Which legacy modules this supersedes
//!
//! This module supersedes [`crate::escape`] outright for the native ICD lane:
//! `HELIOS_ESCAPE_OP_SUBMIT_VENUS`, the blob create/map/release verbs, and
//! `WAIT_FENCE` all become HNR2 Render + HVM1 allocation + the context's own
//! monitored fence. It also supersedes the deleted `HeliosWddmCmdBuf` and
//! `HeliosWddmAllocPrivate` records of the pre-retirement [`crate::wddm`]
//! *for this lane only* — the D3D outer path keeps its own records (HOB1/HOS1,
//! section 10.4) in [`crate::wddm`].
//! Per section 17.1 `escape.rs` and `ioctl.rs` are deleted once `kmd_render`,
//! `umd`, and `umd12` have migrated; that deletion is a later phase and this
//! file adds no dependency on either.
//!
//! # How this differs from the D3D outer-batch path (section 10.4)
//!
//! | | native HNR2 (this file, §10.7) | D3D HOB1 (`crate::wddm`, §10.4) |
//! |---|---|---|
//! | producer | Mesa's own encoder on its raw KMT device | the record-only translator inside DXVK/vkd3d |
//! | carrier | `D3DKMTRender`, **fragmentable** into ≤ 64 pieces | one complete contiguous record per outer WDDM submission, never fragmented |
//! | identity | allocation-list index + allocation generation | type-1 allocation-list index **or** type-2 D3D12 GPUVA |
//! | use record | 24 bytes, `{index,access,generation,firstPatch,patchCount}` | 40 bytes, `{addressOrIndex,byteLength,generation,access,identityKind,…}` |
//! | operand record | 16 bytes keyed by **allocation-list index** | 16 bytes keyed by **use index** |
//! | reply path | HVR1 into the session's role-1 pool | none; results come back through the outer runtime |
//! | private data | reserved zero (`D3DDDICB_RENDER`) | 64-byte HOS1 for D3D12 virtual submit |
//!
//! The two ABIs are deliberately *not* unified: HNR2's fragmentation and reply
//! slots exist because a Venus stream is finite and self-describing, while HOB1
//! must be one sealed immutable batch that a runtime command buffer already
//! sized. Sharing one record would force each lane to carry the other's fields.
//!
//! # Layout rules
//!
//! Every wire struct is `#[repr(C)]`, little-endian, pointer-free, and
//! padding-free (explicit `reserved` fields, never implicit padding), so it
//! derives `Pod`/`Zeroable` and the C mirrors used by Mesa and QEMU are a
//! byte-for-byte transliteration. Every size, alignment, and field offset from
//! the section-10.7 tables is asserted at compile time: a wrong offset breaks
//! the build rather than a test.
//!
//! Validation helpers are total functions returning `Result<_, …Reject>` with a
//! named reason per rejection — never a `bool`, never a panic. Reserved fields
//! are validated as zero, and unknown/mismatched magic, version, size, or
//! generation is a hard reject.
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
//!
//! # ⚠ Producer status — source reachability is not runtime admission
//!
//! Re-measured 2026-08-13 across `kmd_render`, `kmd_logic`, Mesa, and the
//! runtime probes:
//!
//! | family | status |
//! |---|---|
//! | **HVM1** ([`HeliosVenusMemoryAllocationV1`], [`Hvm1Stage`], [`Hvm1Role`], [`Hvm1Placement`]) | **WIRED.** K4/K2a create and bind the exact WDDM allocation; role 1 has four 1-MiB slots in a 4-MiB shared backing. Roles 2/3 remain later Mesa storage work. |
//! | **HVC1**, **HNR2**, **HVR1** (`HELIOS_HVC1_*`, `HELIOS_HNR2_*`, `HELIOS_HVR1_*` and their structs) | **PARTLY WIRED.** Mesa A1/A2 and KMD K5/K6 provide the finite carrier. K11 executes only the exact pure-control HTS1 INIT allowlist and publishes a host-derived HVR1 reply. Allocation-backed and GPU/queue work still refuses until Mesa A3/A4 and their owning KMD units exist. |
//! | [`kernel_dma`] | **PARTLY WIRED, KERNEL-PRIVATE.** K6 writes the fixed DMA-private/capability records, Patch snapshots placements, and Submit retires them. K11 uses one private flag only for its already-terminal pure INIT. Allocation-backed validation and GPU execution remain later work. |
//!
//! Compiling or passing source gates is not evidence of a host-produced reply,
//! display admission, or ordinary Vulkan execution. Those claims require their
//! own correlated target evidence.

use bytemuck::{Pod, Zeroable};

// ─────────────────────────────────────────────────────────────────────────────
// Package-wide admission
// ─────────────────────────────────────────────────────────────────────────────

/// The exact Venus capset ID every HVC1 context must name (section 10.7:
/// "capset | exact Venus capset ID").
///
/// Aliased rather than re-declared so this lane can never drift from the
/// virtio-gpu wire constant the KMD actually sends in `CTX_CREATE`.
pub const HELIOS_NATIVE_RENDER_CAPSET: u32 = crate::virtio_gpu::VIRTIO_GPU_CAPSET_VENUS;

// The CRC64 the `fragment_crc64` / `full_payload_crc64` fields carry is
// CRC-64/ECMA-182, and this crate defines it exactly once, in [`crate::wddm`]:
// [`crate::wddm::HELIOS_CRC64_ECMA182_POLY`] (normal, non-reflected form, init 0,
// xorout 0, `check("123456789") == 0x6C40DF5F0B497347`) with the `const fn`
// [`crate::wddm::crc64_ecma`] implementing it. HOB1 (section 10.4) and HNR2
// (section 10.7) must agree byte-for-byte on the parameterisation, so a second
// declaration here would be a drift mirror — the checksum here is a **corruption
// diagnostic and never validation authority** (section 10.7), so no validator in
// this file consults it, but a producer computing it must call that one function.
//
// The WDDM memory-segment IDs are likewise defined exactly once, in
// [`crate::physical_memory`], which owns the whole segment/page-number
// interpretation (`HELIOS_SEGMENT_ID_SYSTEM` / `_APERTURE` / `_HLM1`). This lane
// only names segments; it does not define them.
use crate::physical_memory::{HELIOS_SEGMENT_ID_APERTURE, HELIOS_SEGMENT_ID_HLM1};

/// Why a package-generation check failed. Folded into every record's reject
/// enum; kept separate so the two distinct failures cannot be confused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationCheck {
    /// The caller passed zero as the admitted generation. Fail closed: a lane
    /// that has not established the package generation admits nothing.
    Unset,
    /// The record names a different package generation. Section 3: "No version
    /// or feature fallback"; generation mismatch is fatal, never downgraded.
    Mismatch,
}

/// Admit a record's package generation against the one this lane was given.
///
/// UNIFIED: the *rule* (zero is never a live generation on either side; equality
/// otherwise) is implemented exactly once, in
/// [`crate::translation_session::check_generation_match`]; this function only
/// projects that shared refusal onto the two-variant vocabulary the reject enums
/// in this file already carry. The production value of `expected` is
/// [`crate::HELIOS_PACKAGE_GENERATION`], supplied by the caller so the KMD can
/// pin one generation per adapter and tests can drive a mismatch.
#[inline]
fn admit_package_generation(actual: u64, expected: u64) -> Result<(), GenerationCheck> {
    match crate::translation_session::check_generation_match(actual, expected) {
        Ok(()) => Ok(()),
        // The shared rule reports "the receiver has no live generation" as a
        // mismatch against zero; this lane names it separately because a zero
        // `expected` means the *caller* never established the package, not that
        // the record is wrong.
        Err(_) if expected == 0 => Err(GenerationCheck::Unset),
        Err(_) => Err(GenerationCheck::Mismatch),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HVC1 — the legacy KMT control/queue context ABI
// ─────────────────────────────────────────────────────────────────────────────

/// `'HVC1'` little-endian (`0x31435648`).
pub const HELIOS_HVC1_MAGIC: u32 = 0x3143_5648;
/// HVC1 ABI version (section 10.7 table, offset 4).
pub const HELIOS_HVC1_ABI_VERSION: u16 = 1;
/// HVC1 structure size (section 10.7 table, offset 6).
pub const HELIOS_HVC1_SIZE: u16 = 32;

/// [`HeliosVulkanContextV1::mode`] — the only admitted mode: finite HNR2 over
/// legacy KMT Render. There is no other mode in this package generation, so any
/// other value is a hard reject rather than a fallback.
pub const HELIOS_HVC1_MODE_FINITE_HNR2_RENDER: u32 = 2;

/// Both [`HeliosVulkanContextV1::queue_family`] and
/// [`HeliosVulkanContextV1::queue_index`] equal to this value mean the control
/// context. The ordinals are **copied diagnostics and never identity** — the KMD
/// binds the context object itself, not these numbers.
pub const HELIOS_HVC1_CONTROL_ORDINAL: u32 = u32::MAX;

/// `D3DKMTCreateContext::NodeOrdinal` for every HVC1 context: this generation
/// exposes exactly one WDDM render node.
pub const HELIOS_HVC1_NODE_ORDINAL: u32 = 0;
/// `D3DKMTCreateContext::EngineAffinity` for every HVC1 context — zero-based,
/// per section 10.7.
pub const HELIOS_HVC1_ENGINE_AFFINITY: u32 = 0;
/// `EngineAffinity` for the context's own monitored progress fence
/// (`D3DKMTCreateSynchronizationObject2`, section 10.7; the same value section
/// 12.2 requires of `D3DKMTOpenNativeFenceFromNtHandle`). This is a **bit mask**
/// over the one exposed node, not the zero-based ordinal above; the two spellings
/// are adjacent here precisely because confusing them is silent.
pub const HELIOS_HVC1_FENCE_ENGINE_AFFINITY: u32 = 1 << 0;

/// `DXGK_CONTEXTINFO::DmaBufferSize` advertised for an HVC1 context.
pub const HELIOS_HVC1_DMA_BUFFER_BYTES: u32 = 256 * 1024;
/// `DXGK_CONTEXTINFO::AllocationListSize` advertised for an HVC1 context.
pub const HELIOS_HVC1_ALLOCATION_LIST_ENTRIES: u32 = 4096;
/// `DXGK_CONTEXTINFO::PatchLocationListSize` advertised for an HVC1 context.
///
/// Distinct from [`HELIOS_HNR2_MAX_PATCH_RECORDS`]: this is the OS patch-location
/// capacity the KMD *writes* (one entry per use record, so at most
/// [`HELIOS_HNR2_MAX_USE_RECORDS`]), while HNR2's 8192 typed patch records live
/// inside the copied command and are never WDDM patch locations.
pub const HELIOS_HVC1_PATCH_LOCATION_ENTRIES: u32 = 4096;
/// `DXGK_CONTEXTINFO::DmaBufferPrivateDataSize` — 64 bytes of KMD-only DMA
/// private data, laid out by [`kernel_dma::Hnr2KmdDmaPrivateV1`].
pub const HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES: u32 = 64;
/// `DXGK_CONTEXTINFO::DmaBufferSegmentSet` — zero, the documented contiguous
/// paged-locked DMA-buffer case. HVC1 never selects the aperture segment here.
pub const HELIOS_HVC1_DMA_BUFFER_SEGMENT_SET: u32 = 0;

/// The control context's host ring index. Its terminal point is renderer command
/// processing and reply publication only; **ring 0 completion is never a
/// GPU-complete fact** (invariant 12).
pub const HELIOS_HVC1_CONTROL_RING_INDEX: u32 = 0;

/// Create-context private data for one native-Vulkan legacy KMT context
/// (`HVC1`). 32 bytes, pointer-free.
///
/// One raw KMT device per `vn_instance` creates exactly one **control** context
/// (host ring 0, HTS1 session owner, restricted to the pure-control opcode class)
/// and one **queue** context per real lower `VkQueue`, each bound to a unique
/// nonzero, non-recycled `INFO_RING_IDX`. Neither the host context nor the ring
/// index is ever returned to user mode, which is why no field here can name one.
///
/// Section 10.7 field table (offset/size/rule) is asserted below.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeliosVulkanContextV1 {
    /// == [`HELIOS_HVC1_MAGIC`].
    pub magic: u32,
    /// == [`HELIOS_HVC1_ABI_VERSION`].
    pub abi_version: u16,
    /// == [`HELIOS_HVC1_SIZE`].
    pub struct_size: u16,
    /// Exact atomic-package generation.
    pub package_generation: u64,
    /// Exact Venus capset ID ([`HELIOS_NATIVE_RENDER_CAPSET`]).
    pub capset: u32,
    /// == [`HELIOS_HVC1_MODE_FINITE_HNR2_RENDER`].
    pub mode: u32,
    /// Copied diagnostic ordinal; never identity. Both ordinals
    /// [`HELIOS_HVC1_CONTROL_ORDINAL`] mean the control context.
    pub queue_family: u32,
    /// Copied diagnostic ordinal; never identity.
    pub queue_index: u32,
}

/// What a validated [`HeliosVulkanContextV1`] asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hvc1ContextClass {
    /// The one control context: host ring 0, pure-control opcodes only, owns the
    /// HTS1 session and its role-1 reply pool.
    Control,
    /// A real lower-VkQueue context: unique nonzero `INFO_RING_IDX`, carries the
    /// actual GPU work, and its `DxgkDdiSubmitCommand` completes only after that
    /// VkQueue's terminal host fence.
    Queue,
}

/// Why [`HeliosVulkanContextV1::validate`] refused. Codes are stable and
/// append-only: they are counter/ETW identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hvc1Reject {
    /// `magic` is not [`HELIOS_HVC1_MAGIC`].
    MagicMismatch,
    /// `abi_version` is not [`HELIOS_HVC1_ABI_VERSION`].
    AbiVersionMismatch,
    /// `struct_size` is not [`HELIOS_HVC1_SIZE`].
    StructSizeMismatch,
    /// The caller has no admitted package generation.
    PackageGenerationUnset,
    /// `package_generation` names a different package.
    PackageGenerationMismatch,
    /// `capset` is not the exact Venus capset.
    CapsetMismatch,
    /// `mode` is not [`HELIOS_HVC1_MODE_FINITE_HNR2_RENDER`].
    ModeUnsupported,
    /// Exactly one ordinal is [`HELIOS_HVC1_CONTROL_ORDINAL`]. The doc defines
    /// only "both mean control"; a half-control record is refused rather than
    /// guessed in either direction.
    QueueOrdinalMixed,
    /// The create-context private-data buffer is not exactly
    /// [`HELIOS_HVC1_SIZE`] bytes.
    PrivateDataSize,
    /// `DXGK_CREATECONTEXTFLAGS` carries a bit HVC1 does not admit. Section
    /// 10.7: "KMD accepts HVC1 only when
    /// `DXGK_CREATECONTEXTFLAGS::VirtualAddressing=0` and all other unsupported
    /// context flags are zero" — i.e. the whole word must be
    /// [`HELIOS_HVC1_CREATE_CONTEXT_FLAGS`].
    ContextFlagsUnsupported,
}

impl Hvc1Reject {
    /// Stable numeric reason code for a KMD counter / ETW field.
    pub const fn code(self) -> u32 {
        match self {
            Self::MagicMismatch => 0x0101,
            Self::AbiVersionMismatch => 0x0102,
            Self::StructSizeMismatch => 0x0103,
            Self::PackageGenerationUnset => 0x0104,
            Self::PackageGenerationMismatch => 0x0105,
            Self::CapsetMismatch => 0x0106,
            Self::ModeUnsupported => 0x0107,
            Self::QueueOrdinalMixed => 0x0108,
            Self::PrivateDataSize => 0x0109,
            Self::ContextFlagsUnsupported => 0x010A,
        }
    }
}

/// The only `DXGK_CREATECONTEXTFLAGS::Value` an HVC1 context may carry.
///
/// Section 10.7 states the rule as "`VirtualAddressing=0` and all other
/// unsupported context flags are zero", and this generation supports none of
/// them, so the admitted word is exactly zero. Declared as a constant rather
/// than left in prose so [`hvc1_admit_context_flags`] has something to compare
/// against and a future supported bit has one place to appear.
pub const HELIOS_HVC1_CREATE_CONTEXT_FLAGS: u32 = 0;

/// The `DXGK_CREATECONTEXTFLAGS` gate section 10.7 places on HVC1.
///
/// Separate from [`HeliosVulkanContextV1::validate`] because the flags are a
/// `DxgkDdiCreateContext` argument, not part of the private data — but it lives
/// here so the rule is code on the same page as the record it guards.
#[inline]
pub const fn hvc1_admit_context_flags(flags: u32) -> Result<(), Hvc1Reject> {
    if flags != HELIOS_HVC1_CREATE_CONTEXT_FLAGS {
        return Err(Hvc1Reject::ContextFlagsUnsupported);
    }
    Ok(())
}

/// Read HVC1 out of the create-context private driver data.
///
/// Total: the exact-length check comes first, so no arm can panic — a panic in
/// a DDI is a silent graphics deadlock (CLAUDE.md key invariants). Owned and
/// unaligned for the same reason as
/// [`crate::translation_session::parse_create_context_private_data`] and
/// [`crate::wddm::HeliosOuterSubmitV1::from_private_data`]: the runtime's
/// buffer carries no alignment promise.
pub fn parse_hvc1_private_data(private_data: &[u8]) -> Result<HeliosVulkanContextV1, Hvc1Reject> {
    if private_data.len() != core::mem::size_of::<HeliosVulkanContextV1>() {
        return Err(Hvc1Reject::PrivateDataSize);
    }
    // With the length already exact, `try_pod_read_unaligned` cannot fail; the
    // arm is kept because a total function may not `unwrap`.
    bytemuck::try_pod_read_unaligned::<HeliosVulkanContextV1>(private_data)
        .map_err(|_| Hvc1Reject::PrivateDataSize)
}

impl HeliosVulkanContextV1 {
    /// The one control context for a session.
    pub const fn new_control(package_generation: u64) -> Self {
        Self {
            magic: HELIOS_HVC1_MAGIC,
            abi_version: HELIOS_HVC1_ABI_VERSION,
            struct_size: HELIOS_HVC1_SIZE,
            package_generation,
            capset: HELIOS_NATIVE_RENDER_CAPSET,
            mode: HELIOS_HVC1_MODE_FINITE_HNR2_RENDER,
            queue_family: HELIOS_HVC1_CONTROL_ORDINAL,
            queue_index: HELIOS_HVC1_CONTROL_ORDINAL,
        }
    }

    /// A queue context. `queue_family`/`queue_index` are copied for diagnostics
    /// only; passing [`HELIOS_HVC1_CONTROL_ORDINAL`] for both would encode a
    /// control context, which no real Vulkan queue can name.
    pub const fn new_queue(package_generation: u64, queue_family: u32, queue_index: u32) -> Self {
        Self {
            magic: HELIOS_HVC1_MAGIC,
            abi_version: HELIOS_HVC1_ABI_VERSION,
            struct_size: HELIOS_HVC1_SIZE,
            package_generation,
            capset: HELIOS_NATIVE_RENDER_CAPSET,
            mode: HELIOS_HVC1_MODE_FINITE_HNR2_RENDER,
            queue_family,
            queue_index,
        }
    }

    /// Total validation of one create-context private-data blob.
    ///
    /// The caller additionally owes the context-flag gate, which is a
    /// `DxgkDdiCreateContext` argument rather than part of this record:
    /// [`hvc1_admit_context_flags`].
    pub fn validate(
        &self,
        expected_package_generation: u64,
        expected_capset: u32,
    ) -> Result<Hvc1ContextClass, Hvc1Reject> {
        if self.magic != HELIOS_HVC1_MAGIC {
            return Err(Hvc1Reject::MagicMismatch);
        }
        if self.abi_version != HELIOS_HVC1_ABI_VERSION {
            return Err(Hvc1Reject::AbiVersionMismatch);
        }
        if self.struct_size != HELIOS_HVC1_SIZE {
            return Err(Hvc1Reject::StructSizeMismatch);
        }
        match admit_package_generation(self.package_generation, expected_package_generation) {
            Ok(()) => {}
            Err(GenerationCheck::Unset) => return Err(Hvc1Reject::PackageGenerationUnset),
            Err(GenerationCheck::Mismatch) => return Err(Hvc1Reject::PackageGenerationMismatch),
        }
        if self.capset != expected_capset {
            return Err(Hvc1Reject::CapsetMismatch);
        }
        if self.mode != HELIOS_HVC1_MODE_FINITE_HNR2_RENDER {
            return Err(Hvc1Reject::ModeUnsupported);
        }
        let family_control = self.queue_family == HELIOS_HVC1_CONTROL_ORDINAL;
        let index_control = self.queue_index == HELIOS_HVC1_CONTROL_ORDINAL;
        match (family_control, index_control) {
            (true, true) => Ok(Hvc1ContextClass::Control),
            (false, false) => Ok(Hvc1ContextClass::Queue),
            _ => Err(Hvc1Reject::QueueOrdinalMixed),
        }
    }
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md section 10.7 — HVC1 field table.
const _: () = {
    assert!(core::mem::size_of::<HeliosVulkanContextV1>() == HELIOS_HVC1_SIZE as usize);
    assert!(core::mem::align_of::<HeliosVulkanContextV1>() == 8);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, capset) == 16);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, mode) == 20);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, queue_family) == 24);
    assert!(core::mem::offset_of!(HeliosVulkanContextV1, queue_index) == 28);
};

// ─────────────────────────────────────────────────────────────────────────────
// HNR2 — the finite Venus stream fragmentation ABI
// ─────────────────────────────────────────────────────────────────────────────

/// `'HNR2'` little-endian (`0x32524e48`).
pub const HELIOS_HNR2_MAGIC: u32 = 0x3252_4E48;
/// HNR2 ABI version (section 10.7 table, offset 4).
pub const HELIOS_HNR2_ABI_VERSION: u16 = 2;
/// HNR2 header size (section 10.7 table, offset 6).
pub const HELIOS_HNR2_HEADER_SIZE: u16 = 112;

/// Maximum reassembled Venus payload for one HNR2 batch: 15 MiB.
pub const HELIOS_HNR2_MAX_PAYLOAD_BYTES: u64 = 15 * 1024 * 1024;
/// Maximum consecutive Render fragments per batch.
pub const HELIOS_HNR2_MAX_FRAGMENTS: u16 = 64;
/// Maximum COMMIT use records; also the maximum `AllocationCount` and the
/// maximum number of output `D3DDDI_PATCHLOCATIONLIST` entries (one per use).
pub const HELIOS_HNR2_MAX_USE_RECORDS: u32 = 4096;
/// Maximum COMMIT typed-patch records — exact parsed resource operands.
pub const HELIOS_HNR2_MAX_PATCH_RECORDS: u32 = 8192;
/// Size of one COMMIT use record.
pub const HELIOS_HNR2_USE_RECORD_SIZE: u32 = 24;
/// Size of one COMMIT typed-patch record.
pub const HELIOS_HNR2_PATCH_RECORD_SIZE: u32 = 16;

/// KMD staging-pool cap: at most 64 outstanding submissions per context
/// (section 10.7, "one slot from a context-local generation-checked pool capped
/// at 64 outstanding submissions and 15 MiB total"). Exhaustion returns resource
/// failure and never waits, scans another context, or spills to a global queue.
pub const HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS: u32 = 64;
/// KMD staging-pool byte cap per context, from the same sentence. Stated
/// independently of [`HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS`] and deliberately
/// not reconciled with it here: the doc gives both numbers and no arithmetic
/// relating them, so both are enforced as written.
pub const HELIOS_HNR2_SLOT_POOL_BYTES: u64 = 15 * 1024 * 1024;

/// [`HeliosNativeRenderV2::flags`] — first fragment of a batch.
pub const HELIOS_HNR2_FLAG_BEGIN: u32 = 1;
/// [`HeliosNativeRenderV2::flags`] — last fragment of a batch. Only a COMMIT
/// carries the allocation list, the use/patch tables, and any reply descriptor.
pub const HELIOS_HNR2_FLAG_COMMIT: u32 = 2;
/// [`HeliosNativeRenderV2::flags`] — this batch requests one bounded HVR1 reply.
pub const HELIOS_HNR2_FLAG_HAS_REPLY: u32 = 4;
/// The complete legal flag set: "only `BEGIN=1`, `COMMIT=2`, `HAS_REPLY=4`".
pub const HELIOS_HNR2_FLAG_MASK: u32 =
    HELIOS_HNR2_FLAG_BEGIN | HELIOS_HNR2_FLAG_COMMIT | HELIOS_HNR2_FLAG_HAS_REPLY;

/// [`HeliosNativeRenderV2::reply_allocation_list_index`] when the batch requests
/// no reply.
pub const HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX: u32 = u32::MAX;
/// Required alignment of [`HeliosNativeRenderV2::reply_offset`] — HVR1's own
/// 8-byte alignment, which is what "exact aligned range" has to mean for a
/// header containing `u64` fields.
pub const HELIOS_HNR2_REPLY_OFFSET_ALIGN: u64 = 8;

/// [`HeliosNativeRenderUse::access_flags`] — the batch reads the allocation.
pub const HELIOS_HNR2_ACCESS_READ: u32 = 1;
/// [`HeliosNativeRenderUse::access_flags`] — the batch writes the allocation.
/// Must equal the `WriteOperation` bit of the matching `D3DDDI_ALLOCATIONLIST`
/// entry; see [`validate_use_write_operation`].
pub const HELIOS_HNR2_ACCESS_WRITE: u32 = 2;
/// "Only `READ=1` and `WRITE=2` exist; unknown bits are zero."
pub const HELIOS_HNR2_ACCESS_MASK: u32 = HELIOS_HNR2_ACCESS_READ | HELIOS_HNR2_ACCESS_WRITE;

/// [`HeliosNativeRenderPatch::operand_kind`] — never valid on the wire; present
/// so a zeroed record is a reject rather than an accidentally meaningful one.
pub const HELIOS_HNR2_OPERAND_KIND_INVALID: u16 = 0;
/// [`HeliosNativeRenderPatch::operand_kind`] — a 32-bit host-resource-id operand
/// in the generated Venus schema (the width Venus encodes a `resourceId` at).
/// The encoder writes **zero** at this position; the KMD rewrites it to a
/// DMA-local capability ordinal at COMMIT.
pub const HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32: u16 = 1;
/// [`HeliosNativeRenderPatch::operand_kind`] — a 64-bit host-resource-id operand.
/// Same zero-placeholder rule as the 32-bit kind.
pub const HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64: u16 = 2;
/// Encoded width of [`HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32`].
pub const HELIOS_HNR2_OPERAND_WIDTH_32: u16 = 4;
/// Encoded width of [`HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64`].
pub const HELIOS_HNR2_OPERAND_WIDTH_64: u16 = 8;
/// Required alignment of [`HeliosNativeRenderPatch::payload_offset`]. The Venus
/// command stream is encoded in 4-byte units, so every generated operand — of
/// either width — starts 4-byte aligned; "arbitrary byte patching is rejected".
pub const HELIOS_HNR2_OPERAND_ALIGN: u32 = 4;

/// Header at the start of every HNR2 Render fragment. 112 bytes, pointer-free.
///
/// One batch is at most [`HELIOS_HNR2_MAX_PAYLOAD_BYTES`] split into at most
/// [`HELIOS_HNR2_MAX_FRAGMENTS`] consecutive Render calls; the first carries
/// `BEGIN`, the last `COMMIT`, and a one-fragment batch carries both. There is
/// exactly one incomplete batch per context, so no fragment lookup and no
/// cross-context assembler exists — [`Hnr2Expect::open`] is that whole state.
///
/// Section 10.7 field table (offset/size/rule) is asserted below.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeliosNativeRenderV2 {
    /// == [`HELIOS_HNR2_MAGIC`].
    pub magic: u32,
    /// == [`HELIOS_HNR2_ABI_VERSION`].
    pub abi_version: u16,
    /// == [`HELIOS_HNR2_HEADER_SIZE`].
    pub header_size: u16,
    /// Exact HVC1/package generation.
    pub package_generation: u64,
    /// Nonzero, strictly increasing for this context.
    pub batch_token: u64,
    /// Exact reassembled size: nonzero and at most
    /// [`HELIOS_HNR2_MAX_PAYLOAD_BYTES`]. Constant across the batch.
    pub total_payload_bytes: u64,
    /// Offset of this fragment's bytes **within the reassembled payload** —
    /// exactly the prior offset plus length; the first fragment's is zero. This
    /// is not a command-buffer offset; see [`Hnr2CommandLayout`].
    pub fragment_payload_offset: u64,
    /// This fragment's payload length; must fit the returned command buffer
    /// after the metadata.
    pub fragment_payload_bytes: u32,
    /// Zero based, exactly the expected next index.
    pub fragment_index: u16,
    /// `1..=64`, constant for the batch.
    pub fragment_count: u16,
    /// Zero before COMMIT; aligned and non-overlapping on COMMIT.
    pub use_record_offset: u32,
    /// Zero before COMMIT; equals the COMMIT `AllocationCount`, at most
    /// [`HELIOS_HNR2_MAX_USE_RECORDS`].
    pub use_record_count: u32,
    /// Zero before COMMIT; aligned and non-overlapping on COMMIT.
    pub patch_record_offset: u32,
    /// Zero before COMMIT; exact parsed resource operands, at most
    /// [`HELIOS_HNR2_MAX_PATCH_RECORDS`].
    pub patch_record_count: u32,
    /// [`HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX`] without a reply; a valid
    /// **writable** allocation-list index with `HAS_REPLY`.
    pub reply_allocation_list_index: u32,
    /// Only [`HELIOS_HNR2_FLAG_BEGIN`], [`HELIOS_HNR2_FLAG_COMMIT`],
    /// [`HELIOS_HNR2_FLAG_HAS_REPLY`].
    pub flags: u32,
    /// Zero without a reply; an exact aligned range inside one reply slot on
    /// COMMIT.
    pub reply_offset: u64,
    /// Zero without a reply; with `HAS_REPLY`, a nonzero bounded capacity no
    /// larger than one 1-MiB slot and wholly inside that slot. It must contain
    /// the HVR1 header plus the exact operation's finite payload.
    pub reply_capacity_bytes: u64,
    /// CRC64-ECMA of this fragment. **Corruption diagnostic, never validation
    /// authority** — no validator here reads it.
    ///
    /// Domain, fixed by this ABI because section 10.7 does not state one and two
    /// halves would otherwise pick differently: **this fragment's payload
    /// slice**, exactly the bytes at [`Hnr2CommandLayout::payload_offset`] for
    /// [`Hnr2CommandLayout::payload_bytes`]. Not the command buffer, not the
    /// header. A one-fragment batch therefore has `fragment_crc64 ==
    /// full_payload_crc64`. The C encoder states the same domain at its write
    /// site and `tools/hnr2-encoder-gate.sh` checks it.
    pub fragment_crc64: u64,
    /// Zero before COMMIT; the exact reassembled-payload CRC64-ECMA on COMMIT.
    /// Diagnostic only, and legitimately zero-valued, so it is checked for
    /// "absent before COMMIT" and nothing else.
    pub full_payload_crc64: u64,
    /// Zero without a reply; the exact nonzero checked-out slot generation with
    /// `HAS_REPLY`. Repeated by [`HeliosVenusReplyV1::slot_generation`].
    pub reply_slot_generation: u64,
}

/// One COMMIT use record. 24 bytes.
///
/// Every directly or transitively reachable allocation in the legacy batch
/// appears **exactly once** here and exactly once in the returned
/// `D3DDDI_ALLOCATIONLIST` with the same `WriteOperation`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub struct HeliosNativeRenderUse {
    /// Index into the COMMIT `D3DDDI_ALLOCATIONLIST`. Not a handle, not an ID.
    pub allocation_list_index: u32,
    /// [`HELIOS_HNR2_ACCESS_READ`] / [`HELIOS_HNR2_ACCESS_WRITE`] only.
    pub access_flags: u32,
    /// The opaque local HVM1 allocation generation Mesa stored at create time
    /// ([`HeliosVenusMemoryAllocationV1::object_generation`]). A staleness check
    /// against the KMD's allocation object — **not** a renderer identity.
    pub expected_allocation_generation: u64,
    /// First [`HeliosNativeRenderPatch`] belonging to this use.
    pub first_patch: u32,
    /// Number of consecutive patch records belonging to this use.
    pub patch_count: u32,
}

/// One COMMIT typed-patch record. 16 bytes.
///
/// It names the byte position and encoded width of one generated
/// host-resource-id operand whose payload bytes the encoder wrote as **zero**.
/// Arbitrary byte patching is rejected: the offset/kind/width must identify such
/// an operand in the generated, fully parsed opcode schema.
///
/// **Who fills the hole: the KMD, guest-side.** `K4-CONTRACT.md` §5 cites this
/// record by name — "the KMD patches the host resid in from
/// `HeliosNativeRenderPatch`" — as the *replacement mechanism* for the retired
/// 48-byte `HeliosWddmOpenIdentity::resource_id` that HWA2 deliberately does not
/// carry. It is not a substitution of one field for another; it is a different
/// mechanism, and it is mesa lane unit **A3** plus K6. Not QEMU: HPM1 is
/// declined (`docs/retirement/FINDINGS.md` F5) and there is no host-side
/// resolver.
///
/// ⚠ **No producer or consumer exists at HEAD.** `grep -rn
/// HeliosNativeRenderPatch kmd_render/src umd/src umd12/src icd/mesa/src`
/// returns matches, but **not one of them is a use of this type** — every hit
/// is a doc comment or a refusal-message string literal naming this record as
/// the thing A3 will build, sited where the driver fails loudly meanwhile. This
/// record is declared, not wired.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub struct HeliosNativeRenderPatch {
    /// Offset of the operand within the **reassembled** Venus payload.
    pub payload_offset: u32,
    /// The allocation this operand resolves to; must equal the owning use's
    /// [`HeliosNativeRenderUse::allocation_list_index`].
    pub allocation_list_index: u32,
    /// `HELIOS_HNR2_OPERAND_KIND_*`.
    pub operand_kind: u16,
    /// Encoded width in bytes; must match `operand_kind`.
    pub encoded_width: u16,
    /// Reserved; validated as zero.
    pub reserved: u32,
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md section 10.7 — HNR2 header + record tables.
const _: () = {
    assert!(core::mem::size_of::<HeliosNativeRenderV2>() == HELIOS_HNR2_HEADER_SIZE as usize);
    assert!(core::mem::align_of::<HeliosNativeRenderV2>() == 8);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, magic) == 0);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, header_size) == 6);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, batch_token) == 16);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, total_payload_bytes) == 24);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, fragment_payload_offset) == 32);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, fragment_payload_bytes) == 40);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, fragment_index) == 44);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, fragment_count) == 46);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, use_record_offset) == 48);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, use_record_count) == 52);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, patch_record_offset) == 56);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, patch_record_count) == 60);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, reply_allocation_list_index) == 64);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, flags) == 68);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, reply_offset) == 72);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, reply_capacity_bytes) == 80);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, fragment_crc64) == 88);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, full_payload_crc64) == 96);
    assert!(core::mem::offset_of!(HeliosNativeRenderV2, reply_slot_generation) == 104);

    assert!(core::mem::size_of::<HeliosNativeRenderUse>() == HELIOS_HNR2_USE_RECORD_SIZE as usize);
    assert!(core::mem::align_of::<HeliosNativeRenderUse>() == 8);
    assert!(core::mem::offset_of!(HeliosNativeRenderUse, allocation_list_index) == 0);
    assert!(core::mem::offset_of!(HeliosNativeRenderUse, access_flags) == 4);
    assert!(core::mem::offset_of!(HeliosNativeRenderUse, expected_allocation_generation) == 8);
    assert!(core::mem::offset_of!(HeliosNativeRenderUse, first_patch) == 16);
    assert!(core::mem::offset_of!(HeliosNativeRenderUse, patch_count) == 20);

    assert!(
        core::mem::size_of::<HeliosNativeRenderPatch>() == HELIOS_HNR2_PATCH_RECORD_SIZE as usize
    );
    assert!(core::mem::align_of::<HeliosNativeRenderPatch>() == 4);
    assert!(core::mem::offset_of!(HeliosNativeRenderPatch, payload_offset) == 0);
    assert!(core::mem::offset_of!(HeliosNativeRenderPatch, allocation_list_index) == 4);
    assert!(core::mem::offset_of!(HeliosNativeRenderPatch, operand_kind) == 8);
    assert!(core::mem::offset_of!(HeliosNativeRenderPatch, encoded_width) == 10);
    assert!(core::mem::offset_of!(HeliosNativeRenderPatch, reserved) == 12);

    // Section 10.7's own worst-case arithmetic, pinned so a bound cannot be
    // changed here without the aggregate being rechecked:
    //   15*1,048,576 + 64*112 + 4096*24 + 8192*16 = 15,965,184
    // below the 64*262,144 = 16,777,216-byte aggregate command-buffer capacity.
    assert!(
        HELIOS_HNR2_MAX_PAYLOAD_BYTES
            + HELIOS_HNR2_MAX_FRAGMENTS as u64 * HELIOS_HNR2_HEADER_SIZE as u64
            + HELIOS_HNR2_MAX_USE_RECORDS as u64 * HELIOS_HNR2_USE_RECORD_SIZE as u64
            + HELIOS_HNR2_MAX_PATCH_RECORDS as u64 * HELIOS_HNR2_PATCH_RECORD_SIZE as u64
            == 15_965_184
    );
    assert!(15_965_184u64 < HELIOS_HNR2_MAX_FRAGMENTS as u64 * HELIOS_HVC1_DMA_BUFFER_BYTES as u64);
    // One output patch location per use record, and the context advertises
    // exactly that many.
    assert!(HELIOS_HVC1_PATCH_LOCATION_ENTRIES == HELIOS_HNR2_MAX_USE_RECORDS);

    // `validate_commit_tables`'s uniqueness bitmap is `[u64; MAX/64]`. If the
    // maximum ever stopped being a multiple of 64 the array would silently be
    // one word short: legal high indices would then be reported as
    // out-of-range, which fails closed but misclassifies. Its doc-comment names
    // this assertion, so it must exist.
    assert!(HELIOS_HNR2_MAX_USE_RECORDS % 64 == 0);
    // 4096 bits is 512 bytes of PASSIVE_LEVEL stack; a KMD kernel stack is
    // 12 KiB, so pin the order of magnitude too.
    assert!((HELIOS_HNR2_MAX_USE_RECORDS / 64) as usize * 8 <= 1024);

    // Every magic is four ASCII bytes read little-endian. A transposed hex digit
    // in a literal would otherwise compile clean, pass every unit test that
    // builds records through the constructors, and fail only against a C mirror
    // or a live host.
    assert!(HELIOS_HVC1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HVC1_MAGIC.to_le_bytes()[1] == b'V');
    assert!(HELIOS_HVC1_MAGIC.to_le_bytes()[2] == b'C');
    assert!(HELIOS_HVC1_MAGIC.to_le_bytes()[3] == b'1');
    assert!(HELIOS_HNR2_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HNR2_MAGIC.to_le_bytes()[1] == b'N');
    assert!(HELIOS_HNR2_MAGIC.to_le_bytes()[2] == b'R');
    assert!(HELIOS_HNR2_MAGIC.to_le_bytes()[3] == b'2');
    assert!(HELIOS_HVM1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HVM1_MAGIC.to_le_bytes()[1] == b'V');
    assert!(HELIOS_HVM1_MAGIC.to_le_bytes()[2] == b'M');
    assert!(HELIOS_HVM1_MAGIC.to_le_bytes()[3] == b'1');
    assert!(HELIOS_HVR1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HVR1_MAGIC.to_le_bytes()[1] == b'V');
    assert!(HELIOS_HVR1_MAGIC.to_le_bytes()[2] == b'R');
    assert!(HELIOS_HVR1_MAGIC.to_le_bytes()[3] == b'1');
};

/// Where each region of one HNR2 Render command buffer starts.
///
/// ⚠ Section 10.7 gives explicit offsets for the use and patch tables but never
/// states where the fragment payload starts *within the command buffer* — only
/// that it "fits this returned command buffer after metadata" and that "the
/// encoder must reserve the COMMIT metadata before choosing every fragment
/// payload length". This ABI therefore fixes one canonical layout and enforces
/// it in both directions, so the question can never be answered differently by
/// the encoder and the validator:
///
/// ```text
/// +0                                    HeliosNativeRenderV2   (112 bytes)
/// +112                                  use records            (COMMIT only)
/// +112 + uses*24                        patch records          (COMMIT only)
/// +112 + uses*24 + patches*16           fragment payload
/// ```
///
/// Both table strides are multiples of 8, so the payload is always 8-aligned
/// with no padding and the regions cannot overlap by construction. An empty
/// table has offset zero (the header rule for "before COMMIT"), never a
/// degenerate pointer into the middle of the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hnr2CommandLayout {
    /// Byte offset of the use table, or zero when there are no use records.
    pub use_record_offset: u32,
    /// Byte offset of the patch table, or zero when there are no patch records.
    pub patch_record_offset: u32,
    /// Byte offset of this fragment's Venus payload.
    pub payload_offset: u32,
    /// This fragment's Venus payload length.
    pub payload_bytes: u32,
    /// Exact total command length this fragment requires.
    pub command_bytes: u32,
}

/// The one incomplete batch a context may have open. There is exactly one per
/// context; this struct *is* the assembler state, which is why no lookup,
/// table, or cross-context assembler exists anywhere in this lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hnr2OpenBatch {
    /// Token every remaining fragment must repeat.
    pub batch_token: u64,
    /// Reassembled size every remaining fragment must repeat.
    pub total_payload_bytes: u64,
    /// Fragment count every remaining fragment must repeat.
    pub fragment_count: u16,
    /// Exact index the next fragment must carry.
    pub next_fragment_index: u16,
    /// Exact payload offset the next fragment must carry.
    pub next_payload_offset: u64,
}

/// Everything outside the header that [`HeliosNativeRenderV2::validate`] needs.
/// All of it is per-context state the KMD already holds; none of it is a lookup.
#[derive(Debug, Clone, Copy)]
pub struct Hnr2Expect {
    /// The admitted atomic-package generation. Zero is refused, never treated as
    /// "any".
    pub package_generation: u64,
    /// Exact `DXGKARG_RENDER::AllocationListSize` for this Render — zero on a
    /// non-COMMIT fragment, and the exact COMMIT manifest length otherwise.
    pub allocation_list_count: u32,
    /// Exact `CommandLength` C53 probed and copied.
    pub command_length: u32,
    /// Exact `DXGKARG_RENDER::DmaSize` — the command buffer dxgkrnl actually
    /// returned for this Render.
    ///
    /// Section 10.7 line 1821: "`DxgkDdiRender` **first validates
    /// `CommandLength` against the advertised maximum**". Passed in rather than
    /// compared against [`HELIOS_HVC1_DMA_BUFFER_BYTES`] directly because that
    /// constant is the advertised *minimum* — "context creation fails if
    /// Dxgkrnl does not return … buffers of at least those advertised minima",
    /// and the ICD "adopts the returned next command/allocation buffers and
    /// their actual sizes". Both bounds are checked: this must be at least the
    /// advertised minimum, and `command_length` at most this.
    pub command_buffer_bytes: u32,
    /// Exact `DXGKARG_RENDER::PatchLocationListInSize`.
    ///
    /// Section 10.7 line 1826: "HNR2 requires `PatchLocationListInSize=0`; its
    /// own typed operand records are inside the copied command and are not WDDM
    /// patch locations." This is the only stated "requires" of the Render entry
    /// gate, so it gets a checked field rather than a comment.
    pub patch_location_list_in_size: u32,
    /// The one incomplete batch on this context, or `None`.
    pub open: Option<Hnr2OpenBatch>,
    /// Highest batch token this context has already accepted; zero when the
    /// context has accepted none.
    pub last_batch_token: u64,
}

/// Which of the four fragment shapes a validated header is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hnr2FragmentClass {
    /// `BEGIN` only — opens a multi-fragment batch.
    Begin,
    /// Neither flag — an interior fragment of an open batch.
    Interior,
    /// `COMMIT` only — closes a multi-fragment batch.
    Commit,
    /// `BEGIN|COMMIT` — a complete one-fragment batch.
    Complete,
}

impl Hnr2FragmentClass {
    /// Does this fragment carry the allocation list, tables, and any reply?
    pub const fn is_commit(self) -> bool {
        matches!(self, Self::Commit | Self::Complete)
    }
    /// Does this fragment open a batch?
    pub const fn is_begin(self) -> bool {
        matches!(self, Self::Begin | Self::Complete)
    }
}

/// What [`HeliosNativeRenderV2::validate`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hnr2Accept {
    /// The fragment's shape.
    pub class: Hnr2FragmentClass,
    /// `HAS_REPLY` was set (COMMIT only).
    pub has_reply: bool,
    /// The exact command-buffer regions.
    pub layout: Hnr2CommandLayout,
    /// Assembler state to store after appending this fragment's bytes; `None` on
    /// a COMMIT, because the batch closes.
    pub next_open: Option<Hnr2OpenBatch>,
}

/// Why [`HeliosNativeRenderV2::validate`] refused. Codes are stable and
/// append-only: they are counter/ETW identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hnr2Reject {
    /// `magic` is not [`HELIOS_HNR2_MAGIC`].
    MagicMismatch,
    /// `abi_version` is not [`HELIOS_HNR2_ABI_VERSION`].
    AbiVersionMismatch,
    /// `header_size` is not [`HELIOS_HNR2_HEADER_SIZE`].
    HeaderSizeMismatch,
    /// The caller has no admitted package generation.
    PackageGenerationUnset,
    /// `package_generation` names a different package.
    PackageGenerationMismatch,
    /// `flags` carries a bit outside [`HELIOS_HNR2_FLAG_MASK`].
    FlagBitsUnknown,
    /// `batch_token` is zero.
    BatchTokenZero,
    /// A new batch's token did not strictly increase on this context.
    BatchTokenNotIncreasing,
    /// A continuation fragment named a different batch.
    BatchTokenMismatch,
    /// `BEGIN` arrived while a batch was already open. The incomplete assembler
    /// is discarded and the context lost; it is never silently restarted.
    BatchAlreadyOpen,
    /// A non-`BEGIN` fragment arrived with no batch open.
    NoOpenBatch,
    /// `fragment_count` is zero.
    FragmentCountZero,
    /// `fragment_count` exceeds [`HELIOS_HNR2_MAX_FRAGMENTS`].
    FragmentCountTooLarge,
    /// `fragment_count` changed mid-batch.
    FragmentCountChanged,
    /// `fragment_index >= fragment_count`.
    FragmentIndexOutOfRange,
    /// `fragment_index` is not the expected next index.
    FragmentIndexOutOfOrder,
    /// `BEGIN` is set on a non-first fragment, or missing on the first.
    BeginFlagMisplaced,
    /// `COMMIT` is set on a non-final fragment, or missing on the final one.
    CommitFlagMisplaced,
    /// `total_payload_bytes` is zero.
    TotalPayloadZero,
    /// `total_payload_bytes` exceeds [`HELIOS_HNR2_MAX_PAYLOAD_BYTES`].
    TotalPayloadTooLarge,
    /// `total_payload_bytes` changed mid-batch.
    TotalPayloadChanged,
    /// `fragment_payload_bytes` is zero. A fragment that carries nothing cannot
    /// be part of an exact reassembly.
    FragmentPayloadZero,
    /// `fragment_payload_offset` is not the expected next offset (zero on the
    /// first fragment, prior offset plus length afterwards).
    FragmentOffsetMismatch,
    /// This fragment runs past `total_payload_bytes`, or the sum overflowed.
    FragmentPayloadOverrun,
    /// A non-final fragment already consumed the whole payload, which would make
    /// every remaining fragment empty.
    NonFinalFragmentExhaustsPayload,
    /// The final fragment does not end exactly at `total_payload_bytes`.
    FinalFragmentShort,
    /// Use-record offset or count is nonzero before COMMIT.
    UseRecordsBeforeCommit,
    /// Patch-record offset or count is nonzero before COMMIT.
    PatchRecordsBeforeCommit,
    /// `full_payload_crc64` is nonzero before COMMIT.
    FullPayloadCrcBeforeCommit,
    /// A non-COMMIT Render arrived with a nonempty allocation list.
    AllocationListNotEmpty,
    /// `use_record_count` exceeds [`HELIOS_HNR2_MAX_USE_RECORDS`].
    UseRecordCountTooLarge,
    /// `patch_record_count` exceeds [`HELIOS_HNR2_MAX_PATCH_RECORDS`].
    PatchRecordCountTooLarge,
    /// `use_record_count` is not exactly the COMMIT `AllocationCount`.
    UseRecordCountMismatch,
    /// Patch records exist with no use records to own them.
    PatchWithoutUse,
    /// `use_record_offset` is not the canonical [`Hnr2CommandLayout`] offset.
    UseRecordOffsetMismatch,
    /// `patch_record_offset` is not the canonical [`Hnr2CommandLayout`] offset.
    PatchRecordOffsetMismatch,
    /// The canonical layout does not fit a `u32` command length.
    CommandLengthOverflow,
    /// `CommandLength` is not exactly the bytes this header describes.
    CommandLengthMismatch,
    /// `CommandLength` exceeds the command buffer Dxgkrnl returned for this
    /// Render — section 10.7's "validates `CommandLength` against the advertised
    /// maximum".
    CommandLengthAboveDmaBuffer,
    /// Dxgkrnl returned a command buffer smaller than
    /// [`HELIOS_HVC1_DMA_BUFFER_BYTES`], the advertised minimum context creation
    /// is required to have enforced.
    CommandBufferBelowAdvertisedMinimum,
    /// `PatchLocationListInSize` is nonzero. HNR2 requires zero: its typed
    /// operands are inside the copied command, never WDDM patch locations.
    PatchLocationListInNotEmpty,
    /// `HAS_REPLY` on a fragment that is not the COMMIT.
    ReplyFlagOutsideCommit,
    /// `reply_allocation_list_index` is not
    /// [`HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX`] on a batch with no reply.
    ReplyIndexNotSentinel,
    /// `reply_offset` is nonzero on a batch with no reply.
    ReplyOffsetNotZero,
    /// `reply_capacity_bytes` is nonzero on a batch with no reply.
    ReplyCapacityNotZero,
    /// `reply_slot_generation` is nonzero on a batch with no reply.
    ReplySlotGenerationNotZero,
    /// `reply_allocation_list_index` is outside the COMMIT allocation list.
    ReplyIndexOutOfRange,
    /// `reply_slot_generation` is zero with `HAS_REPLY`.
    ReplySlotGenerationZero,
    /// `reply_capacity_bytes` is below one HVR1 header.
    ReplyCapacityTooSmall,
    /// `reply_capacity_bytes` exceeds one 1-MiB HVR1 slot.
    ReplyCapacityTooLarge,
    /// `reply_offset` is not [`HELIOS_HNR2_REPLY_OFFSET_ALIGN`]-aligned.
    ReplyOffsetMisaligned,
    /// `reply_offset` does not fall in one of the four 1-MiB slots.
    ReplySlotIndexOutOfRange,
    /// The reply range is not wholly inside the one slot it starts in.
    ReplyRangeCrossesSlot,
}

impl Hnr2Reject {
    /// Stable numeric reason code for a KMD counter / ETW field.
    pub const fn code(self) -> u32 {
        match self {
            Self::MagicMismatch => 0x0201,
            Self::AbiVersionMismatch => 0x0202,
            Self::HeaderSizeMismatch => 0x0203,
            Self::PackageGenerationUnset => 0x0204,
            Self::PackageGenerationMismatch => 0x0205,
            Self::FlagBitsUnknown => 0x0206,
            Self::BatchTokenZero => 0x0207,
            Self::BatchTokenNotIncreasing => 0x0208,
            Self::BatchTokenMismatch => 0x0209,
            Self::BatchAlreadyOpen => 0x020A,
            Self::NoOpenBatch => 0x020B,
            Self::FragmentCountZero => 0x020C,
            Self::FragmentCountTooLarge => 0x020D,
            Self::FragmentCountChanged => 0x020E,
            Self::FragmentIndexOutOfRange => 0x020F,
            Self::FragmentIndexOutOfOrder => 0x0210,
            Self::BeginFlagMisplaced => 0x0211,
            Self::CommitFlagMisplaced => 0x0212,
            Self::TotalPayloadZero => 0x0213,
            Self::TotalPayloadTooLarge => 0x0214,
            Self::TotalPayloadChanged => 0x0215,
            Self::FragmentPayloadZero => 0x0216,
            Self::FragmentOffsetMismatch => 0x0217,
            Self::FragmentPayloadOverrun => 0x0218,
            Self::NonFinalFragmentExhaustsPayload => 0x0219,
            Self::FinalFragmentShort => 0x021A,
            Self::UseRecordsBeforeCommit => 0x021B,
            Self::PatchRecordsBeforeCommit => 0x021C,
            Self::FullPayloadCrcBeforeCommit => 0x021D,
            Self::AllocationListNotEmpty => 0x021E,
            Self::UseRecordCountTooLarge => 0x021F,
            Self::PatchRecordCountTooLarge => 0x0220,
            Self::UseRecordCountMismatch => 0x0221,
            Self::PatchWithoutUse => 0x0222,
            Self::UseRecordOffsetMismatch => 0x0223,
            Self::PatchRecordOffsetMismatch => 0x0224,
            Self::CommandLengthOverflow => 0x0225,
            Self::CommandLengthMismatch => 0x0226,
            Self::ReplyFlagOutsideCommit => 0x0227,
            Self::ReplyIndexNotSentinel => 0x0228,
            Self::ReplyOffsetNotZero => 0x0229,
            Self::ReplyCapacityNotZero => 0x022A,
            Self::ReplySlotGenerationNotZero => 0x022B,
            Self::ReplyIndexOutOfRange => 0x022C,
            Self::ReplySlotGenerationZero => 0x022D,
            Self::ReplyCapacityTooSmall => 0x022E,
            Self::ReplyCapacityTooLarge => 0x022F,
            Self::ReplyOffsetMisaligned => 0x0230,
            Self::ReplySlotIndexOutOfRange => 0x0231,
            Self::ReplyRangeCrossesSlot => 0x0232,
            Self::CommandLengthAboveDmaBuffer => 0x0233,
            Self::CommandBufferBelowAdvertisedMinimum => 0x0234,
            Self::PatchLocationListInNotEmpty => 0x0235,
        }
    }
}

impl HeliosNativeRenderV2 {
    /// The canonical command-buffer regions this header describes, checked
    /// against the offsets it declares.
    ///
    /// Pure arithmetic on already-copied header bytes: no dereference, no
    /// user-buffer access, and every add/multiply is checked, so it cannot
    /// panic in a `panic=abort` kernel image.
    pub fn command_layout(&self) -> Result<Hnr2CommandLayout, Hnr2Reject> {
        if self.use_record_count > HELIOS_HNR2_MAX_USE_RECORDS {
            return Err(Hnr2Reject::UseRecordCountTooLarge);
        }
        if self.patch_record_count > HELIOS_HNR2_MAX_PATCH_RECORDS {
            return Err(Hnr2Reject::PatchRecordCountTooLarge);
        }
        if self.use_record_count == 0 && self.patch_record_count != 0 {
            return Err(Hnr2Reject::PatchWithoutUse);
        }

        let header = HELIOS_HNR2_HEADER_SIZE as u64;
        let use_bytes = self.use_record_count as u64 * HELIOS_HNR2_USE_RECORD_SIZE as u64;
        let patch_bytes = self.patch_record_count as u64 * HELIOS_HNR2_PATCH_RECORD_SIZE as u64;

        // An empty table has offset zero — the same spelling the header rule
        // uses before COMMIT — so an empty region can never overlap anything.
        let want_use_offset = if self.use_record_count == 0 {
            0
        } else {
            header
        };
        let want_patch_offset = if self.patch_record_count == 0 {
            0
        } else {
            header + use_bytes
        };
        if self.use_record_offset as u64 != want_use_offset {
            return Err(Hnr2Reject::UseRecordOffsetMismatch);
        }
        if self.patch_record_offset as u64 != want_patch_offset {
            return Err(Hnr2Reject::PatchRecordOffsetMismatch);
        }

        let payload_offset = header + use_bytes + patch_bytes;
        let command_bytes = payload_offset + self.fragment_payload_bytes as u64;
        if command_bytes > u32::MAX as u64 {
            return Err(Hnr2Reject::CommandLengthOverflow);
        }
        Ok(Hnr2CommandLayout {
            use_record_offset: want_use_offset as u32,
            patch_record_offset: want_patch_offset as u32,
            payload_offset: payload_offset as u32,
            payload_bytes: self.fragment_payload_bytes,
            command_bytes: command_bytes as u32,
        })
    }

    /// `maxChunkBytes` this batch granted the host, i.e.
    /// `reply_capacity_bytes - HVR1 header`. `None` when the batch requested no
    /// reply or the capacity cannot hold a header.
    pub fn granted_reply_chunk_bytes(&self) -> Option<u64> {
        if self.flags & HELIOS_HNR2_FLAG_HAS_REPLY == 0 {
            return None;
        }
        self.reply_capacity_bytes
            .checked_sub(HELIOS_HVR1_HEADER_SIZE as u64)
    }

    /// Total validation of one fragment header against this context's state.
    ///
    /// What a header alone cannot prove, and where each part is proved instead:
    ///   * the use/patch tables — [`validate_commit_tables`], which the caller
    ///     must run after this returns on a COMMIT;
    ///   * `reply_allocation_list_index` naming a **writable** entry — also
    ///     [`validate_commit_tables`], because writability lives in the use
    ///     table; and
    ///   * that entry belonging to this session's role-1 reply pool, and the
    ///     reply range lying inside that allocation — the KMD's own allocation
    ///     objects, which this crate cannot see.
    pub fn validate(&self, expect: &Hnr2Expect) -> Result<Hnr2Accept, Hnr2Reject> {
        use Hnr2Reject as R;

        if self.magic != HELIOS_HNR2_MAGIC {
            return Err(R::MagicMismatch);
        }
        if self.abi_version != HELIOS_HNR2_ABI_VERSION {
            return Err(R::AbiVersionMismatch);
        }
        if self.header_size != HELIOS_HNR2_HEADER_SIZE {
            return Err(R::HeaderSizeMismatch);
        }
        match admit_package_generation(self.package_generation, expect.package_generation) {
            Ok(()) => {}
            Err(GenerationCheck::Unset) => return Err(R::PackageGenerationUnset),
            Err(GenerationCheck::Mismatch) => return Err(R::PackageGenerationMismatch),
        }
        if self.flags & !HELIOS_HNR2_FLAG_MASK != 0 {
            return Err(R::FlagBitsUnknown);
        }

        let begin = self.flags & HELIOS_HNR2_FLAG_BEGIN != 0;
        let commit = self.flags & HELIOS_HNR2_FLAG_COMMIT != 0;
        let has_reply = self.flags & HELIOS_HNR2_FLAG_HAS_REPLY != 0;

        // ── batch identity and fragment order ────────────────────────────────
        if self.batch_token == 0 {
            return Err(R::BatchTokenZero);
        }
        if self.fragment_count == 0 {
            return Err(R::FragmentCountZero);
        }
        if self.fragment_count > HELIOS_HNR2_MAX_FRAGMENTS {
            return Err(R::FragmentCountTooLarge);
        }
        if self.fragment_index >= self.fragment_count {
            return Err(R::FragmentIndexOutOfRange);
        }
        if begin != (self.fragment_index == 0) {
            return Err(R::BeginFlagMisplaced);
        }
        if commit != (self.fragment_index == self.fragment_count - 1) {
            return Err(R::CommitFlagMisplaced);
        }
        if self.total_payload_bytes == 0 {
            return Err(R::TotalPayloadZero);
        }
        if self.total_payload_bytes > HELIOS_HNR2_MAX_PAYLOAD_BYTES {
            return Err(R::TotalPayloadTooLarge);
        }
        if self.fragment_payload_bytes == 0 {
            return Err(R::FragmentPayloadZero);
        }

        match (&expect.open, begin) {
            (Some(_), true) => return Err(R::BatchAlreadyOpen),
            (None, false) => return Err(R::NoOpenBatch),
            (None, true) => {
                if self.batch_token <= expect.last_batch_token {
                    return Err(R::BatchTokenNotIncreasing);
                }
                if self.fragment_payload_offset != 0 {
                    return Err(R::FragmentOffsetMismatch);
                }
            }
            (Some(open), false) => {
                if self.batch_token != open.batch_token {
                    return Err(R::BatchTokenMismatch);
                }
                if self.fragment_count != open.fragment_count {
                    return Err(R::FragmentCountChanged);
                }
                if self.total_payload_bytes != open.total_payload_bytes {
                    return Err(R::TotalPayloadChanged);
                }
                if self.fragment_index != open.next_fragment_index {
                    return Err(R::FragmentIndexOutOfOrder);
                }
                if self.fragment_payload_offset != open.next_payload_offset {
                    return Err(R::FragmentOffsetMismatch);
                }
            }
        }

        let payload_end = self
            .fragment_payload_offset
            .checked_add(self.fragment_payload_bytes as u64)
            .ok_or(R::FragmentPayloadOverrun)?;
        if payload_end > self.total_payload_bytes {
            return Err(R::FragmentPayloadOverrun);
        }
        if commit && payload_end != self.total_payload_bytes {
            return Err(R::FinalFragmentShort);
        }
        if !commit && payload_end == self.total_payload_bytes {
            return Err(R::NonFinalFragmentExhaustsPayload);
        }

        // ── COMMIT-only metadata ─────────────────────────────────────────────
        if commit {
            if self.use_record_count > HELIOS_HNR2_MAX_USE_RECORDS {
                return Err(R::UseRecordCountTooLarge);
            }
            if self.patch_record_count > HELIOS_HNR2_MAX_PATCH_RECORDS {
                return Err(R::PatchRecordCountTooLarge);
            }
            if self.use_record_count != expect.allocation_list_count {
                return Err(R::UseRecordCountMismatch);
            }
        } else {
            if self.use_record_count != 0 || self.use_record_offset != 0 {
                return Err(R::UseRecordsBeforeCommit);
            }
            if self.patch_record_count != 0 || self.patch_record_offset != 0 {
                return Err(R::PatchRecordsBeforeCommit);
            }
            if self.full_payload_crc64 != 0 {
                return Err(R::FullPayloadCrcBeforeCommit);
            }
            if expect.allocation_list_count != 0 {
                return Err(R::AllocationListNotEmpty);
            }
        }

        // "HNR2 requires PatchLocationListInSize=0" (section 10.7): the typed
        // operand records live inside the copied command and are not WDDM patch
        // locations, so an incoming patch-location list is a different protocol.
        if expect.patch_location_list_in_size != 0 {
            return Err(R::PatchLocationListInNotEmpty);
        }
        // The advertised command buffer is a floor for what Dxgkrnl returns, so
        // a returned buffer below it means context creation admitted something
        // it should have refused.
        if expect.command_buffer_bytes < HELIOS_HVC1_DMA_BUFFER_BYTES {
            return Err(R::CommandBufferBelowAdvertisedMinimum);
        }
        let layout = self.command_layout()?;
        if layout.command_bytes != expect.command_length {
            return Err(R::CommandLengthMismatch);
        }
        // Section 10.7: `DxgkDdiRender` first validates `CommandLength` against
        // the advertised maximum.
        if expect.command_length > expect.command_buffer_bytes {
            return Err(R::CommandLengthAboveDmaBuffer);
        }

        // ── reply descriptor ─────────────────────────────────────────────────
        if has_reply {
            if !commit {
                return Err(R::ReplyFlagOutsideCommit);
            }
            if self.reply_allocation_list_index >= expect.allocation_list_count {
                return Err(R::ReplyIndexOutOfRange);
            }
            if self.reply_slot_generation == 0 {
                return Err(R::ReplySlotGenerationZero);
            }
            if self.reply_capacity_bytes < HELIOS_HVR1_HEADER_SIZE as u64 {
                return Err(R::ReplyCapacityTooSmall);
            }
            if self.reply_capacity_bytes
                > HELIOS_HVR1_HEADER_SIZE as u64 + HELIOS_HVR1_MAX_CHUNK_BYTES
            {
                return Err(R::ReplyCapacityTooLarge);
            }
            // `%` rather than `is_multiple_of`: this crate is also compiled by
            // the Windows kernel toolchain, whose pinned nightly predates that
            // method's stabilisation, and every neighbouring module spells the
            // alignment test this way.
            if self.reply_offset % HELIOS_HNR2_REPLY_OFFSET_ALIGN != 0 {
                return Err(R::ReplyOffsetMisaligned);
            }
            let slot = self.reply_offset / HELIOS_HVM1_REPLY_SLOT_BYTES;
            if slot >= HELIOS_HVM1_REPLY_SLOT_COUNT as u64 {
                return Err(R::ReplySlotIndexOutOfRange);
            }
            let slot_end = (slot + 1) * HELIOS_HVM1_REPLY_SLOT_BYTES;
            let reply_end = self
                .reply_offset
                .checked_add(self.reply_capacity_bytes)
                .ok_or(R::ReplyRangeCrossesSlot)?;
            if reply_end > slot_end {
                return Err(R::ReplyRangeCrossesSlot);
            }
        } else {
            if self.reply_allocation_list_index != HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX {
                return Err(R::ReplyIndexNotSentinel);
            }
            if self.reply_offset != 0 {
                return Err(R::ReplyOffsetNotZero);
            }
            if self.reply_capacity_bytes != 0 {
                return Err(R::ReplyCapacityNotZero);
            }
            if self.reply_slot_generation != 0 {
                return Err(R::ReplySlotGenerationNotZero);
            }
        }

        let class = match (begin, commit) {
            (true, true) => Hnr2FragmentClass::Complete,
            (true, false) => Hnr2FragmentClass::Begin,
            (false, true) => Hnr2FragmentClass::Commit,
            (false, false) => Hnr2FragmentClass::Interior,
        };
        let next_open = if commit {
            None
        } else {
            Some(Hnr2OpenBatch {
                batch_token: self.batch_token,
                total_payload_bytes: self.total_payload_bytes,
                fragment_count: self.fragment_count,
                next_fragment_index: self.fragment_index + 1,
                next_payload_offset: payload_end,
            })
        };
        Ok(Hnr2Accept {
            class,
            has_reply,
            layout,
            next_open,
        })
    }
}

/// Why [`validate_commit_tables`] refused. Codes are stable and append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hnr2TableReject {
    /// The use slice length is not the header's `use_record_count`.
    UseCountMismatch,
    /// The patch slice length is not the header's `patch_record_count`.
    PatchCountMismatch,
    /// The COMMIT allocation list is larger than
    /// [`HELIOS_HNR2_MAX_USE_RECORDS`].
    AllocationListTooLarge,
    /// A use record carries a bit outside [`HELIOS_HNR2_ACCESS_MASK`].
    AccessFlagsUnknownBits,
    /// A use record claims neither `READ` nor `WRITE`. A listed allocation that
    /// is neither read nor written cannot be part of the batch's closure.
    AccessFlagsZero,
    /// A use record's `expected_allocation_generation` is zero; every KMD
    /// allocation object has a nonzero generation.
    AllocationGenerationZero,
    /// A use or patch record indexes outside the COMMIT allocation list.
    AllocationIndexOutOfRange,
    /// An allocation appears in more than one use record. Each reachable
    /// allocation appears exactly once.
    AllocationUsedTwice,
    /// A use record's patch run does not start where the previous one ended.
    PatchRunNotContiguous,
    /// A use record's patch run overflows, or runs past the patch table.
    PatchRunOutOfRange,
    /// Patch records exist that no use record's run covers.
    PatchRunLeavesGap,
    /// A patch record names a different allocation than its owning use record.
    PatchAllocationMismatch,
    /// A patch record's `operand_kind` is outside the generated vocabulary.
    PatchOperandKindUnknown,
    /// A patch record's `encoded_width` does not match its `operand_kind`.
    PatchOperandWidthMismatch,
    /// A patch record's `payload_offset` is not
    /// [`HELIOS_HNR2_OPERAND_ALIGN`]-aligned.
    PatchOffsetMisaligned,
    /// A patch record's operand does not lie wholly inside the reassembled
    /// payload.
    PatchOffsetOutOfPayload,
    /// A patch record's `reserved` field is nonzero.
    PatchReservedNonZero,
    /// A use record's `WRITE` bit disagrees with the allocation list entry's
    /// `WriteOperation` ([`validate_use_write_operation`]).
    WriteOperationMismatch,
    /// `HAS_REPLY` names an allocation-list entry whose use record does not
    /// carry [`HELIOS_HNR2_ACCESS_WRITE`]. Section 10.7's reply row is "valid
    /// **writable** index with `HAS_REPLY`", and section 10.4 lets a control
    /// request "write only the exact slot … named writable in that Render's
    /// allocation list": the reply target is the sole legal write of the pure
    /// control class, so a read-only target is not a reply target.
    ReplyIndexNotWritable,
    /// `HAS_REPLY` names an allocation-list entry that no use record claims. The
    /// use table is a bijection onto the COMMIT allocation list, so this is only
    /// reachable when the caller passed tables that
    /// [`HeliosNativeRenderV2::validate`] has not accepted.
    ReplyIndexHasNoUseRecord,
}

impl Hnr2TableReject {
    /// Stable numeric reason code for a KMD counter / ETW field.
    pub const fn code(self) -> u32 {
        match self {
            Self::UseCountMismatch => 0x0301,
            Self::PatchCountMismatch => 0x0302,
            Self::AllocationListTooLarge => 0x0303,
            Self::AccessFlagsUnknownBits => 0x0304,
            Self::AccessFlagsZero => 0x0305,
            Self::AllocationGenerationZero => 0x0306,
            Self::AllocationIndexOutOfRange => 0x0307,
            Self::AllocationUsedTwice => 0x0308,
            Self::PatchRunNotContiguous => 0x0309,
            Self::PatchRunOutOfRange => 0x030A,
            Self::PatchRunLeavesGap => 0x030B,
            Self::PatchAllocationMismatch => 0x030C,
            Self::PatchOperandKindUnknown => 0x030D,
            Self::PatchOperandWidthMismatch => 0x030E,
            Self::PatchOffsetMisaligned => 0x030F,
            Self::PatchOffsetOutOfPayload => 0x0310,
            Self::PatchReservedNonZero => 0x0311,
            Self::WriteOperationMismatch => 0x0312,
            Self::ReplyIndexNotWritable => 0x0313,
            Self::ReplyIndexHasNoUseRecord => 0x0314,
        }
    }
}

/// The encoded width one operand kind must carry, or `None` if the kind is
/// outside the generated vocabulary.
pub const fn hnr2_operand_width(operand_kind: u16) -> Option<u16> {
    match operand_kind {
        HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32 => Some(HELIOS_HNR2_OPERAND_WIDTH_32),
        HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64 => Some(HELIOS_HNR2_OPERAND_WIDTH_64),
        _ => None,
    }
}

/// Cross-check one use record's access against the OS allocation-list entry.
///
/// "Every directly or transitively reachable allocation in the legacy batch
/// appears exactly once in the returned `D3DDDI_ALLOCATIONLIST` with the same
/// `WriteOperation`." The list entry is a WDDM structure this crate cannot see,
/// so the KMD passes the bit in.
pub fn validate_use_write_operation(
    record: &HeliosNativeRenderUse,
    list_write_operation: bool,
) -> Result<(), Hnr2TableReject> {
    let record_write = record.access_flags & HELIOS_HNR2_ACCESS_WRITE != 0;
    if record_write != list_write_operation {
        return Err(Hnr2TableReject::WriteOperationMismatch);
    }
    Ok(())
}

/// Total validation of a COMMIT's use and typed-patch tables.
///
/// Enforces, in one pass and with no allocation:
///   * one use record per allocation-list entry, each index in range and used
///     **exactly once** (the 4096-bit stack bitmap below, sized by the
///     `HELIOS_HNR2_MAX_USE_RECORDS % 64 == 0` assertion in this module's
///     layout block);
///   * access bits drawn only from `{READ, WRITE}` and never empty;
///   * every patch record owned by exactly one use record — the runs tile
///     `[0, patch_record_count)` in order, so "aligned/non-overlapping" holds by
///     construction rather than by a quadratic overlap search;
///   * every operand kind/width/offset inside the generated vocabulary and
///     wholly inside the reassembled payload; and
///   * the `HAS_REPLY` target being a **writable** entry — the "valid writable
///     index" half of section 10.7's reply row, which the header alone cannot
///     check because writability lives in the use table.
///
/// It does **not** read the payload: whether the operand bytes at each offset
/// are the required zero placeholder is checked by the KMD's re-parse of the
/// assembled stream against the generated opcode schema.
pub fn validate_commit_tables(
    header: &HeliosNativeRenderV2,
    uses: &[HeliosNativeRenderUse],
    patches: &[HeliosNativeRenderPatch],
    allocation_list_count: u32,
) -> Result<(), Hnr2TableReject> {
    use Hnr2TableReject as R;

    if uses.len() as u64 != header.use_record_count as u64 {
        return Err(R::UseCountMismatch);
    }
    if patches.len() as u64 != header.patch_record_count as u64 {
        return Err(R::PatchCountMismatch);
    }
    if allocation_list_count > HELIOS_HNR2_MAX_USE_RECORDS {
        return Err(R::AllocationListTooLarge);
    }

    // 4096 bits: one per legal allocation-list index. Fixed size, no alloc, and
    // 512 bytes of PASSIVE_LEVEL stack.
    let mut seen = [0u64; (HELIOS_HNR2_MAX_USE_RECORDS / 64) as usize];
    let mut next_patch: u32 = 0;
    let has_reply = header.flags & HELIOS_HNR2_FLAG_HAS_REPLY != 0;
    let mut reply_target_is_writable = false;
    let mut reply_target_seen = false;

    for record in uses {
        if record.access_flags & !HELIOS_HNR2_ACCESS_MASK != 0 {
            return Err(R::AccessFlagsUnknownBits);
        }
        if record.access_flags == 0 {
            return Err(R::AccessFlagsZero);
        }
        if record.expected_allocation_generation == 0 {
            return Err(R::AllocationGenerationZero);
        }
        if record.allocation_list_index >= allocation_list_count {
            return Err(R::AllocationIndexOutOfRange);
        }
        let word = seen
            .get_mut((record.allocation_list_index / 64) as usize)
            .ok_or(R::AllocationIndexOutOfRange)?;
        let bit = 1u64 << (record.allocation_list_index % 64);
        if *word & bit != 0 {
            return Err(R::AllocationUsedTwice);
        }
        *word |= bit;

        if has_reply && record.allocation_list_index == header.reply_allocation_list_index {
            reply_target_seen = true;
            reply_target_is_writable = record.access_flags & HELIOS_HNR2_ACCESS_WRITE != 0;
        }

        if record.first_patch != next_patch {
            return Err(R::PatchRunNotContiguous);
        }
        let run_end = record
            .first_patch
            .checked_add(record.patch_count)
            .ok_or(R::PatchRunOutOfRange)?;
        if run_end as u64 > patches.len() as u64 {
            return Err(R::PatchRunOutOfRange);
        }
        let run = patches
            .get(record.first_patch as usize..run_end as usize)
            .ok_or(R::PatchRunOutOfRange)?;
        for patch in run {
            if patch.reserved != 0 {
                return Err(R::PatchReservedNonZero);
            }
            if patch.allocation_list_index != record.allocation_list_index {
                return Err(R::PatchAllocationMismatch);
            }
            let width = hnr2_operand_width(patch.operand_kind).ok_or(R::PatchOperandKindUnknown)?;
            if patch.encoded_width != width {
                return Err(R::PatchOperandWidthMismatch);
            }
            if patch.payload_offset % HELIOS_HNR2_OPERAND_ALIGN != 0 {
                return Err(R::PatchOffsetMisaligned);
            }
            let operand_end = patch.payload_offset as u64 + width as u64;
            if operand_end > header.total_payload_bytes {
                return Err(R::PatchOffsetOutOfPayload);
            }
        }
        next_patch = run_end;
    }

    if next_patch as u64 != patches.len() as u64 {
        return Err(R::PatchRunLeavesGap);
    }
    if has_reply {
        if !reply_target_seen {
            return Err(R::ReplyIndexHasNoUseRecord);
        }
        if !reply_target_is_writable {
            return Err(R::ReplyIndexNotWritable);
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// HVM1 — the ordinary WDDM allocation role vocabulary
// ─────────────────────────────────────────────────────────────────────────────

/// `'HVM1'` little-endian (`0x314d5648`).
pub const HELIOS_HVM1_MAGIC: u32 = 0x314D_5648;
/// HVM1 ABI version (section 10.7 table, offset 4).
pub const HELIOS_HVM1_ABI_VERSION: u16 = 1;
/// HVM1 structure size (section 10.7 table, offset 6).
pub const HELIOS_HVM1_SIZE: u16 = 64;

/// Role 1 — the HTS1 session's one reply/feedback pool: a 4-MiB allocation
/// divided into four fixed 1-MiB slots, Lock2-mapped once after create and
/// residency and retained until pool teardown.
pub const HELIOS_HVM1_ROLE_REPLY_POOL: u32 = 1;
/// Role 2 — an ordinary application `VkDeviceMemory` from the
/// `DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT` memory type. Lock2 is taken at the
/// Vulkan map boundary and held only for that map's lifetime.
pub const HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE: u32 = 2;
/// Role 3 — Venus feedback storage.
pub const HELIOS_HVM1_ROLE_FEEDBACK: u32 = 3;
/// Role 4 — an ordinary application `VkDeviceMemory` from the `DEVICE_LOCAL`
/// only memory type. **Rejects map and has no CPU VA**; it may never be passed
/// to Lock2.
pub const HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL: u32 = 4;

/// [`HeliosVenusMemoryAllocationV1::access`] — the CPU reads these bytes through
/// the Lock2 view.
pub const HELIOS_HVM1_ACCESS_CPU_READ: u32 = 1;
/// [`HeliosVenusMemoryAllocationV1::access`] — the CPU writes these bytes.
pub const HELIOS_HVM1_ACCESS_CPU_WRITE: u32 = 2;
/// [`HeliosVenusMemoryAllocationV1::access`] — the host/renderer reads them.
pub const HELIOS_HVM1_ACCESS_HOST_READ: u32 = 4;
/// [`HeliosVenusMemoryAllocationV1::access`] — the host/renderer writes them.
pub const HELIOS_HVM1_ACCESS_HOST_WRITE: u32 = 8;
/// "only `CPU_READ=1`, `CPU_WRITE=2`, `HOST_READ=4`, `HOST_WRITE=8`".
pub const HELIOS_HVM1_ACCESS_MASK: u32 = HELIOS_HVM1_ACCESS_CPU_READ
    | HELIOS_HVM1_ACCESS_CPU_WRITE
    | HELIOS_HVM1_ACCESS_HOST_READ
    | HELIOS_HVM1_ACCESS_HOST_WRITE;

/// [`HeliosVenusMemoryAllocationV1::cache_policy`] — not CPU visible; **role 4
/// only**.
pub const HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE: u32 = 0;
/// [`HeliosVenusMemoryAllocationV1::cache_policy`] — write-combined; **roles 1-3
/// only**. Khronos defines uncached host memory as coherent, which is the whole
/// basis on which role 2 is advertised `HOST_COHERENT` without a flush protocol.
pub const HELIOS_HVM1_CACHE_WRITE_COMBINED: u32 = 1;

/// The segment page shift the KMD returns in this generation.
pub const HELIOS_HVM1_SEGMENT_PAGE_SHIFT: u32 = 12;

/// The role-1 pool's exact byte size.
pub const HELIOS_HVM1_REPLY_POOL_BYTES: u64 = 4 * 1024 * 1024;
/// One reply slot's byte size.
pub const HELIOS_HVM1_REPLY_SLOT_BYTES: u64 = 1024 * 1024;
/// Slots per role-1 pool.
pub const HELIOS_HVM1_REPLY_SLOT_COUNT: u32 = 4;

/// The four exact storage roles. An HVM1 naming anything else is refused: this
/// is a closed vocabulary, not an extension point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hvm1Role {
    /// [`HELIOS_HVM1_ROLE_REPLY_POOL`].
    ReplyPool,
    /// [`HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE`].
    VulkanHostVisible,
    /// [`HELIOS_HVM1_ROLE_FEEDBACK`].
    Feedback,
    /// [`HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL`].
    VulkanDeviceLocal,
}

/// The per-role WDDM placement/flags contract from section 10.7. Not a wire
/// struct: it is the exact set of `DXGK_ALLOCATIONINFOFLAGS`/`Flags2` bits and
/// segment choices the KMD must produce for a role, stated once so the KMD
/// cannot express it differently per call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hvm1Placement {
    /// `DXGK_ALLOCATIONINFOFLAGS::CpuVisible`.
    pub cpu_visible: bool,
    /// `DXGK_ALLOCATIONINFOFLAGS::Cached` — zero for every role.
    pub cached: bool,
    /// `DXGK_ALLOCATIONINFOFLAGS::AccessedPhysically` — set for every role, and
    /// truthful: the legacy physical Render engine really does dereference the
    /// final segment/physical-address capability patched from the allocation
    /// list. It is not set merely to ask for contiguity.
    pub accessed_physically: bool,
    /// `DXGK_ALLOCATIONINFOFLAGS::ExplicitResidencyNotification` — set for every
    /// role.
    pub explicit_residency_notification: bool,
    /// `Flags2::DisablePartialResidency` — set for every role.
    pub disable_partial_residency: bool,
    /// `Flags2::RestrictedToSingleSegment` — set for every role. Whole-allocation
    /// residency in one segment at a time; **not** a claim that an offset is
    /// pinned.
    pub restricted_to_single_segment: bool,
    /// Preferred read/write segment: the WDDM aperture. Shared-backing
    /// allocations must reside only in an aperture segment.
    pub preferred_segment: u32,
    /// The exact [`HeliosVenusMemoryAllocationV1::cache_policy`] for this role.
    pub cache_policy: u32,
    /// May this role ever be passed to `D3DKMTLock2`?
    pub lockable: bool,
}

impl Hvm1Role {
    /// Decode the wire value. `None` for anything outside the closed vocabulary.
    pub const fn from_u32(value: u32) -> Option<Self> {
        match value {
            HELIOS_HVM1_ROLE_REPLY_POOL => Some(Self::ReplyPool),
            HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE => Some(Self::VulkanHostVisible),
            HELIOS_HVM1_ROLE_FEEDBACK => Some(Self::Feedback),
            HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL => Some(Self::VulkanDeviceLocal),
            _ => None,
        }
    }

    /// The wire value.
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::ReplyPool => HELIOS_HVM1_ROLE_REPLY_POOL,
            Self::VulkanHostVisible => HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE,
            Self::Feedback => HELIOS_HVM1_ROLE_FEEDBACK,
            Self::VulkanDeviceLocal => HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL,
        }
    }

    /// Access bits this role may carry.
    ///
    /// ⚠ Section 10.7 says "exact role-compatible subset" without giving the
    /// table. The compatibility facts it *does* state are that roles 1-3 are
    /// `CpuVisible=1` and role 4 is `CpuVisible=0` and "may never be passed to
    /// Lock2". This lane therefore forbids both CPU bits on role 4 and admits
    /// the rest, pairing each role with a [`Self::required_access`] minimum so
    /// that a zero or nonsensical access word is refused rather than accepted as
    /// a "subset".
    pub const fn permitted_access(self) -> u32 {
        match self {
            Self::ReplyPool | Self::VulkanHostVisible | Self::Feedback => HELIOS_HVM1_ACCESS_MASK,
            // No CPU VA exists for role 4, so no CPU access can be truthful.
            Self::VulkanDeviceLocal => HELIOS_HVM1_ACCESS_HOST_READ | HELIOS_HVM1_ACCESS_HOST_WRITE,
        }
    }

    /// Access bits this role must carry.
    pub const fn required_access(self) -> u32 {
        match self {
            // The host publishes replies/feedback; the CPU decodes them.
            Self::ReplyPool | Self::Feedback => {
                HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_HOST_WRITE
            }
            // Advertised `HOST_VISIBLE`: the Vulkan map is readable and writable.
            Self::VulkanHostVisible => HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_CPU_WRITE,
            // Device-local memory the host renderer owns outright.
            Self::VulkanDeviceLocal => HELIOS_HVM1_ACCESS_HOST_READ,
        }
    }

    /// The exact cache policy this role must declare.
    pub const fn cache_policy(self) -> u32 {
        match self {
            Self::ReplyPool | Self::VulkanHostVisible | Self::Feedback => {
                HELIOS_HVM1_CACHE_WRITE_COMBINED
            }
            Self::VulkanDeviceLocal => HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE,
        }
    }

    /// The exact byte size this role must request, when the role fixes one.
    pub const fn exact_byte_size(self) -> Option<u64> {
        match self {
            Self::ReplyPool => Some(HELIOS_HVM1_REPLY_POOL_BYTES),
            _ => None,
        }
    }

    /// The exact WDDM placement/flags contract for this role.
    pub const fn placement(self) -> Hvm1Placement {
        let cpu_visible = !matches!(self, Self::VulkanDeviceLocal);
        Hvm1Placement {
            cpu_visible,
            cached: false,
            accessed_physically: true,
            explicit_residency_notification: true,
            disable_partial_residency: true,
            restricted_to_single_segment: true,
            preferred_segment: HELIOS_SEGMENT_ID_APERTURE,
            cache_policy: self.cache_policy(),
            lockable: cpu_visible,
        }
    }
}

/// Create/open record for one ordinary native-Vulkan WDDM allocation (`HVM1`).
/// 64 bytes, pointer-free.
///
/// It is the **only** per-allocation private data on a
/// `D3DKMTCreateAllocation2` in this lane, which is zeroed and issued with
/// `hResource=0`, one allocation, `pSystemMem=NULL`, priority `NORMAL`,
/// `VidPnSourceId=D3DDDI_ID_NOTAPPLICABLE`, and the resource flags
/// `CreateResource=1`, `CreateShared=1`, and `NtSecuritySharing=1`.
/// `ExistingSysMem`, `ExistingKernelSysMem`, `ExistingSection`, and
/// `PermanentSysMem` remain zero.
///
/// Three fields are **write-back**: `object_generation`, `segment_page_shift`,
/// and `allocation_alignment` are zero on input and filled by the KMD. Validate
/// with [`Hvm1Stage::CreateInput`] before the call and
/// [`Hvm1Stage::CreateOutput`] after it — the same struct, two different exact
/// contracts.
///
/// Section 10.7 field table (offset/size/rule) is asserted below.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeliosVenusMemoryAllocationV1 {
    /// == [`HELIOS_HVM1_MAGIC`].
    pub magic: u32,
    /// == [`HELIOS_HVM1_ABI_VERSION`].
    pub abi_version: u16,
    /// == [`HELIOS_HVM1_SIZE`].
    pub struct_size: u16,
    /// Exact atomic-package generation.
    pub package_generation: u64,
    /// Zero on input; the KMD writes a nonzero diagnostic generation.
    ///
    /// ⭐ This is the opaque local allocation capability Mesa stores **instead of
    /// a virtio resource id**, and repeats in
    /// [`HeliosNativeRenderUse::expected_allocation_generation`].
    pub object_generation: u64,
    /// Nonzero exact allocation/renderer-view size.
    pub byte_size: u64,
    /// `HELIOS_HVM1_ROLE_*`.
    pub role: u32,
    /// `HELIOS_HVM1_ACCESS_*`, an exact role-compatible subset.
    pub access: u32,
    /// `HELIOS_HVM1_CACHE_*`, exact for the role.
    pub cache_policy: u32,
    /// Zero on input; the KMD returns the selected segment's page shift, which
    /// is [`HELIOS_HVM1_SEGMENT_PAGE_SHIFT`] in this generation.
    pub segment_page_shift: u32,
    /// Zero on input; the KMD returns the exact alignment.
    pub allocation_alignment: u64,
    /// Reserved; validated as zero in both directions.
    pub reserved: u64,
}

/// Which side of the `DxgkDdiCreateAllocation` write-back is being validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hvm1Stage {
    /// The bytes user mode supplies: the three write-back fields must be zero.
    CreateInput,
    /// The bytes the KMD returns: the three write-back fields must be exact.
    CreateOutput,
}

/// Why [`HeliosVenusMemoryAllocationV1::validate`] refused. Codes are stable and
/// append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hvm1Reject {
    /// `magic` is not [`HELIOS_HVM1_MAGIC`].
    MagicMismatch,
    /// `abi_version` is not [`HELIOS_HVM1_ABI_VERSION`].
    AbiVersionMismatch,
    /// `struct_size` is not [`HELIOS_HVM1_SIZE`].
    StructSizeMismatch,
    /// The caller has no admitted package generation.
    PackageGenerationUnset,
    /// `package_generation` names a different package.
    PackageGenerationMismatch,
    /// `reserved` is nonzero.
    ReservedNonZero,
    /// `role` is outside the closed four-role vocabulary.
    RoleUnknown,
    /// `byte_size` is zero.
    ByteSizeZero,
    /// `byte_size` is not the exact size the role fixes (role 1: 4 MiB).
    ByteSizeNotExactForRole,
    /// `access` carries a bit outside [`HELIOS_HVM1_ACCESS_MASK`].
    AccessUnknownBits,
    /// `access` carries a bit this role forbids — notably a CPU bit on role 4,
    /// which has no CPU VA at all.
    AccessNotRoleCompatible,
    /// `access` is missing a bit this role requires.
    AccessMissingRequired,
    /// `cache_policy` is not the exact value for this role.
    CachePolicyNotRoleExact,
    /// A write-back field was nonzero on input.
    WriteBackFieldNotZeroOnInput,
    /// The KMD returned a zero `object_generation`.
    ObjectGenerationZeroOnOutput,
    /// The KMD returned a `segment_page_shift` other than
    /// [`HELIOS_HVM1_SEGMENT_PAGE_SHIFT`].
    SegmentPageShiftUnexpected,
    /// The KMD returned a zero or non-power-of-two alignment.
    AllocationAlignmentInvalid,
    /// The `(pPrivateDriverData, PrivateDriverDataSize)` buffer is not exactly
    /// [`HELIOS_HVM1_SIZE`] bytes. Appended when
    /// [`HeliosVenusMemoryAllocationV1::from_private_data`] was added; the codes
    /// above are stable and this one continues them.
    PrivateDataSizeMismatch,
}

impl Hvm1Reject {
    /// Stable numeric reason code for a KMD/ICD counter or ETW field.
    pub const fn code(self) -> u32 {
        match self {
            Self::MagicMismatch => 0x0401,
            Self::AbiVersionMismatch => 0x0402,
            Self::StructSizeMismatch => 0x0403,
            Self::PackageGenerationUnset => 0x0404,
            Self::PackageGenerationMismatch => 0x0405,
            Self::ReservedNonZero => 0x0406,
            Self::RoleUnknown => 0x0407,
            Self::ByteSizeZero => 0x0408,
            Self::ByteSizeNotExactForRole => 0x0409,
            Self::AccessUnknownBits => 0x040A,
            Self::AccessNotRoleCompatible => 0x040B,
            Self::AccessMissingRequired => 0x040C,
            Self::CachePolicyNotRoleExact => 0x040D,
            Self::WriteBackFieldNotZeroOnInput => 0x040E,
            Self::ObjectGenerationZeroOnOutput => 0x040F,
            Self::SegmentPageShiftUnexpected => 0x0410,
            Self::AllocationAlignmentInvalid => 0x0411,
            Self::PrivateDataSizeMismatch => 0x0412,
        }
    }
}

impl HeliosVenusMemoryAllocationV1 {
    /// Build the create-time input record for `role`. The three write-back
    /// fields are zero, as the input contract requires.
    pub const fn new(package_generation: u64, role: Hvm1Role, byte_size: u64, access: u32) -> Self {
        Self {
            magic: HELIOS_HVM1_MAGIC,
            abi_version: HELIOS_HVM1_ABI_VERSION,
            struct_size: HELIOS_HVM1_SIZE,
            package_generation,
            object_generation: 0,
            byte_size,
            role: role.to_u32(),
            access,
            cache_policy: role.cache_policy(),
            segment_page_shift: 0,
            allocation_alignment: 0,
            reserved: 0,
        }
    }

    /// The session's one role-1 reply/feedback pool: exactly 4 MiB, four 1-MiB
    /// slots, host-written and CPU-read.
    pub const fn new_reply_pool(package_generation: u64) -> Self {
        Self::new(
            package_generation,
            Hvm1Role::ReplyPool,
            HELIOS_HVM1_REPLY_POOL_BYTES,
            HELIOS_HVM1_ACCESS_CPU_READ
                | HELIOS_HVM1_ACCESS_CPU_WRITE
                | HELIOS_HVM1_ACCESS_HOST_READ
                | HELIOS_HVM1_ACCESS_HOST_WRITE,
        )
    }

    /// Read one HVM1 out of a `(pPrivateDriverData, PrivateDriverDataSize)`
    /// pair that must be exactly [`HELIOS_HVM1_SIZE`] bytes long.
    ///
    /// Owned and unaligned for the same reason as
    /// [`crate::wddm::HeliosWddmAllocationDescV2::from_private_data`] and
    /// [`crate::wddm::HeliosOuterCommandAllocationV1::from_private_data`], and
    /// carrying the identical obligation: the runtime's buffer carries no
    /// alignment promise, and a KMD that read 64 bytes out of a shorter one
    /// would take an out-of-bounds **kernel** read this crate could not catch.
    /// Every consumer must enter through here rather than casting the pointer.
    ///
    /// ⚠ Like HOC1's, this is the validation entry point and not a substitute
    /// for the write-back: `object_generation`, `segment_page_shift` and
    /// `allocation_alignment` are filled in the *runtime's own buffer* at
    /// create, so a KMD that parses through here must write those bytes back
    /// through the original pointer. It also does not validate — call
    /// [`Self::validate`] with the right [`Hvm1Stage`] on the returned record.
    pub fn from_private_data(bytes: &[u8]) -> Result<Self, Hvm1Reject> {
        if bytes.len() != core::mem::size_of::<Self>() {
            return Err(Hvm1Reject::PrivateDataSizeMismatch);
        }
        // With the length already exact, `try_pod_read_unaligned` cannot fail;
        // the arm is kept because a total function may not `unwrap`.
        bytemuck::try_pod_read_unaligned::<Self>(bytes)
            .map_err(|_| Hvm1Reject::PrivateDataSizeMismatch)
    }

    /// Total validation of one HVM1 record at the given stage.
    pub fn validate(
        &self,
        expected_package_generation: u64,
        stage: Hvm1Stage,
    ) -> Result<Hvm1Role, Hvm1Reject> {
        use Hvm1Reject as R;

        if self.magic != HELIOS_HVM1_MAGIC {
            return Err(R::MagicMismatch);
        }
        if self.abi_version != HELIOS_HVM1_ABI_VERSION {
            return Err(R::AbiVersionMismatch);
        }
        if self.struct_size != HELIOS_HVM1_SIZE {
            return Err(R::StructSizeMismatch);
        }
        match admit_package_generation(self.package_generation, expected_package_generation) {
            Ok(()) => {}
            Err(GenerationCheck::Unset) => return Err(R::PackageGenerationUnset),
            Err(GenerationCheck::Mismatch) => return Err(R::PackageGenerationMismatch),
        }
        if self.reserved != 0 {
            return Err(R::ReservedNonZero);
        }

        let role = Hvm1Role::from_u32(self.role).ok_or(R::RoleUnknown)?;
        if self.byte_size == 0 {
            return Err(R::ByteSizeZero);
        }
        if let Some(exact) = role.exact_byte_size() {
            if self.byte_size != exact {
                return Err(R::ByteSizeNotExactForRole);
            }
        }
        if self.access & !HELIOS_HVM1_ACCESS_MASK != 0 {
            return Err(R::AccessUnknownBits);
        }
        if self.access & !role.permitted_access() != 0 {
            return Err(R::AccessNotRoleCompatible);
        }
        if self.access & role.required_access() != role.required_access() {
            return Err(R::AccessMissingRequired);
        }
        if self.cache_policy != role.cache_policy() {
            return Err(R::CachePolicyNotRoleExact);
        }

        match stage {
            Hvm1Stage::CreateInput => {
                if self.object_generation != 0
                    || self.segment_page_shift != 0
                    || self.allocation_alignment != 0
                {
                    return Err(R::WriteBackFieldNotZeroOnInput);
                }
            }
            Hvm1Stage::CreateOutput => {
                if self.object_generation == 0 {
                    return Err(R::ObjectGenerationZeroOnOutput);
                }
                if self.segment_page_shift != HELIOS_HVM1_SEGMENT_PAGE_SHIFT {
                    return Err(R::SegmentPageShiftUnexpected);
                }
                if self.allocation_alignment == 0 || !self.allocation_alignment.is_power_of_two() {
                    return Err(R::AllocationAlignmentInvalid);
                }
            }
        }
        Ok(role)
    }
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md section 10.7 — HVM1 field table.
const _: () = {
    assert!(core::mem::size_of::<HeliosVenusMemoryAllocationV1>() == HELIOS_HVM1_SIZE as usize);
    assert!(core::mem::align_of::<HeliosVenusMemoryAllocationV1>() == 8);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, object_generation) == 16);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, byte_size) == 24);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, role) == 32);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, access) == 36);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, cache_policy) == 40);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, segment_page_shift) == 44);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, allocation_alignment) == 48);
    assert!(core::mem::offset_of!(HeliosVenusMemoryAllocationV1, reserved) == 56);

    // The role-1 pool is exactly four slots.
    assert!(
        HELIOS_HVM1_REPLY_SLOT_BYTES * HELIOS_HVM1_REPLY_SLOT_COUNT as u64
            == HELIOS_HVM1_REPLY_POOL_BYTES
    );
    // The header and maximum reply chunk consume one complete slot.
    assert!(
        HELIOS_HVR1_HEADER_SIZE as u64 + HELIOS_HVR1_MAX_CHUNK_BYTES
            == HELIOS_HVM1_REPLY_SLOT_BYTES
    );
};

// ─────────────────────────────────────────────────────────────────────────────
// HVR1 — the bounded reply header and its continuation snapshots
// ─────────────────────────────────────────────────────────────────────────────

/// `'HVR1'` little-endian (`0x31525648`).
pub const HELIOS_HVR1_MAGIC: u32 = 0x3152_5648;
/// HVR1 version (section 10.7 table, offset 4).
pub const HELIOS_HVR1_VERSION: u16 = 1;
/// HVR1 header size (section 10.7 table, offset 6). The payload follows at byte
/// 80.
pub const HELIOS_HVR1_HEADER_SIZE: u16 = 80;

/// Maximum immutable logical-result size of one snapshot: 64 MiB.
pub const HELIOS_HVR1_MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum live immutable snapshots per HTS1 session.
pub const HELIOS_HVR1_MAX_LIVE_SNAPSHOTS: u32 = 4;
/// Maximum live snapshot bytes per HTS1 session: 256 MiB.
pub const HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum bytes one HNR2 transaction may publish behind one HVR1 header.
pub const HELIOS_HVR1_MAX_CHUNK_BYTES: u64 =
    HELIOS_HVM1_REPLY_SLOT_BYTES - HELIOS_HVR1_HEADER_SIZE as u64;

// ── Capacity admission for the two native-side bounded pools ───────────────
//
// §17.1 requires this file to carry the section-10.7 bounds; a `const` alone is
// not a bound. §10.7: the KMD staging pool is "capped at 64 outstanding
// submissions and 15 MiB total" and "exhaustion returns resource failure and
// never waits, scans another context, or spills to a global queue"; snapshots
// are capped at "at most four snapshots and 256 MiB of snapshot bytes … per
// HTS1 session" and "the fifth caller drops the slot/snapshot lock and
// event-waits for the oldest exact C51 owner". Both are refusals a caller must
// be able to make without re-deriving the comparison.

/// Which native-side bounded pool a [`Hnr2CapacityRefusal`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hnr2CapacityLimit {
    /// [`HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS`] — staging slots checked out
    /// on one context.
    OutstandingSubmissions,
    /// [`HELIOS_HNR2_SLOT_POOL_BYTES`] — bytes staged on one context.
    SlotPoolBytes,
    /// [`HELIOS_HVR1_MAX_LIVE_SNAPSHOTS`] — live immutable snapshots on one
    /// session.
    LiveSnapshots,
    /// [`HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES`] — live snapshot bytes on one
    /// session.
    LiveSnapshotBytes,
    /// [`HELIOS_HVR1_MAX_SNAPSHOT_BYTES`] — bytes in one snapshot.
    SnapshotBytes,
}

/// A bounded native-side pool was exhausted. ⛔ Never a wait and never a spill:
/// the staging-pool arms are a resource failure, and the snapshot arms are the
/// point at which the caller drops its lock and event-waits for the oldest
/// exact C51 owner — the wait is the *caller's*, ordered by a completion, not a
/// retry loop inside this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hnr2CapacityRefusal {
    pub limit: Hnr2CapacityLimit,
    /// What admitting this request would make the live count / size.
    pub requested: u64,
    /// The cap.
    pub capacity: u64,
}

/// Admit one more staging slot of `bytes` on a context already holding
/// `outstanding` slots and `staged_bytes`.
#[inline]
pub const fn admit_render_slot(
    outstanding: u32,
    staged_bytes: u64,
    bytes: u64,
) -> Result<(), Hnr2CapacityRefusal> {
    if outstanding >= HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS {
        return Err(Hnr2CapacityRefusal {
            limit: Hnr2CapacityLimit::OutstandingSubmissions,
            requested: outstanding as u64 + 1,
            capacity: HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS as u64,
        });
    }
    // Checked: a caller's running total plus a wire-supplied size must not wrap
    // into an admission.
    let total = match staged_bytes.checked_add(bytes) {
        Some(total) => total,
        None => u64::MAX,
    };
    if total > HELIOS_HNR2_SLOT_POOL_BYTES {
        return Err(Hnr2CapacityRefusal {
            limit: Hnr2CapacityLimit::SlotPoolBytes,
            requested: total,
            capacity: HELIOS_HNR2_SLOT_POOL_BYTES,
        });
    }
    Ok(())
}

/// Admit one more immutable snapshot of `bytes` on a session already holding
/// `live` snapshots and `live_bytes`.
#[inline]
pub const fn admit_snapshot(
    live: u32,
    live_bytes: u64,
    bytes: u64,
) -> Result<(), Hnr2CapacityRefusal> {
    if bytes > HELIOS_HVR1_MAX_SNAPSHOT_BYTES {
        return Err(Hnr2CapacityRefusal {
            limit: Hnr2CapacityLimit::SnapshotBytes,
            requested: bytes,
            capacity: HELIOS_HVR1_MAX_SNAPSHOT_BYTES,
        });
    }
    if live >= HELIOS_HVR1_MAX_LIVE_SNAPSHOTS {
        return Err(Hnr2CapacityRefusal {
            limit: Hnr2CapacityLimit::LiveSnapshots,
            requested: live as u64 + 1,
            capacity: HELIOS_HVR1_MAX_LIVE_SNAPSHOTS as u64,
        });
    }
    let total = match live_bytes.checked_add(bytes) {
        Some(total) => total,
        None => u64::MAX,
    };
    if total > HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES {
        return Err(Hnr2CapacityRefusal {
            limit: Hnr2CapacityLimit::LiveSnapshotBytes,
            requested: total,
            capacity: HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES,
        });
    }
    Ok(())
}

/// [`HeliosVenusReplyV1::flags`] — more chunks of this snapshot remain.
pub const HELIOS_HVR1_FLAG_MORE: u32 = 1;
/// [`HeliosVenusReplyV1::flags`] — this is the last chunk.
pub const HELIOS_HVR1_FLAG_FINAL: u32 = 2;
/// "exactly one of `MORE=1` or `FINAL=2`".
pub const HELIOS_HVR1_FLAG_MASK: u32 = HELIOS_HVR1_FLAG_MORE | HELIOS_HVR1_FLAG_FINAL;

/// Header the host writes at the start of the checked-out reply-slot range,
/// immediately before the chunk payload. 80 bytes, pointer-free.
///
/// # The bounded-snapshot contract this header expresses
///
/// Reply storage is **fixed and bounded, with no growth and no spill path**:
///
///   * the host executes the original operation **once**, retains every source
///     object reference, and freezes at most [`HELIOS_HVR1_MAX_SNAPSHOT_BYTES`]
///     of result bytes plus a final status into an **immutable** snapshot with
///     one nonzero, monotonic session-local [`Self::snapshot_generation`];
///   * at most [`HELIOS_HVR1_MAX_LIVE_SNAPSHOTS`] snapshots and
///     [`HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES`] of snapshot bytes are live per
///     HTS1 session; the fifth caller drops the slot/snapshot lock and
///     event-waits for the oldest exact owner — it never grows the pool;
///   * one HNR2 transaction publishes at most one header plus
///     [`HELIOS_HVR1_MAX_CHUNK_BYTES`] into one of the four
///     [`HELIOS_HVM1_REPLY_SLOT_BYTES`] slots of the one 4-MiB role-1 pool; and
///   * a continuation ([`HeliosVenusReplyContinuationV1`]) copies **the exact
///     next bytes** of the frozen snapshot. It never re-runs the operation and
///     never changes [`Self::status`]. A result with no bounded rule is not
///     advertised at all, rather than being truncated or streamed.
///
/// Mesa validates every field here before decode. Section 10.7 field table
/// (offset/size/rule) is asserted below.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeliosVenusReplyV1 {
    /// == [`HELIOS_HVR1_MAGIC`].
    pub magic: u32,
    /// == [`HELIOS_HVR1_VERSION`].
    pub version: u16,
    /// == [`HELIOS_HVR1_HEADER_SIZE`].
    pub header_size: u16,
    /// Exact live package generation.
    pub package_generation: u64,
    /// Exact HTS1 session generation.
    pub session_generation: u64,
    /// Equals [`HeliosNativeRenderV2::reply_slot_generation`] (HNR2 offset 104).
    pub slot_generation: u64,
    /// Equals the requesting [`HeliosNativeRenderV2::batch_token`].
    pub batch_token: u64,
    /// Exact nonzero retained snapshot generation.
    pub snapshot_generation: u64,
    /// Exact generated reply opcode.
    pub opcode: u32,
    /// Exact signed Vulkan/decoder result. Positive Vulkan results such as
    /// `VK_INCOMPLETE` are legal outcomes of a bounded rule, so no validator
    /// here constrains this field.
    pub status: i32,
    /// Immutable logical-result size, at most
    /// [`HELIOS_HVR1_MAX_SNAPSHOT_BYTES`].
    pub total_bytes: u64,
    /// Exact expected next offset into the snapshot.
    pub chunk_offset: u64,
    /// At most [`HELIOS_HVR1_MAX_CHUNK_BYTES`]; the payload follows at byte 80.
    pub chunk_bytes: u32,
    /// Exactly one of [`HELIOS_HVR1_FLAG_MORE`] / [`HELIOS_HVR1_FLAG_FINAL`].
    pub flags: u32,
}

/// The generated continuation request: `{packageGeneration, sessionGeneration,
/// snapshotGeneration, expectedOffset, maxChunkBytes}` from section 10.7.
///
/// ⚠ The doc names these five fields but gives no offset table for them, because
/// the request is a *generated* command carried inside an ordinary HNR2 Venus
/// payload rather than a header the KMD parses. This fixed 40-byte little-endian
/// layout is this lane's definition of that tuple, with an explicit
/// [`Self::reserved`] so the record stays padding-free like every other struct
/// here.
///
/// It may only copy the exact next bytes of an already-frozen snapshot: it never
/// recomputes the operation and never changes the status.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
pub struct HeliosVenusReplyContinuationV1 {
    /// Exact live package generation.
    pub package_generation: u64,
    /// Exact HTS1 session generation.
    pub session_generation: u64,
    /// Exact nonzero snapshot generation being continued.
    pub snapshot_generation: u64,
    /// Exact next byte offset into that snapshot.
    pub expected_offset: u64,
    /// Bytes this transaction can accept, at most
    /// [`HELIOS_HVR1_MAX_CHUNK_BYTES`]; equals
    /// `reply_capacity_bytes - HVR1 header` of the carrying HNR2.
    pub max_chunk_bytes: u32,
    /// Reserved; validated as zero.
    pub reserved: u32,
}

/// Everything a reply consumer already knows before it reads a chunk.
#[derive(Debug, Clone, Copy)]
pub struct Hvr1Expect {
    /// The admitted atomic-package generation. Zero is refused.
    pub package_generation: u64,
    /// The exact HTS1 session generation.
    pub session_generation: u64,
    /// The exact checked-out slot generation this transaction used.
    pub slot_generation: u64,
    /// The exact batch token that requested this reply.
    pub batch_token: u64,
    /// `None` for the first chunk (the header defines the snapshot); `Some(g)`
    /// for every continuation, which must repeat it exactly.
    pub snapshot_generation: Option<u64>,
    /// `None` for the first chunk; `Some(n)` afterwards — the size is immutable
    /// once published.
    pub total_bytes: Option<u64>,
    /// Exact offset this chunk must start at (zero for the first chunk).
    pub expected_chunk_offset: u64,
    /// `maxChunkBytes` this transaction granted, from
    /// [`HeliosNativeRenderV2::granted_reply_chunk_bytes`].
    pub granted_chunk_bytes: u64,
}

/// What [`HeliosVenusReplyV1::validate`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hvr1Accept {
    /// The snapshot this chunk belongs to.
    pub snapshot_generation: u64,
    /// The immutable logical-result size.
    pub total_bytes: u64,
    /// Bytes to copy out of the slot, starting at byte
    /// [`HELIOS_HVR1_HEADER_SIZE`] of the reply range.
    pub chunk_bytes: u32,
    /// The offset a continuation must ask for next; equals `total_bytes` when
    /// [`Self::complete`].
    pub next_offset: u64,
    /// `FINAL` was set and the snapshot is fully consumed.
    pub complete: bool,
    /// The exact signed Vulkan/decoder result, passed through unmodified.
    pub status: i32,
}

/// Why [`HeliosVenusReplyV1::validate`] refused. Codes are stable and
/// append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hvr1Reject {
    /// `magic` is not [`HELIOS_HVR1_MAGIC`].
    MagicMismatch,
    /// `version` is not [`HELIOS_HVR1_VERSION`].
    VersionMismatch,
    /// `header_size` is not [`HELIOS_HVR1_HEADER_SIZE`].
    HeaderSizeMismatch,
    /// The caller has no admitted package generation.
    PackageGenerationUnset,
    /// `package_generation` names a different package.
    PackageGenerationMismatch,
    /// `session_generation` is not this session.
    SessionGenerationMismatch,
    /// `slot_generation` is not the checked-out slot's generation. A stale slot
    /// is never decoded.
    SlotGenerationMismatch,
    /// `batch_token` is not the requesting batch's token.
    BatchTokenMismatch,
    /// `snapshot_generation` is zero.
    SnapshotGenerationZero,
    /// `snapshot_generation` changed mid-snapshot.
    SnapshotGenerationMismatch,
    /// `total_bytes` exceeds [`HELIOS_HVR1_MAX_SNAPSHOT_BYTES`].
    TotalBytesTooLarge,
    /// `total_bytes` changed mid-snapshot; the logical size is immutable.
    TotalBytesChanged,
    /// `chunk_offset` is not the exact expected next offset.
    ChunkOffsetMismatch,
    /// `chunk_bytes` exceeds [`HELIOS_HVR1_MAX_CHUNK_BYTES`].
    ChunkBytesTooLarge,
    /// `chunk_bytes` exceeds the capacity this transaction granted.
    ChunkBytesExceedCapacity,
    /// The chunk runs past `total_bytes`, or the sum overflowed.
    ChunkOverrunsSnapshot,
    /// `flags` is not exactly one of `MORE` / `FINAL`.
    FlagsNotExactlyOne,
    /// `MORE` with a zero-length chunk: no progress, so a continuation loop
    /// could never terminate.
    EmptyContinuation,
    /// `MORE` although the chunk already reaches `total_bytes`.
    MoreAfterSnapshotEnd,
    /// `FINAL` although the chunk stops short of `total_bytes`. A prefix is
    /// never exposed as success.
    FinalBeforeSnapshotEnd,
}

impl Hvr1Reject {
    /// Stable numeric reason code for an ICD counter or ETW field.
    pub const fn code(self) -> u32 {
        match self {
            Self::MagicMismatch => 0x0501,
            Self::VersionMismatch => 0x0502,
            Self::HeaderSizeMismatch => 0x0503,
            Self::PackageGenerationUnset => 0x0504,
            Self::PackageGenerationMismatch => 0x0505,
            Self::SessionGenerationMismatch => 0x0506,
            Self::SlotGenerationMismatch => 0x0507,
            Self::BatchTokenMismatch => 0x0508,
            Self::SnapshotGenerationZero => 0x0509,
            Self::SnapshotGenerationMismatch => 0x050A,
            Self::TotalBytesTooLarge => 0x050B,
            Self::TotalBytesChanged => 0x050C,
            Self::ChunkOffsetMismatch => 0x050D,
            Self::ChunkBytesTooLarge => 0x050E,
            Self::ChunkBytesExceedCapacity => 0x050F,
            Self::ChunkOverrunsSnapshot => 0x0510,
            Self::FlagsNotExactlyOne => 0x0511,
            Self::EmptyContinuation => 0x0512,
            Self::MoreAfterSnapshotEnd => 0x0513,
            Self::FinalBeforeSnapshotEnd => 0x0514,
        }
    }
}

impl HeliosVenusReplyV1 {
    /// Total validation of one published reply chunk.
    pub fn validate(&self, expect: &Hvr1Expect) -> Result<Hvr1Accept, Hvr1Reject> {
        use Hvr1Reject as R;

        if self.magic != HELIOS_HVR1_MAGIC {
            return Err(R::MagicMismatch);
        }
        if self.version != HELIOS_HVR1_VERSION {
            return Err(R::VersionMismatch);
        }
        if self.header_size != HELIOS_HVR1_HEADER_SIZE {
            return Err(R::HeaderSizeMismatch);
        }
        match admit_package_generation(self.package_generation, expect.package_generation) {
            Ok(()) => {}
            Err(GenerationCheck::Unset) => return Err(R::PackageGenerationUnset),
            Err(GenerationCheck::Mismatch) => return Err(R::PackageGenerationMismatch),
        }
        if self.session_generation != expect.session_generation {
            return Err(R::SessionGenerationMismatch);
        }
        if self.slot_generation != expect.slot_generation {
            return Err(R::SlotGenerationMismatch);
        }
        if self.batch_token != expect.batch_token {
            return Err(R::BatchTokenMismatch);
        }
        if self.snapshot_generation == 0 {
            return Err(R::SnapshotGenerationZero);
        }
        if let Some(expected) = expect.snapshot_generation {
            if self.snapshot_generation != expected {
                return Err(R::SnapshotGenerationMismatch);
            }
        }
        if self.total_bytes > HELIOS_HVR1_MAX_SNAPSHOT_BYTES {
            return Err(R::TotalBytesTooLarge);
        }
        if let Some(expected) = expect.total_bytes {
            if self.total_bytes != expected {
                return Err(R::TotalBytesChanged);
            }
        }
        if self.chunk_offset != expect.expected_chunk_offset {
            return Err(R::ChunkOffsetMismatch);
        }
        if self.chunk_bytes as u64 > HELIOS_HVR1_MAX_CHUNK_BYTES {
            return Err(R::ChunkBytesTooLarge);
        }
        if self.chunk_bytes as u64 > expect.granted_chunk_bytes {
            return Err(R::ChunkBytesExceedCapacity);
        }
        let chunk_end = self
            .chunk_offset
            .checked_add(self.chunk_bytes as u64)
            .ok_or(R::ChunkOverrunsSnapshot)?;
        if chunk_end > self.total_bytes {
            return Err(R::ChunkOverrunsSnapshot);
        }

        let more = self.flags & HELIOS_HVR1_FLAG_MORE != 0;
        let final_chunk = self.flags & HELIOS_HVR1_FLAG_FINAL != 0;
        if self.flags & !HELIOS_HVR1_FLAG_MASK != 0 || more == final_chunk {
            return Err(R::FlagsNotExactlyOne);
        }
        if more {
            if self.chunk_bytes == 0 {
                return Err(R::EmptyContinuation);
            }
            if chunk_end >= self.total_bytes {
                return Err(R::MoreAfterSnapshotEnd);
            }
        } else if chunk_end != self.total_bytes {
            return Err(R::FinalBeforeSnapshotEnd);
        }

        Ok(Hvr1Accept {
            snapshot_generation: self.snapshot_generation,
            total_bytes: self.total_bytes,
            chunk_bytes: self.chunk_bytes,
            next_offset: chunk_end,
            complete: final_chunk,
            status: self.status,
        })
    }
}

/// Why [`HeliosVenusReplyContinuationV1::validate`] refused. Codes are stable
/// and append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hvr1ContinuationReject {
    /// The caller has no admitted package generation.
    PackageGenerationUnset,
    /// `package_generation` names a different package.
    PackageGenerationMismatch,
    /// `session_generation` is not this session.
    SessionGenerationMismatch,
    /// `snapshot_generation` is zero.
    SnapshotGenerationZero,
    /// `snapshot_generation` is not the retained snapshot.
    SnapshotGenerationMismatch,
    /// `expected_offset` is not the exact next byte offset.
    ExpectedOffsetMismatch,
    /// `expected_offset` is at or past the snapshot's end; a completed snapshot
    /// is consumed and dropped, never re-read.
    ExpectedOffsetPastEnd,
    /// `max_chunk_bytes` is zero: a continuation that can accept nothing makes
    /// no progress.
    MaxChunkBytesZero,
    /// `max_chunk_bytes` exceeds [`HELIOS_HVR1_MAX_CHUNK_BYTES`].
    MaxChunkBytesTooLarge,
    /// `reserved` is nonzero.
    ReservedNonZero,
}

impl Hvr1ContinuationReject {
    /// Stable numeric reason code for a host/ICD counter or ETW field.
    pub const fn code(self) -> u32 {
        match self {
            Self::PackageGenerationUnset => 0x0521,
            Self::PackageGenerationMismatch => 0x0522,
            Self::SessionGenerationMismatch => 0x0523,
            Self::SnapshotGenerationZero => 0x0524,
            Self::SnapshotGenerationMismatch => 0x0525,
            Self::ExpectedOffsetMismatch => 0x0526,
            Self::ExpectedOffsetPastEnd => 0x0527,
            Self::MaxChunkBytesZero => 0x0528,
            Self::MaxChunkBytesTooLarge => 0x0529,
            Self::ReservedNonZero => 0x052A,
        }
    }
}

impl HeliosVenusReplyContinuationV1 {
    /// Build the request for the bytes that follow an accepted chunk.
    ///
    /// `None` when the snapshot is already complete: the final consume drops the
    /// source refs and zeroes the slot, so there is nothing left to ask for.
    pub fn next(
        accepted: &Hvr1Accept,
        package_generation: u64,
        session_generation: u64,
        max_chunk_bytes: u32,
    ) -> Option<Self> {
        if accepted.complete {
            return None;
        }
        Some(Self {
            package_generation,
            session_generation,
            snapshot_generation: accepted.snapshot_generation,
            expected_offset: accepted.next_offset,
            max_chunk_bytes,
            reserved: 0,
        })
    }

    /// Total validation of one continuation request against the retained
    /// snapshot's state.
    pub fn validate(
        &self,
        expected_package_generation: u64,
        session_generation: u64,
        snapshot_generation: u64,
        snapshot_total_bytes: u64,
        snapshot_next_offset: u64,
    ) -> Result<(), Hvr1ContinuationReject> {
        use Hvr1ContinuationReject as R;

        match admit_package_generation(self.package_generation, expected_package_generation) {
            Ok(()) => {}
            Err(GenerationCheck::Unset) => return Err(R::PackageGenerationUnset),
            Err(GenerationCheck::Mismatch) => return Err(R::PackageGenerationMismatch),
        }
        if self.session_generation != session_generation {
            return Err(R::SessionGenerationMismatch);
        }
        if self.snapshot_generation == 0 {
            return Err(R::SnapshotGenerationZero);
        }
        if self.snapshot_generation != snapshot_generation {
            return Err(R::SnapshotGenerationMismatch);
        }
        if self.expected_offset != snapshot_next_offset {
            return Err(R::ExpectedOffsetMismatch);
        }
        if self.expected_offset >= snapshot_total_bytes {
            return Err(R::ExpectedOffsetPastEnd);
        }
        if self.max_chunk_bytes == 0 {
            return Err(R::MaxChunkBytesZero);
        }
        if self.max_chunk_bytes as u64 > HELIOS_HVR1_MAX_CHUNK_BYTES {
            return Err(R::MaxChunkBytesTooLarge);
        }
        if self.reserved != 0 {
            return Err(R::ReservedNonZero);
        }
        Ok(())
    }
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md section 10.7 — HVR1 field table, plus this
// lane's continuation-request layout.
const _: () = {
    assert!(core::mem::size_of::<HeliosVenusReplyV1>() == HELIOS_HVR1_HEADER_SIZE as usize);
    assert!(core::mem::align_of::<HeliosVenusReplyV1>() == 8);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, version) == 4);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, header_size) == 6);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, session_generation) == 16);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, slot_generation) == 24);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, batch_token) == 32);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, snapshot_generation) == 40);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, opcode) == 48);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, status) == 52);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, total_bytes) == 56);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, chunk_offset) == 64);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, chunk_bytes) == 72);
    assert!(core::mem::offset_of!(HeliosVenusReplyV1, flags) == 76);

    assert!(core::mem::size_of::<HeliosVenusReplyContinuationV1>() == 40);
    assert!(core::mem::align_of::<HeliosVenusReplyContinuationV1>() == 8);
    assert!(core::mem::offset_of!(HeliosVenusReplyContinuationV1, package_generation) == 0);
    assert!(core::mem::offset_of!(HeliosVenusReplyContinuationV1, session_generation) == 8);
    assert!(core::mem::offset_of!(HeliosVenusReplyContinuationV1, snapshot_generation) == 16);
    assert!(core::mem::offset_of!(HeliosVenusReplyContinuationV1, expected_offset) == 24);
    assert!(core::mem::offset_of!(HeliosVenusReplyContinuationV1, max_chunk_bytes) == 32);
    assert!(core::mem::offset_of!(HeliosVenusReplyContinuationV1, reserved) == 36);

    // At most four immutable 64-MiB snapshots, 256 MiB total, per session.
    assert!(
        HELIOS_HVR1_MAX_SNAPSHOT_BYTES * HELIOS_HVR1_MAX_LIVE_SNAPSHOTS as u64
            == HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES
    );
    assert!(HELIOS_HVR1_MAX_CHUNK_BYTES < HELIOS_HNR2_MAX_PAYLOAD_BYTES);
    assert!(
        HELIOS_HVR1_HEADER_SIZE as u64 + HELIOS_HVR1_MAX_CHUNK_BYTES
            == HELIOS_HVM1_REPLY_SLOT_BYTES
    );
};

// ─────────────────────────────────────────────────────────────────────────────
// kernel_dma — KERNEL-INTERNAL scheduler DMA records
// ─────────────────────────────────────────────────────────────────────────────

/// ⛔ **KERNEL-INTERNAL. These records exist only in scheduler DMA and are NEVER
/// returned to user mode.**
///
/// Everything in this module is written by `DxgkDdiRender`, refined by
/// `DxgkDdiPatch`, and consumed only by KMD SubmitCommand paths. No byte of
/// it is ever copied back into an ICD buffer, an Escape output, a Lock2 view, or
/// a reply slot, and no user-mode caller may construct one. The module is
/// separate so that "is this record user-visible?" is answered by a path segment
/// rather than by a comment somebody has to find:
///
///   * [`kernel_dma::Hnr2PhysicalCapability`] carries a **physical address**, a
///     segment ID, and a placement epoch. Handing any of those to user mode
///     would leak the guest-physical layout of HLM1's BAR window.
///   * [`kernel_dma::Hnr2KmdDmaPrivateV1`] is the `DmaBufferPrivateDataSize`
///     record; Dxgkrnl
///     keeps it opaque and the UMD never sees it. HVC1 contexts advertise
///     [`HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES`] and this struct is exactly that.
///
/// The pointer-free rule still holds inside the kernel: no pointer, handle, PID,
/// **host resource ID**, or name enters DMA. K6 resolves each capability against
/// its exact WDDM allocation object and snapshots placement, but does not
/// substitute a host resource id or execute an allocation-backed `SUBMIT_3D` in
/// the current package.
///
/// ⛔ **SUPERSEDED BY `docs/retirement/FINDINGS.md` F5.** The two paragraphs
/// above previously read "an HPM1 placement epoch" and "QEMU resolves each
/// capability through the exact HPM1 page owner and substitutes
/// renderer-private resource IDs only in its own host-only copy of the command
/// stream". HPM1 is declined and `qemu-helios` is at its pre-retirement base,
/// so there is no host-side resolver and no host-only copy. See the file
/// banner's "⛔⛔ no resid may appear" block for the property that trades away.
///
/// # Producer status
///
/// Re-measured 2026-08-13: K6 emits the capability table and DMA-private record,
/// Patch snapshots the exact allocation-list placement, and Submit consumes the
/// private record for staging retirement. K11 marks only an exact synchronous
/// pure-control INIT after its host reply and HVR1 publication are terminal.
/// [`kernel_dma::Hnr2PhysicalCapability::validate_at_submit`] and the general
/// allocation/GPU executor remain deliberately unreachable until their owning
/// later units exist.
///
/// The `hpm_epoch` field and [`kernel_dma::Hnr2DmaReject::PlacementEpochStale`]
/// keep their names because this file is the wire ABI and a rename is a layout
/// event with C mirrors and `_Static_assert` twins; their *meaning* is now "the
/// KMD's own placement epoch", not HPM1's. A later allocation executor must
/// preserve that meaning rather than reviving a host page-owner dependency.
///
/// Passing this file's unit tests or a source gate is not evidence that general
/// host execution exists.
pub mod kernel_dma {
    use super::{
        HELIOS_HNR2_ACCESS_MASK, HELIOS_HNR2_MAX_USE_RECORDS, HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES,
        HELIOS_SEGMENT_ID_APERTURE, HELIOS_SEGMENT_ID_HLM1,
    };
    use bytemuck::{Pod, Zeroable};

    /// Maximum output `D3DDDI_PATCHLOCATIONLIST` entries a COMMIT may emit —
    /// one per use record, so exactly [`HELIOS_HNR2_MAX_USE_RECORDS`].
    pub const HELIOS_HNR2_MAX_OUTPUT_PATCHES: u32 = HELIOS_HNR2_MAX_USE_RECORDS;

    /// One DMA-local physical capability. 48 bytes, emitted **one per COMMIT use
    /// record** and never more.
    ///
    /// `DxgkDdiRender` pre-patches `segment_id`/`physical_address`/`hpm_epoch`
    /// whenever the validated kernel allocation list already reports
    /// `SegmentId != 0`; otherwise those placement fields stay zero pending
    /// residency and `DxgkDdiPatch`. `DxgkDdiPatch` may be called repeatedly and
    /// is infallible, idempotent, and side-effect-free: it snapshots the exact
    /// allocation-list placement into this record and does no host call,
    /// mapping, allocation, or refcount transfer.
    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
    pub struct Hnr2PhysicalCapability {
        /// The KMD allocation object's generation — the same value the use
        /// record carried as `expected_allocation_generation`.
        pub allocation_generation: u64,
        /// Current segment: [`HELIOS_SEGMENT_ID_APERTURE`] or
        /// [`HELIOS_SEGMENT_ID_HLM1`]; zero means "not yet placed".
        pub segment_id: u32,
        /// `HELIOS_HNR2_ACCESS_*`, copied from the use record and the
        /// allocation list's `WriteOperation`.
        pub access_flags: u32,
        /// Current physical address of the placement. Zero while unplaced.
        pub physical_address: u64,
        /// Byte offset of the used range within the allocation.
        pub allocation_offset: u64,
        /// Byte length of the used range.
        pub byte_length: u64,
        /// Placement epoch this capability was snapshotted against. A stale
        /// epoch removes the context; it is never repaired by a lookup.
        ///
        /// ⛔ The name is HPM1's, the meaning is not: per `FINDINGS.md` F5 the
        /// epoch is the **KMD's own** placement epoch, because HPM1 is declined
        /// and no host-side page owner exists. The field keeps its wire name
        /// only because renaming it is a layout event across the C mirrors.
        pub hpm_epoch: u64,
    }

    /// This exact submission's finite K11 host operation completed in Render.
    /// The bit lives in KMD-private DMA data, never HNR2/HVR1 or user memory.
    pub const HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED: u32 = 1;
    pub const HELIOS_HNR2_KMD_DMA_FLAG_MASK: u32 =
        HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED;

    /// `HKD1` — KMD-private descriptor for one D3D11 physical HOB1 Render.
    /// It never leaves scheduler DMA private data and therefore carries no
    /// WDDM handle, GPUVA, pointer, host resource id, PID, or fallback key.
    pub const HELIOS_HOB1_KMD_DMA_MAGIC: u32 = 0x3144_4B48;
    pub const HELIOS_HOB1_KMD_DMA_ABI_VERSION: u16 = 1;
    pub const HELIOS_HOB1_KMD_DMA_BYTES: u16 = HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES as u16;

    /// Exact 64-byte Render-to-SubmitCommand custody descriptor for the D3D11
    /// outer lane. Identity remains the live context plus its executor slot;
    /// every scalar here is only an anti-stale cross-check.
    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
    pub struct Hob1KmdDmaPrivateV1 {
        pub magic: u32,
        pub abi_version: u16,
        pub struct_bytes: u16,
        pub batch_id: u64,
        pub session_generation: u64,
        pub context_generation: u64,
        pub slot_generation: u64,
        pub hob1_crc64: u64,
        pub payload_bytes: u32,
        pub ring_index: u32,
        pub slot_index: u32,
        pub flags: u32,
    }

    /// 64-byte KMD-only DMA private record for one HNR2 submission.
    ///
    /// ⚠ Section 10.7 fixes the size ("A 64-byte pointer-free KMD DMA-private
    /// record") and the field list — "device/context/slot generations, ring
    /// index, slot index, batch token, lengths, checksum, and capability-table
    /// bounds" — but gives no offset table, because nothing outside the KMD
    /// parses it. This layout is this lane's definition of that list; it is
    /// asserted at exactly [`HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES`] so the
    /// advertised `DXGK_CONTEXTINFO::DmaBufferPrivateDataSize` and the record
    /// cannot drift apart.
    #[repr(C)]
    #[derive(Debug, Default, Clone, Copy, Pod, Zeroable, PartialEq, Eq)]
    pub struct Hnr2KmdDmaPrivateV1 {
        /// The submission's HNR2 batch token.
        pub batch_token: u64,
        /// Generation of the KMD device object that owns the context.
        pub device_generation: u64,
        /// Generation of the KMD context object.
        pub context_generation: u64,
        /// Generation of the staging slot this submission holds.
        pub slot_generation: u64,
        /// The COMMIT's reassembled-payload CRC64-ECMA. Diagnostic only.
        pub full_payload_crc64: u64,
        /// Reassembled Venus payload length.
        pub payload_bytes: u32,
        /// Byte offset of the capability table inside the DMA buffer.
        pub capability_offset: u32,
        /// Capability-table entry count; equals the COMMIT use count.
        pub capability_count: u32,
        /// The context's host ring index: zero for control (decode-only
        /// retirement), nonzero for a real VkQueue timeline.
        pub ring_index: u32,
        /// Index of the staging slot within the context-local pool.
        pub slot_index: u32,
        /// KMD-private per-submission flags. Only
        /// [`HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED`] exists.
        pub flags: u32,
    }

    /// Why a kernel DMA record was refused. Codes are stable and append-only.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Hnr2DmaReject {
        /// Output patch-location count is not exactly the use count.
        OutputPatchCountNotUseCount,
        /// Use count exceeds [`HELIOS_HNR2_MAX_OUTPUT_PATCHES`].
        OutputPatchCountTooLarge,
        /// `allocation_generation` is zero.
        AllocationGenerationZero,
        /// `access_flags` carries a bit outside [`HELIOS_HNR2_ACCESS_MASK`], or
        /// is empty.
        AccessFlagsInvalid,
        /// `byte_length` is zero.
        ByteLengthZero,
        /// The used range overflows, or runs past the allocation.
        RangeOutOfAllocation,
        /// An unplaced capability (`segment_id == 0`) carries a nonzero physical
        /// address or epoch.
        UnplacedCapabilityNotZeroed,
        /// `segment_id` is neither the aperture segment nor HLM1.
        SegmentUnknown,
        /// A capability reached Submit still unplaced.
        CapabilityUnplacedAtSubmit,
        /// `allocation_generation` no longer matches the KMD allocation object.
        AllocationGenerationStale,
        /// `hpm_epoch` is not the current placement epoch. (Named for HPM1;
        /// the epoch is the KMD's own — `FINDINGS.md` F5.)
        PlacementEpochStale,
        /// The capability's segment is not the allocation's current segment.
        SegmentNotCurrent,
        /// The physical address is not page aligned.
        PhysicalAddressMisaligned,
    }

    impl Hnr2DmaReject {
        /// Stable numeric reason code for a KMD counter / ETW field.
        pub const fn code(self) -> u32 {
            match self {
                Self::OutputPatchCountNotUseCount => 0x0601,
                Self::OutputPatchCountTooLarge => 0x0602,
                Self::AllocationGenerationZero => 0x0603,
                Self::AccessFlagsInvalid => 0x0604,
                Self::ByteLengthZero => 0x0605,
                Self::RangeOutOfAllocation => 0x0606,
                Self::UnplacedCapabilityNotZeroed => 0x0607,
                Self::SegmentUnknown => 0x0608,
                Self::CapabilityUnplacedAtSubmit => 0x0609,
                Self::AllocationGenerationStale => 0x060A,
                Self::PlacementEpochStale => 0x060B,
                Self::SegmentNotCurrent => 0x060C,
                Self::PhysicalAddressMisaligned => 0x060D,
            }
        }
    }

    /// The exact one-output-patch-per-use rule: "Output count equals use count
    /// and never exceeds 4096."
    pub fn validate_output_patch_count(
        use_count: u32,
        output_patch_count: u32,
    ) -> Result<(), Hnr2DmaReject> {
        if use_count > HELIOS_HNR2_MAX_OUTPUT_PATCHES {
            return Err(Hnr2DmaReject::OutputPatchCountTooLarge);
        }
        if output_patch_count != use_count {
            return Err(Hnr2DmaReject::OutputPatchCountNotUseCount);
        }
        Ok(())
    }

    /// The current placement of one allocation, as `DxgkDdiSubmitCommand` sees
    /// it. Everything here is live KMD state, never anything the batch
    /// supplied. (Was "live KMD/HPM1 state" — HPM1 is declined, `FINDINGS.md`
    /// F5, so the KMD is the only source.)
    #[derive(Debug, Clone, Copy)]
    pub struct Hnr2CapabilityExpect {
        /// The allocation object's current generation.
        pub allocation_generation: u64,
        /// The allocation's current segment.
        pub segment_id: u32,
        /// The current placement epoch (the KMD's own — `FINDINGS.md` F5).
        pub hpm_epoch: u64,
        /// The allocation's byte size.
        pub allocation_bytes: u64,
        /// Page size of the current segment (`1 << segment_page_shift`).
        pub page_bytes: u64,
    }

    impl Hnr2PhysicalCapability {
        /// Has a placement been patched in yet?
        pub const fn is_placed(&self) -> bool {
            self.segment_id != 0
        }

        /// Shape checks a capability must pass the moment Render emits it,
        /// before any residency or Patch call.
        pub fn validate_at_render(&self, allocation_bytes: u64) -> Result<(), Hnr2DmaReject> {
            if self.allocation_generation == 0 {
                return Err(Hnr2DmaReject::AllocationGenerationZero);
            }
            if self.access_flags == 0 || self.access_flags & !HELIOS_HNR2_ACCESS_MASK != 0 {
                return Err(Hnr2DmaReject::AccessFlagsInvalid);
            }
            if self.byte_length == 0 {
                return Err(Hnr2DmaReject::ByteLengthZero);
            }
            let end = self
                .allocation_offset
                .checked_add(self.byte_length)
                .ok_or(Hnr2DmaReject::RangeOutOfAllocation)?;
            if end > allocation_bytes {
                return Err(Hnr2DmaReject::RangeOutOfAllocation);
            }
            if !self.is_placed() {
                // Placement fields stay zero until residency/Patch supplies them.
                if self.physical_address != 0 || self.hpm_epoch != 0 {
                    return Err(Hnr2DmaReject::UnplacedCapabilityNotZeroed);
                }
            } else if self.segment_id != HELIOS_SEGMENT_ID_APERTURE
                && self.segment_id != HELIOS_SEGMENT_ID_HLM1
            {
                return Err(Hnr2DmaReject::SegmentUnknown);
            }
            Ok(())
        }

        /// Full validation against the still-current KMD-side placement at
        /// `DxgkDdiSubmitCommand` (was "still-current HPM1 ownership";
        /// `FINDINGS.md` F5 declines HPM1, so the KMD's allocation objects are
        /// the only ownership record).
        /// A stale, unresident, or mismatched capability
        /// removes the context/device — it is **never** repaired by looking up a
        /// host ID.
        pub fn validate_at_submit(
            &self,
            expect: &Hnr2CapabilityExpect,
        ) -> Result<(), Hnr2DmaReject> {
            self.validate_at_render(expect.allocation_bytes)?;
            if !self.is_placed() {
                return Err(Hnr2DmaReject::CapabilityUnplacedAtSubmit);
            }
            if self.allocation_generation != expect.allocation_generation {
                return Err(Hnr2DmaReject::AllocationGenerationStale);
            }
            if self.segment_id != expect.segment_id {
                return Err(Hnr2DmaReject::SegmentNotCurrent);
            }
            if self.hpm_epoch != expect.hpm_epoch {
                return Err(Hnr2DmaReject::PlacementEpochStale);
            }
            // The zero guard is load-bearing: a caller that never established a
            // page size must not get a division, and must not be told a
            // capability is aligned.
            if expect.page_bytes == 0 || self.physical_address % expect.page_bytes != 0 {
                return Err(Hnr2DmaReject::PhysicalAddressMisaligned);
            }
            Ok(())
        }
    }

    // HELIOS_PRESENT_SYNC_RETIREMENT.md section 10.7 — the 48-byte DMA-local
    // capability and the 64-byte KMD DMA-private record.
    const _: () = {
        assert!(core::mem::size_of::<Hnr2PhysicalCapability>() == 48);
        assert!(core::mem::align_of::<Hnr2PhysicalCapability>() == 8);
        assert!(core::mem::offset_of!(Hnr2PhysicalCapability, allocation_generation) == 0);
        assert!(core::mem::offset_of!(Hnr2PhysicalCapability, segment_id) == 8);
        assert!(core::mem::offset_of!(Hnr2PhysicalCapability, access_flags) == 12);
        assert!(core::mem::offset_of!(Hnr2PhysicalCapability, physical_address) == 16);
        assert!(core::mem::offset_of!(Hnr2PhysicalCapability, allocation_offset) == 24);
        assert!(core::mem::offset_of!(Hnr2PhysicalCapability, byte_length) == 32);
        assert!(core::mem::offset_of!(Hnr2PhysicalCapability, hpm_epoch) == 40);

        assert!(core::mem::size_of::<Hob1KmdDmaPrivateV1>() == 64);
        assert!(
            core::mem::size_of::<Hob1KmdDmaPrivateV1>()
                == HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES as usize
        );
        assert!(core::mem::align_of::<Hob1KmdDmaPrivateV1>() == 8);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, magic) == 0);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, batch_id) == 8);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, session_generation) == 16);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, context_generation) == 24);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, slot_generation) == 32);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, hob1_crc64) == 40);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, payload_bytes) == 48);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, ring_index) == 52);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, slot_index) == 56);
        assert!(core::mem::offset_of!(Hob1KmdDmaPrivateV1, flags) == 60);

        assert!(
            core::mem::size_of::<Hnr2KmdDmaPrivateV1>()
                == HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES as usize
        );
        assert!(core::mem::align_of::<Hnr2KmdDmaPrivateV1>() == 8);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, batch_token) == 0);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, device_generation) == 8);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, context_generation) == 16);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, slot_generation) == 24);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, full_payload_crc64) == 32);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, payload_bytes) == 40);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, capability_offset) == 44);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, capability_count) == 48);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, ring_index) == 52);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, slot_index) == 56);
        assert!(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, flags) == 60);
        assert!(HELIOS_HNR2_KMD_DMA_FLAG_MASK == 1);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: u64 = 0x2026_0809_0000_0001;

    // ── HVC1 ─────────────────────────────────────────────────────────────────

    #[test]
    fn hvc1_classifies_control_and_queue() {
        let control = HeliosVulkanContextV1::new_control(PKG);
        assert_eq!(
            control.validate(PKG, HELIOS_NATIVE_RENDER_CAPSET),
            Ok(Hvc1ContextClass::Control)
        );
        let queue = HeliosVulkanContextV1::new_queue(PKG, 0, 1);
        assert_eq!(
            queue.validate(PKG, HELIOS_NATIVE_RENDER_CAPSET),
            Ok(Hvc1ContextClass::Queue)
        );
    }

    #[test]
    fn hvc1_refuses_wrong_package_mode_and_half_control() {
        let good = HeliosVulkanContextV1::new_control(PKG);
        assert_eq!(
            good.validate(PKG + 1, HELIOS_NATIVE_RENDER_CAPSET),
            Err(Hvc1Reject::PackageGenerationMismatch)
        );
        // A zero admitted generation admits nothing.
        assert_eq!(
            good.validate(0, HELIOS_NATIVE_RENDER_CAPSET),
            Err(Hvc1Reject::PackageGenerationUnset)
        );
        let mut bad_mode = good;
        bad_mode.mode = 1;
        assert_eq!(
            bad_mode.validate(PKG, HELIOS_NATIVE_RENDER_CAPSET),
            Err(Hvc1Reject::ModeUnsupported)
        );
        let mut half = good;
        half.queue_index = 0;
        assert_eq!(
            half.validate(PKG, HELIOS_NATIVE_RENDER_CAPSET),
            Err(Hvc1Reject::QueueOrdinalMixed)
        );
    }

    // ── HNR2 ─────────────────────────────────────────────────────────────────

    fn fragment(
        index: u16,
        count: u16,
        offset: u64,
        bytes: u32,
        total: u64,
    ) -> HeliosNativeRenderV2 {
        let mut flags = 0;
        if index == 0 {
            flags |= HELIOS_HNR2_FLAG_BEGIN;
        }
        if index + 1 == count {
            flags |= HELIOS_HNR2_FLAG_COMMIT;
        }
        HeliosNativeRenderV2 {
            magic: HELIOS_HNR2_MAGIC,
            abi_version: HELIOS_HNR2_ABI_VERSION,
            header_size: HELIOS_HNR2_HEADER_SIZE,
            package_generation: PKG,
            batch_token: 7,
            total_payload_bytes: total,
            fragment_payload_offset: offset,
            fragment_payload_bytes: bytes,
            fragment_index: index,
            fragment_count: count,
            use_record_offset: 0,
            use_record_count: 0,
            patch_record_offset: 0,
            patch_record_count: 0,
            reply_allocation_list_index: HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX,
            flags,
            reply_offset: 0,
            reply_capacity_bytes: 0,
            fragment_crc64: 0,
            full_payload_crc64: 0,
            reply_slot_generation: 0,
        }
    }

    fn expect(open: Option<Hnr2OpenBatch>, allocs: u32, command_length: u32) -> Hnr2Expect {
        Hnr2Expect {
            package_generation: PKG,
            allocation_list_count: allocs,
            command_length,
            // Exactly the advertised minimum, which is what Dxgkrnl returns in
            // the ordinary case.
            command_buffer_bytes: HELIOS_HVC1_DMA_BUFFER_BYTES,
            patch_location_list_in_size: 0,
            open,
            last_batch_token: 0,
        }
    }

    #[test]
    fn hnr2_one_fragment_batch_is_begin_and_commit() {
        let mut header = fragment(0, 1, 0, 256, 256);
        header.use_record_count = 2;
        header.use_record_offset = HELIOS_HNR2_HEADER_SIZE as u32;
        header.patch_record_count = 3;
        header.patch_record_offset = HELIOS_HNR2_HEADER_SIZE as u32 + 2 * 24;
        let command_length = 112 + 2 * 24 + 3 * 16 + 256;
        let accept = header.validate(&expect(None, 2, command_length)).unwrap();
        assert_eq!(accept.class, Hnr2FragmentClass::Complete);
        assert!(accept.class.is_commit() && accept.class.is_begin());
        assert_eq!(accept.next_open, None);
        assert_eq!(accept.layout.payload_offset, 112 + 2 * 24 + 3 * 16);
        assert_eq!(accept.layout.command_bytes, command_length);
    }

    #[test]
    fn hnr2_two_fragment_batch_chains_exactly() {
        let first = fragment(0, 2, 0, 100, 300);
        let accept = first.validate(&expect(None, 0, 112 + 100)).unwrap();
        assert_eq!(accept.class, Hnr2FragmentClass::Begin);
        let open = accept.next_open.unwrap();
        assert_eq!(open.next_fragment_index, 1);
        assert_eq!(open.next_payload_offset, 100);

        // The exact next offset is required; 99 or 101 is a reject.
        let skewed = fragment(1, 2, 101, 199, 300);
        assert_eq!(
            skewed.validate(&expect(Some(open), 0, 112 + 199)),
            Err(Hnr2Reject::FragmentOffsetMismatch)
        );

        let last = fragment(1, 2, 100, 200, 300);
        let accept = last.validate(&expect(Some(open), 0, 112 + 200)).unwrap();
        assert_eq!(accept.class, Hnr2FragmentClass::Commit);
        assert_eq!(accept.next_open, None);
    }

    #[test]
    fn hnr2_refuses_out_of_band_shapes() {
        // BEGIN while a batch is open.
        let open = Hnr2OpenBatch {
            batch_token: 7,
            total_payload_bytes: 300,
            fragment_count: 2,
            next_fragment_index: 1,
            next_payload_offset: 100,
        };
        let begin = fragment(0, 2, 0, 100, 300);
        assert_eq!(
            begin.validate(&expect(Some(open), 0, 112 + 100)),
            Err(Hnr2Reject::BatchAlreadyOpen)
        );
        // Interior fragment with no batch open.
        let interior = fragment(1, 3, 100, 100, 300);
        assert_eq!(
            interior.validate(&expect(None, 0, 112 + 100)),
            Err(Hnr2Reject::NoOpenBatch)
        );
        // Above the 64-fragment bound.
        let too_many = fragment(0, HELIOS_HNR2_MAX_FRAGMENTS + 1, 0, 100, 300);
        assert_eq!(
            too_many.validate(&expect(None, 0, 112 + 100)),
            Err(Hnr2Reject::FragmentCountTooLarge)
        );
        // Above the 15-MiB bound.
        let too_big = fragment(0, 1, 0, 16, HELIOS_HNR2_MAX_PAYLOAD_BYTES + 1);
        assert_eq!(
            too_big.validate(&expect(None, 0, 112 + 16)),
            Err(Hnr2Reject::TotalPayloadTooLarge)
        );
        // Unknown flag bit.
        let mut odd_flags = fragment(0, 1, 0, 16, 16);
        odd_flags.flags |= 8;
        assert_eq!(
            odd_flags.validate(&expect(None, 0, 112 + 16)),
            Err(Hnr2Reject::FlagBitsUnknown)
        );
        // Tables before COMMIT.
        let mut early_tables = fragment(0, 2, 0, 16, 64);
        early_tables.use_record_count = 1;
        early_tables.use_record_offset = 112;
        assert_eq!(
            early_tables.validate(&expect(None, 0, 112 + 24 + 16)),
            Err(Hnr2Reject::UseRecordsBeforeCommit)
        );
        // CommandLength must be exactly what the header describes.
        let exact = fragment(0, 1, 0, 16, 16);
        assert_eq!(
            exact.validate(&expect(None, 0, 112 + 17)),
            Err(Hnr2Reject::CommandLengthMismatch)
        );
    }

    #[test]
    fn hnr2_reply_range_must_sit_inside_one_slot() {
        let mut header = fragment(0, 1, 0, 64, 64);
        header.use_record_count = 1;
        header.use_record_offset = 112;
        header.flags |= HELIOS_HNR2_FLAG_HAS_REPLY;
        header.reply_allocation_list_index = 0;
        header.reply_slot_generation = 9;
        header.reply_capacity_bytes = HELIOS_HVR1_HEADER_SIZE as u64 + 4096;
        header.reply_offset = HELIOS_HVM1_REPLY_SLOT_BYTES; // slot 1 base
        let command_length = 112 + 24 + 64;
        let accept = header.validate(&expect(None, 1, command_length)).unwrap();
        assert!(accept.has_reply);
        assert_eq!(header.granted_reply_chunk_bytes(), Some(4096));

        // Straddling the slot boundary is refused.
        let mut straddle = header;
        straddle.reply_offset = 2 * HELIOS_HVM1_REPLY_SLOT_BYTES - 8;
        assert_eq!(
            straddle.validate(&expect(None, 1, command_length)),
            Err(Hnr2Reject::ReplyRangeCrossesSlot)
        );
        // Past the fourth slot is refused.
        let mut past_end = header;
        past_end.reply_offset = HELIOS_HVM1_REPLY_POOL_BYTES;
        assert_eq!(
            past_end.validate(&expect(None, 1, command_length)),
            Err(Hnr2Reject::ReplySlotIndexOutOfRange)
        );
        // Above one complete reply slot is refused.
        let mut too_big = header;
        too_big.reply_capacity_bytes =
            HELIOS_HVR1_HEADER_SIZE as u64 + HELIOS_HVR1_MAX_CHUNK_BYTES + 1;
        assert_eq!(
            too_big.validate(&expect(None, 1, command_length)),
            Err(Hnr2Reject::ReplyCapacityTooLarge)
        );
        // HAS_REPLY on a non-COMMIT fragment is refused.
        let mut early = fragment(0, 2, 0, 64, 128);
        early.flags |= HELIOS_HNR2_FLAG_HAS_REPLY;
        early.reply_allocation_list_index = 0;
        early.reply_slot_generation = 9;
        early.reply_capacity_bytes = HELIOS_HVR1_HEADER_SIZE as u64;
        assert_eq!(
            early.validate(&expect(None, 0, 112 + 64)),
            Err(Hnr2Reject::ReplyFlagOutsideCommit)
        );
    }

    fn use_record(
        index: u32,
        access: u32,
        first_patch: u32,
        patch_count: u32,
    ) -> HeliosNativeRenderUse {
        HeliosNativeRenderUse {
            allocation_list_index: index,
            access_flags: access,
            expected_allocation_generation: 0x5A5A,
            first_patch,
            patch_count,
        }
    }

    fn patch_record(offset: u32, index: u32) -> HeliosNativeRenderPatch {
        HeliosNativeRenderPatch {
            payload_offset: offset,
            allocation_list_index: index,
            operand_kind: HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32,
            encoded_width: HELIOS_HNR2_OPERAND_WIDTH_32,
            reserved: 0,
        }
    }

    #[test]
    fn hnr2_tables_tile_exactly_once() {
        let mut header = fragment(0, 1, 0, 256, 256);
        header.use_record_count = 2;
        header.use_record_offset = 112;
        header.patch_record_count = 3;
        header.patch_record_offset = 112 + 48;
        let uses = [
            use_record(0, HELIOS_HNR2_ACCESS_READ, 0, 2),
            use_record(1, HELIOS_HNR2_ACCESS_WRITE, 2, 1),
        ];
        let patches = [patch_record(0, 0), patch_record(8, 0), patch_record(16, 1)];
        assert_eq!(validate_commit_tables(&header, &uses, &patches, 2), Ok(()));

        // The same allocation twice is refused.
        let dup = [
            use_record(0, HELIOS_HNR2_ACCESS_READ, 0, 2),
            use_record(0, HELIOS_HNR2_ACCESS_WRITE, 2, 1),
        ];
        assert_eq!(
            validate_commit_tables(&header, &dup, &patches, 2),
            Err(Hnr2TableReject::AllocationUsedTwice)
        );

        // A patch nobody owns is refused.
        let short = [
            use_record(0, HELIOS_HNR2_ACCESS_READ, 0, 2),
            use_record(1, HELIOS_HNR2_ACCESS_WRITE, 2, 0),
        ];
        assert_eq!(
            validate_commit_tables(&header, &short, &patches, 2),
            Err(Hnr2TableReject::PatchRunLeavesGap)
        );

        // A patch that names another use's allocation is refused.
        let crossed = [patch_record(0, 0), patch_record(8, 1), patch_record(16, 1)];
        assert_eq!(
            validate_commit_tables(&header, &uses, &crossed, 2),
            Err(Hnr2TableReject::PatchAllocationMismatch)
        );

        // Misaligned and out-of-payload operands are refused.
        let misaligned = [patch_record(1, 0), patch_record(8, 0), patch_record(16, 1)];
        assert_eq!(
            validate_commit_tables(&header, &uses, &misaligned, 2),
            Err(Hnr2TableReject::PatchOffsetMisaligned)
        );
        let outside = [patch_record(0, 0), patch_record(8, 0), patch_record(256, 1)];
        assert_eq!(
            validate_commit_tables(&header, &uses, &outside, 2),
            Err(Hnr2TableReject::PatchOffsetOutOfPayload)
        );

        // An unknown operand kind is refused rather than patched blindly.
        let mut unknown = patches;
        unknown[0].operand_kind = 77;
        assert_eq!(
            validate_commit_tables(&header, &uses, &unknown, 2),
            Err(Hnr2TableReject::PatchOperandKindUnknown)
        );
    }

    /// Section 10.7's reply row is "valid **writable** index with `HAS_REPLY`",
    /// and the reply slot is the sole legal write of the pure-control class. The
    /// header cannot check writability — it lives in the use table.
    #[test]
    fn hnr2_reply_target_must_be_a_writable_use() {
        let mut header = fragment(0, 1, 0, 256, 256);
        header.use_record_count = 2;
        header.use_record_offset = 112;
        header.patch_record_count = 3;
        header.patch_record_offset = 112 + 48;
        header.flags |= HELIOS_HNR2_FLAG_HAS_REPLY;
        header.reply_allocation_list_index = 1;
        let patches = [patch_record(0, 0), patch_record(8, 0), patch_record(16, 1)];

        // Entry 1 is WRITE: admitted.
        let writable = [
            use_record(0, HELIOS_HNR2_ACCESS_READ, 0, 2),
            use_record(1, HELIOS_HNR2_ACCESS_WRITE, 2, 1),
        ];
        assert_eq!(
            validate_commit_tables(&header, &writable, &patches, 2),
            Ok(())
        );

        // The same batch aiming its reply at a read-only allocation.
        let read_only = [
            use_record(0, HELIOS_HNR2_ACCESS_READ, 0, 2),
            use_record(1, HELIOS_HNR2_ACCESS_READ, 2, 1),
        ];
        assert_eq!(
            validate_commit_tables(&header, &read_only, &patches, 2),
            Err(Hnr2TableReject::ReplyIndexNotWritable)
        );

        // A reply index no use record claims.
        let mut orphan = header;
        orphan.reply_allocation_list_index = 5;
        assert_eq!(
            validate_commit_tables(&orphan, &writable, &patches, 2),
            Err(Hnr2TableReject::ReplyIndexHasNoUseRecord)
        );
    }

    /// Section 10.7 line 1821 and line 1826: the two Render entry-gate rules
    /// that are about the OS's buffers rather than the header's own bytes.
    #[test]
    fn hnr2_render_entry_gate_bounds_the_os_buffers() {
        let header = fragment(0, 1, 0, 256, 256);
        let command_length = 112 + 256;

        // An incoming WDDM patch-location list is a different protocol.
        let mut patched = expect(None, 0, command_length);
        patched.patch_location_list_in_size = 1;
        assert_eq!(
            header.validate(&patched),
            Err(Hnr2Reject::PatchLocationListInNotEmpty)
        );

        // A command buffer below the advertised minimum means context creation
        // admitted something it was required to refuse.
        let mut small_buffer = expect(None, 0, command_length);
        small_buffer.command_buffer_bytes = HELIOS_HVC1_DMA_BUFFER_BYTES - 1;
        assert_eq!(
            header.validate(&small_buffer),
            Err(Hnr2Reject::CommandBufferBelowAdvertisedMinimum)
        );

        // `CommandLength` above the buffer Dxgkrnl actually returned.
        let big = fragment(0, 1, 0, 300_000, 300_000);
        let mut over = expect(None, 0, 112 + 300_000);
        over.command_buffer_bytes = HELIOS_HVC1_DMA_BUFFER_BYTES;
        assert_eq!(
            big.validate(&over),
            Err(Hnr2Reject::CommandLengthAboveDmaBuffer)
        );
    }

    /// HVC1's create-context private data is exact-length checked in one shared
    /// place, and the context-flag gate is a constant rather than prose.
    #[test]
    fn hvc1_private_data_and_context_flags_are_gated() {
        let control = HeliosVulkanContextV1::new_control(PKG);
        let bytes = bytemuck::bytes_of(&control);
        assert_eq!(
            parse_hvc1_private_data(bytes)
                .unwrap()
                .validate(PKG, HELIOS_NATIVE_RENDER_CAPSET),
            Ok(Hvc1ContextClass::Control)
        );
        // `HeliosVulkanContextV1` deliberately derives no `PartialEq` (no wire
        // record in this module does), so the refusals are matched by shape.
        assert!(matches!(
            parse_hvc1_private_data(&bytes[..31]),
            Err(Hvc1Reject::PrivateDataSize)
        ));
        assert!(matches!(
            parse_hvc1_private_data(&[]),
            Err(Hvc1Reject::PrivateDataSize)
        ));

        assert_eq!(hvc1_admit_context_flags(0), Ok(()));
        // Any bit at all — `VirtualAddressing` included — is unsupported.
        assert_eq!(
            hvc1_admit_context_flags(1),
            Err(Hvc1Reject::ContextFlagsUnsupported)
        );
    }

    /// Section 10.7's two native-side pools are bounded by mechanisms, not by
    /// constants a caller is trusted to compare against, and neither ever waits
    /// or spills inside this crate.
    #[test]
    fn native_pools_refuse_at_their_caps_and_name_them() {
        assert_eq!(admit_render_slot(0, 0, 4096), Ok(()));
        assert_eq!(
            admit_render_slot(HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS, 0, 4096),
            Err(Hnr2CapacityRefusal {
                limit: Hnr2CapacityLimit::OutstandingSubmissions,
                requested: HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS as u64 + 1,
                capacity: HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS as u64,
            })
        );
        assert_eq!(
            admit_render_slot(0, HELIOS_HNR2_SLOT_POOL_BYTES, 1),
            Err(Hnr2CapacityRefusal {
                limit: Hnr2CapacityLimit::SlotPoolBytes,
                requested: HELIOS_HNR2_SLOT_POOL_BYTES + 1,
                capacity: HELIOS_HNR2_SLOT_POOL_BYTES,
            })
        );
        // A running total that would wrap must refuse, never admit.
        assert!(admit_render_slot(0, u64::MAX, 2).is_err());

        assert_eq!(admit_snapshot(0, 0, 1024), Ok(()));
        assert_eq!(
            admit_snapshot(0, 0, HELIOS_HVR1_MAX_SNAPSHOT_BYTES + 1),
            Err(Hnr2CapacityRefusal {
                limit: Hnr2CapacityLimit::SnapshotBytes,
                requested: HELIOS_HVR1_MAX_SNAPSHOT_BYTES + 1,
                capacity: HELIOS_HVR1_MAX_SNAPSHOT_BYTES,
            })
        );
        assert_eq!(
            admit_snapshot(HELIOS_HVR1_MAX_LIVE_SNAPSHOTS, 0, 1024),
            Err(Hnr2CapacityRefusal {
                limit: Hnr2CapacityLimit::LiveSnapshots,
                requested: HELIOS_HVR1_MAX_LIVE_SNAPSHOTS as u64 + 1,
                capacity: HELIOS_HVR1_MAX_LIVE_SNAPSHOTS as u64,
            })
        );
        assert_eq!(
            admit_snapshot(1, HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES, 1),
            Err(Hnr2CapacityRefusal {
                limit: Hnr2CapacityLimit::LiveSnapshotBytes,
                requested: HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES + 1,
                capacity: HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES,
            })
        );
    }

    #[test]
    fn hnr2_write_operation_must_match_the_allocation_list() {
        let read = use_record(0, HELIOS_HNR2_ACCESS_READ, 0, 0);
        assert_eq!(validate_use_write_operation(&read, false), Ok(()));
        assert_eq!(
            validate_use_write_operation(&read, true),
            Err(Hnr2TableReject::WriteOperationMismatch)
        );
        let write = use_record(0, HELIOS_HNR2_ACCESS_WRITE, 0, 0);
        assert_eq!(validate_use_write_operation(&write, true), Ok(()));
    }

    // ── HVM1 ─────────────────────────────────────────────────────────────────

    #[test]
    fn hvm1_reply_pool_is_exactly_four_mib() {
        let pool = HeliosVenusMemoryAllocationV1::new_reply_pool(PKG);
        assert_eq!(
            pool.validate(PKG, Hvm1Stage::CreateInput),
            Ok(Hvm1Role::ReplyPool)
        );
        let mut wrong_size = pool;
        wrong_size.byte_size = HELIOS_HVM1_REPLY_POOL_BYTES / 2;
        assert_eq!(
            wrong_size.validate(PKG, Hvm1Stage::CreateInput),
            Err(Hvm1Reject::ByteSizeNotExactForRole)
        );
    }

    #[test]
    fn hvm1_device_local_has_no_cpu_access_and_no_lock() {
        let local = HeliosVenusMemoryAllocationV1::new(
            PKG,
            Hvm1Role::VulkanDeviceLocal,
            4096,
            HELIOS_HVM1_ACCESS_HOST_READ | HELIOS_HVM1_ACCESS_HOST_WRITE,
        );
        assert_eq!(
            local.validate(PKG, Hvm1Stage::CreateInput),
            Ok(Hvm1Role::VulkanDeviceLocal)
        );
        assert!(!Hvm1Role::VulkanDeviceLocal.placement().lockable);
        assert!(!Hvm1Role::VulkanDeviceLocal.placement().cpu_visible);
        assert_eq!(
            Hvm1Role::VulkanDeviceLocal.placement().cache_policy,
            HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE
        );

        let mut cpu_mapped = local;
        cpu_mapped.access |= HELIOS_HVM1_ACCESS_CPU_WRITE;
        assert_eq!(
            cpu_mapped.validate(PKG, Hvm1Stage::CreateInput),
            Err(Hvm1Reject::AccessNotRoleCompatible)
        );
        // Role 4 must declare NOT_CPU_VISIBLE, never write-combined.
        let mut wc = local;
        wc.cache_policy = HELIOS_HVM1_CACHE_WRITE_COMBINED;
        assert_eq!(
            wc.validate(PKG, Hvm1Stage::CreateInput),
            Err(Hvm1Reject::CachePolicyNotRoleExact)
        );
    }

    /// §10.7's HVM1 arrives on the same `(pPrivateDriverData,
    /// PrivateDriverDataSize)` pair as HWA2 and HOC1, and the KMD reads it in
    /// kernel mode: the length must be a gate taken *before* the read, or a
    /// short user buffer is an out-of-bounds kernel read that no field
    /// validation afterwards can undo. `HeliosVenusMemoryAllocationV1` does not
    /// derive `PartialEq` (unlike its two siblings), so the round-trip is
    /// asserted on the bytes.
    #[test]
    fn hvm1_is_read_through_a_bounded_reader() {
        let pool = HeliosVenusMemoryAllocationV1::new_reply_pool(PKG);
        let bytes = bytemuck::bytes_of(&pool);
        let parsed = HeliosVenusMemoryAllocationV1::from_private_data(bytes)
            .expect("an exactly-64-byte buffer must parse");
        assert_eq!(bytemuck::bytes_of(&parsed), bytes);

        // Short: the whole point of the gate.
        assert_eq!(
            HeliosVenusMemoryAllocationV1::from_private_data(&bytes[..63]).unwrap_err(),
            Hvm1Reject::PrivateDataSizeMismatch
        );
        // Empty, and one byte too long: both are "not exactly 64".
        assert_eq!(
            HeliosVenusMemoryAllocationV1::from_private_data(&[]).unwrap_err(),
            Hvm1Reject::PrivateDataSizeMismatch
        );
        let mut over = [0u8; 65];
        over[..64].copy_from_slice(bytes);
        assert_eq!(
            HeliosVenusMemoryAllocationV1::from_private_data(&over).unwrap_err(),
            Hvm1Reject::PrivateDataSizeMismatch
        );

        // A correct-length buffer at an ODD address must parse: the record has
        // `u64` fields and alignment 8, the runtime promises neither, and the
        // read is unaligned on purpose. It must not be refused with a *length*
        // error that names the correct length.
        let mut staging = [0u8; 72];
        staging[1..65].copy_from_slice(bytes);
        let parsed = HeliosVenusMemoryAllocationV1::from_private_data(&staging[1..65])
            .expect("an unaligned exactly-64-byte buffer must parse");
        assert_eq!(bytemuck::bytes_of(&parsed), bytes);
        assert_eq!(
            parsed.validate(PKG, Hvm1Stage::CreateInput),
            Ok(Hvm1Role::ReplyPool)
        );

        // The reader is not a validator: a 64-byte buffer of garbage parses and
        // is then refused by name, which is what keeps the two jobs separate.
        assert_eq!(
            HeliosVenusMemoryAllocationV1::from_private_data(&[0u8; 64])
                .expect("length is exact")
                .validate(PKG, Hvm1Stage::CreateInput),
            Err(Hvm1Reject::MagicMismatch)
        );

        // The appended code continues the stable 0x04xx block.
        assert_eq!(Hvm1Reject::PrivateDataSizeMismatch.code(), 0x0412);
    }

    #[test]
    fn hvm1_write_back_fields_are_stage_exact() {
        let host_visible = HeliosVenusMemoryAllocationV1::new(
            PKG,
            Hvm1Role::VulkanHostVisible,
            65536,
            HELIOS_HVM1_ACCESS_CPU_READ
                | HELIOS_HVM1_ACCESS_CPU_WRITE
                | HELIOS_HVM1_ACCESS_HOST_READ,
        );
        // Input: all three write-back fields zero.
        assert!(host_visible.validate(PKG, Hvm1Stage::CreateInput).is_ok());
        assert_eq!(
            host_visible.validate(PKG, Hvm1Stage::CreateOutput),
            Err(Hvm1Reject::ObjectGenerationZeroOnOutput)
        );

        let mut returned = host_visible;
        returned.object_generation = 42;
        returned.segment_page_shift = HELIOS_HVM1_SEGMENT_PAGE_SHIFT;
        returned.allocation_alignment = 4096;
        assert_eq!(
            returned.validate(PKG, Hvm1Stage::CreateOutput),
            Ok(Hvm1Role::VulkanHostVisible)
        );
        assert_eq!(
            returned.validate(PKG, Hvm1Stage::CreateInput),
            Err(Hvm1Reject::WriteBackFieldNotZeroOnInput)
        );

        let mut odd_shift = returned;
        odd_shift.segment_page_shift = 16;
        assert_eq!(
            odd_shift.validate(PKG, Hvm1Stage::CreateOutput),
            Err(Hvm1Reject::SegmentPageShiftUnexpected)
        );
        let mut odd_align = returned;
        odd_align.allocation_alignment = 4095;
        assert_eq!(
            odd_align.validate(PKG, Hvm1Stage::CreateOutput),
            Err(Hvm1Reject::AllocationAlignmentInvalid)
        );
    }

    // ── HVR1 ─────────────────────────────────────────────────────────────────

    fn reply(snapshot: u64, total: u64, offset: u64, bytes: u32, flags: u32) -> HeliosVenusReplyV1 {
        HeliosVenusReplyV1 {
            magic: HELIOS_HVR1_MAGIC,
            version: HELIOS_HVR1_VERSION,
            header_size: HELIOS_HVR1_HEADER_SIZE,
            package_generation: PKG,
            session_generation: 11,
            slot_generation: 9,
            batch_token: 7,
            snapshot_generation: snapshot,
            opcode: 0x1234,
            status: 0,
            total_bytes: total,
            chunk_offset: offset,
            chunk_bytes: bytes,
            flags,
        }
    }

    fn reply_expect(snapshot: Option<u64>, total: Option<u64>, offset: u64) -> Hvr1Expect {
        Hvr1Expect {
            package_generation: PKG,
            session_generation: 11,
            slot_generation: 9,
            batch_token: 7,
            snapshot_generation: snapshot,
            total_bytes: total,
            expected_chunk_offset: offset,
            granted_chunk_bytes: 4096,
        }
    }

    #[test]
    fn hvr1_continuation_chain_is_exact() {
        let first = reply(0x77, 6000, 0, 4096, HELIOS_HVR1_FLAG_MORE);
        let accept = first.validate(&reply_expect(None, None, 0)).unwrap();
        assert!(!accept.complete);
        assert_eq!(accept.next_offset, 4096);

        let request =
            HeliosVenusReplyContinuationV1::next(&accept, PKG, 11, 4096).expect("more remains");
        assert_eq!(request.expected_offset, 4096);
        assert_eq!(request.snapshot_generation, 0x77);
        assert_eq!(request.validate(PKG, 11, 0x77, 6000, 4096), Ok(()));
        // A request for a byte range the snapshot already finished is refused.
        assert_eq!(
            request.validate(PKG, 11, 0x77, 4096, 4096),
            Err(Hvr1ContinuationReject::ExpectedOffsetPastEnd)
        );

        let last = reply(0x77, 6000, 4096, 1904, HELIOS_HVR1_FLAG_FINAL);
        let accept = last
            .validate(&reply_expect(Some(0x77), Some(6000), 4096))
            .unwrap();
        assert!(accept.complete);
        assert_eq!(accept.next_offset, 6000);
        assert_eq!(
            HeliosVenusReplyContinuationV1::next(&accept, PKG, 11, 4096),
            None
        );
    }

    #[test]
    fn hvr1_refuses_prefix_as_success_and_unbounded_results() {
        // FINAL that stops short of total_bytes: a prefix is never success.
        let short = reply(0x77, 6000, 0, 4096, HELIOS_HVR1_FLAG_FINAL);
        assert_eq!(
            short.validate(&reply_expect(None, None, 0)),
            Err(Hvr1Reject::FinalBeforeSnapshotEnd)
        );
        // MORE with no bytes would never terminate.
        let empty = reply(0x77, 6000, 0, 0, HELIOS_HVR1_FLAG_MORE);
        assert_eq!(
            empty.validate(&reply_expect(None, None, 0)),
            Err(Hvr1Reject::EmptyContinuation)
        );
        // Both flags, or neither, is a reject.
        let both = reply(
            0x77,
            16,
            0,
            16,
            HELIOS_HVR1_FLAG_MORE | HELIOS_HVR1_FLAG_FINAL,
        );
        assert_eq!(
            both.validate(&reply_expect(None, None, 0)),
            Err(Hvr1Reject::FlagsNotExactlyOne)
        );
        // Above the 64-MiB snapshot bound there is no growth path.
        let huge = reply(
            0x77,
            HELIOS_HVR1_MAX_SNAPSHOT_BYTES + 1,
            0,
            16,
            HELIOS_HVR1_FLAG_MORE,
        );
        assert_eq!(
            huge.validate(&reply_expect(None, None, 0)),
            Err(Hvr1Reject::TotalBytesTooLarge)
        );
        // More than the transaction granted is a reject, not a resize.
        let over = reply(0x77, 65536, 0, 8192, HELIOS_HVR1_FLAG_MORE);
        assert_eq!(
            over.validate(&reply_expect(None, None, 0)),
            Err(Hvr1Reject::ChunkBytesExceedCapacity)
        );
        // A stale slot generation is never decoded.
        let mut stale = reply(0x77, 16, 0, 16, HELIOS_HVR1_FLAG_FINAL);
        stale.slot_generation = 8;
        assert_eq!(
            stale.validate(&reply_expect(None, None, 0)),
            Err(Hvr1Reject::SlotGenerationMismatch)
        );
    }

    // ── kernel DMA ───────────────────────────────────────────────────────────

    #[test]
    fn one_output_patch_per_use() {
        use kernel_dma::{validate_output_patch_count, Hnr2DmaReject};
        assert_eq!(validate_output_patch_count(4, 4), Ok(()));
        assert_eq!(
            validate_output_patch_count(4, 5),
            Err(Hnr2DmaReject::OutputPatchCountNotUseCount)
        );
        assert_eq!(
            validate_output_patch_count(HELIOS_HNR2_MAX_USE_RECORDS + 1, 4097),
            Err(Hnr2DmaReject::OutputPatchCountTooLarge)
        );
    }

    #[test]
    fn capability_is_unplaced_until_patch_and_stale_at_submit() {
        use kernel_dma::{Hnr2CapabilityExpect, Hnr2DmaReject, Hnr2PhysicalCapability};

        let unplaced = Hnr2PhysicalCapability {
            allocation_generation: 5,
            segment_id: 0,
            access_flags: HELIOS_HNR2_ACCESS_READ,
            physical_address: 0,
            allocation_offset: 0,
            byte_length: 4096,
            hpm_epoch: 0,
        };
        assert_eq!(unplaced.validate_at_render(4096), Ok(()));
        assert!(!unplaced.is_placed());

        let expect = Hnr2CapabilityExpect {
            allocation_generation: 5,
            segment_id: HELIOS_SEGMENT_ID_HLM1,
            hpm_epoch: 3,
            allocation_bytes: 4096,
            page_bytes: 4096,
        };
        assert_eq!(
            unplaced.validate_at_submit(&expect),
            Err(Hnr2DmaReject::CapabilityUnplacedAtSubmit)
        );

        let mut patched = unplaced;
        patched.segment_id = HELIOS_SEGMENT_ID_HLM1;
        patched.physical_address = 0x1_0000_0000;
        patched.hpm_epoch = 3;
        assert_eq!(patched.validate_at_submit(&expect), Ok(()));

        let mut stale_epoch = patched;
        stale_epoch.hpm_epoch = 2;
        assert_eq!(
            stale_epoch.validate_at_submit(&expect),
            Err(Hnr2DmaReject::PlacementEpochStale)
        );
        let mut stale_gen = patched;
        stale_gen.allocation_generation = 4;
        assert_eq!(
            stale_gen.validate_at_submit(&expect),
            Err(Hnr2DmaReject::AllocationGenerationStale)
        );
    }

    /// `protocol/include/helios_native_render.h` hand-copies every constant
    /// below. Pin the exact literals here so a change on the Rust side without
    /// the matching header edit is caught by a failing test that names the
    /// header, not by a live VM mis-parsing a Render fragment or a reply slot.
    /// (Offsets and sizes need no test: both sides assert them at compile time.)
    ///
    /// ⛔ The header deliberately mirrors NOTHING from [`kernel_dma`]:
    /// `Hnr2PhysicalCapability` carries a physical address, a segment ID and a
    /// placement epoch, and §17.1 states the record "exists only in
    /// scheduler DMA and is never returned to user mode". So no constant of that
    /// module is pinned here either — an assertion that a Mesa-facing header
    /// carries a kernel-internal value would be asserting the wrong thing.
    ///
    /// [`HELIOS_NATIVE_RENDER_CAPSET`] is aliased from
    /// [`crate::virtio_gpu::VIRTIO_GPU_CAPSET_VENUS`] on this side but must be a
    /// literal in C (there is no C mirror of `virtio_gpu.rs`), so it is pinned to
    /// the literal — asserting alias-equals-alias would prove nothing.
    #[test]
    fn c_mirror_carries_these_exact_constants() {
        // protocol/include/helios_native_render.h
        assert_eq!(crate::HELIOS_PACKAGE_GENERATION, 0x4845_4C49_0000_0003);
        assert_eq!(HELIOS_NATIVE_RENDER_CAPSET, 4);

        assert_eq!(HELIOS_HVC1_MAGIC, 0x3143_5648);
        assert_eq!(HELIOS_HVC1_ABI_VERSION, 1);
        assert_eq!(HELIOS_HVC1_SIZE, 32);
        assert_eq!(HELIOS_HVC1_MODE_FINITE_HNR2_RENDER, 2);
        assert_eq!(HELIOS_HVC1_CONTROL_ORDINAL, 0xFFFF_FFFF);
        assert_eq!(HELIOS_HVC1_NODE_ORDINAL, 0);
        assert_eq!(HELIOS_HVC1_ENGINE_AFFINITY, 0);
        assert_eq!(HELIOS_HVC1_FENCE_ENGINE_AFFINITY, 1);
        assert_eq!(HELIOS_HVC1_DMA_BUFFER_BYTES, 262_144);
        assert_eq!(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES, 4096);
        assert_eq!(HELIOS_HVC1_PATCH_LOCATION_ENTRIES, 4096);
        assert_eq!(HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES, 64);
        assert_eq!(HELIOS_HVC1_DMA_BUFFER_SEGMENT_SET, 0);
        assert_eq!(HELIOS_HVC1_CONTROL_RING_INDEX, 0);
        assert_eq!(HELIOS_HVC1_CREATE_CONTEXT_FLAGS, 0);

        assert_eq!(HELIOS_HNR2_MAGIC, 0x3252_4E48);
        assert_eq!(HELIOS_HNR2_ABI_VERSION, 2);
        assert_eq!(HELIOS_HNR2_HEADER_SIZE, 112);
        assert_eq!(HELIOS_HNR2_MAX_PAYLOAD_BYTES, 15_728_640);
        assert_eq!(HELIOS_HNR2_MAX_FRAGMENTS, 64);
        assert_eq!(HELIOS_HNR2_MAX_USE_RECORDS, 4096);
        assert_eq!(HELIOS_HNR2_MAX_PATCH_RECORDS, 8192);
        assert_eq!(HELIOS_HNR2_USE_RECORD_SIZE, 24);
        assert_eq!(HELIOS_HNR2_PATCH_RECORD_SIZE, 16);
        assert_eq!(HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS, 64);
        assert_eq!(HELIOS_HNR2_SLOT_POOL_BYTES, 15_728_640);
        assert_eq!(HELIOS_HNR2_FLAG_BEGIN, 1);
        assert_eq!(HELIOS_HNR2_FLAG_COMMIT, 2);
        assert_eq!(HELIOS_HNR2_FLAG_HAS_REPLY, 4);
        assert_eq!(HELIOS_HNR2_FLAG_MASK, 7);
        assert_eq!(HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX, 0xFFFF_FFFF);
        assert_eq!(HELIOS_HNR2_REPLY_OFFSET_ALIGN, 8);
        assert_eq!(HELIOS_HNR2_ACCESS_READ, 1);
        assert_eq!(HELIOS_HNR2_ACCESS_WRITE, 2);
        assert_eq!(HELIOS_HNR2_ACCESS_MASK, 3);
        assert_eq!(HELIOS_HNR2_OPERAND_KIND_INVALID, 0);
        assert_eq!(HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32, 1);
        assert_eq!(HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64, 2);
        assert_eq!(HELIOS_HNR2_OPERAND_WIDTH_32, 4);
        assert_eq!(HELIOS_HNR2_OPERAND_WIDTH_64, 8);
        assert_eq!(HELIOS_HNR2_OPERAND_ALIGN, 4);

        assert_eq!(HELIOS_HVM1_MAGIC, 0x314D_5648);
        assert_eq!(HELIOS_HVM1_ABI_VERSION, 1);
        assert_eq!(HELIOS_HVM1_SIZE, 64);
        assert_eq!(HELIOS_HVM1_ROLE_REPLY_POOL, 1);
        assert_eq!(HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE, 2);
        assert_eq!(HELIOS_HVM1_ROLE_FEEDBACK, 3);
        assert_eq!(HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL, 4);
        assert_eq!(HELIOS_HVM1_ACCESS_CPU_READ, 1);
        assert_eq!(HELIOS_HVM1_ACCESS_CPU_WRITE, 2);
        assert_eq!(HELIOS_HVM1_ACCESS_HOST_READ, 4);
        assert_eq!(HELIOS_HVM1_ACCESS_HOST_WRITE, 8);
        assert_eq!(HELIOS_HVM1_ACCESS_MASK, 15);
        assert_eq!(HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE, 0);
        assert_eq!(HELIOS_HVM1_CACHE_WRITE_COMBINED, 1);
        assert_eq!(HELIOS_HVM1_SEGMENT_PAGE_SHIFT, 12);
        assert_eq!(HELIOS_HVM1_REPLY_POOL_BYTES, 4_194_304);
        assert_eq!(HELIOS_HVM1_REPLY_SLOT_BYTES, 1_048_576);
        assert_eq!(HELIOS_HVM1_REPLY_SLOT_COUNT, 4);

        assert_eq!(HELIOS_HVR1_MAGIC, 0x3152_5648);
        assert_eq!(HELIOS_HVR1_VERSION, 1);
        assert_eq!(HELIOS_HVR1_HEADER_SIZE, 80);
        assert_eq!(HELIOS_HVR1_MAX_SNAPSHOT_BYTES, 67_108_864);
        assert_eq!(HELIOS_HVR1_MAX_LIVE_SNAPSHOTS, 4);
        assert_eq!(HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES, 268_435_456);
        assert_eq!(HELIOS_HVR1_MAX_CHUNK_BYTES, 1_048_496);
        assert_eq!(HELIOS_HNR2_MAX_PAYLOAD_BYTES, 15_728_640);
        assert_eq!(HELIOS_HVR1_FLAG_MORE, 1);
        assert_eq!(HELIOS_HVR1_FLAG_FINAL, 2);
        assert_eq!(HELIOS_HVR1_FLAG_MASK, 3);

        // The 40-byte continuation request has no size constant on either side;
        // both assert the literal at compile time.
        assert_eq!(core::mem::size_of::<HeliosVenusReplyContinuationV1>(), 40);
    }
}
