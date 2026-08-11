//! Allocation management DDIs — the HPS2 retirement's allocation identity model.
//!
//! Normative: `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` and the orchestrator's
//! decision record `docs/retirement/K4-CONTRACT.md`, which wins wherever it
//! completes or contradicts the frozen reference.
//!
//! * §10.3 (`:1014-1189`) — **HWA2**, `HeliosWddmAllocationDescV2`, the
//!   versioned 168-byte create-time descriptor. KMD writes it **only** at
//!   create; every opener treats it as `const`.
//! * §10.7 (`:1946-2011`) — **HVM1**, `HeliosVenusMemoryAllocationV1`, the
//!   64-byte record for one ordinary native-Vulkan `VkDeviceMemory`, admitted
//!   only as one of four exact storage roles.
//! * §10.6 (`:1464-1517`) — **HOC1**, `HeliosOuterCommandAllocationV1`, the
//!   64-byte record for the one D3D12 outer-command pool per device.
//! * §14 (`:3417-3469`) — lifetime/identity/reset. The allocation generation
//!   this file stamps is minted by `crate::adapter::allocation_object`.
//!
//! # The one rule that shapes every path below
//!
//! §10.3:1040-1041: *"The creator allocates the complete buffer, KMD writes it
//! only on create, and every opener treats it as const."* Concretely, and this
//! is the whole point of the retirement's identity model:
//!
//! * `DxgkDdiCreateAllocation` parses the create-**input** record, validates it
//!   in full, creates the backing **itself**, and writes the complete
//!   create-**output** record back into the `[in/out]` per-allocation buffer.
//! * ⛔ `DxgkDdiOpenAllocation` writes **no byte** of any private buffer. The
//!   retired `HeliosWddmOpenIdentity` restamped the first 48 bytes at open time,
//!   so two openers of one allocation could disagree about what they had.
//! * ⛔ There is no UMD-supplied backing. §18.1:4723-4726 makes "no raw `resid`
//!   or host backing token is accepted from a UMD/ICD, batch, or create/open
//!   private descriptor" a static gate; the adoption arm is gone, not disabled.
//!
//! # ⚠ The named gap: HWA2 carries no host resource id (`K4-CONTRACT` §5)
//!
//! The retired open-identity blob carried a `resource_id` at offset 24 and four
//! consumers read it. HWA2 deliberately carries none (the "No host resource
//! token, `resid`, PID, process handle, …" paragraph on
//! `protocol::wddm::HeliosWddmAllocationDescV2` — cite the SYMBOL: that block
//! has moved twice and the `:355-361` this line used to carry now lands inside
//! `HeliosWddmPlaneRecordV2`), and `DXGK_OPENALLOCATIONINFO::hAllocation` is dxgkrnl's runtime
//! token, not this driver's `AllocationContext*` — so **the open path cannot
//! resolve a host resource id at all**, and no adapter-global table may be added
//! to bridge it (§3:373-379, §13.3:3398-3406). The replacement is a different
//! mechanism, not a different field: the ICD stops naming host resources and the
//! KMD patches the host resid in from `HeliosNativeRenderPatch`. That is mesa
//! lane unit **A3** plus K6.
//!
//! ⇒ [`dxgkddi_open_allocation`] therefore publishes **no** [`PresentAllocInfo`]
//! and counts every such open in `OaNoRid`. `present_alloc_info` answers `None`,
//! the Present path refuses at its own gates, and the desktop does not composite
//! until A3 lands. That is the retirement's intended intermediate state, not a
//! regression (`ROADMAP.md` sequencing decision: "the KMD lane goes next,
//! desktop breakage accepted"). Never fabricate a resid to make it look alive.
//!
//! TRUST BOUNDARY: `pPrivateDriverData` is guest-supplied and
//! `PrivateDriverDataSize` is the only authoritative length. Every record is
//! entered through its `from_private_data`, which owns the exact-length check —
//! **never** a pointer cast to the record type. The one raw read below the
//! record types is the 4-byte magic used to pick the arm, and it is bounds
//! checked per-arm, not against a max-union.

use alloc::boxed::Box;
use core::ffi::c_void;
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use bytemuck::bytes_of;
use helios_protocol::{
    helios_hwa2_kind_is_image, helios_hwa2_swizzle_is_direct_flip_capable,
    HeliosOuterCommandAllocationV1, HeliosVenusMemoryAllocationV1, HeliosWddmAllocationDescV2,
    HeliosWddmPlaneRecordV2, Hvm1Role, Hvm1Stage, D3DDDI_ID_UNINITIALIZED, HELIOS_HOC1_BYTES,
    HELIOS_HOC1_MAGIC, HELIOS_HVM1_MAGIC, HELIOS_HVM1_SEGMENT_PAGE_SHIFT, HELIOS_HVM1_SIZE,
    HELIOS_HWA2_BIND_RENDER_TARGET, HELIOS_HWA2_BIND_SHADER_RESOURCE,
    HELIOS_HWA2_BIND_UNORDERED_ACCESS, HELIOS_HWA2_BYTES, HELIOS_HWA2_FLAG_CPU_VISIBLE,
    HELIOS_HWA2_FLAG_CROSS_ADAPTER, HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY,
    HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE, HELIOS_HWA2_FLAG_DISPLAYABLE,
    HELIOS_HWA2_FLAG_PRIMARY, HELIOS_HWA2_FLAG_PROTECTED, HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED,
    HELIOS_HWA2_FLAG_SHARED, HELIOS_HWA2_FLAG_STANDARD, HELIOS_HWA2_FLAG_STEREO,
    HELIOS_HWA2_KIND_BUFFER, HELIOS_HWA2_KIND_IMAGE, HELIOS_HWA2_KIND_PAGING_OBJECT,
    HELIOS_HWA2_KIND_STANDARD_PRIMARY, HELIOS_HWA2_KIND_STANDARD_SHADOW,
    HELIOS_HWA2_KIND_STANDARD_STAGING, HELIOS_HWA2_MAGIC, HELIOS_HWA2_MEMORY_CPU_VISIBLE,
    HELIOS_HWA2_MEMORY_DEVICE_LOCAL, HELIOS_HWA2_MISC_GDI_COMPATIBLE, HELIOS_HWA2_SWIZZLE_LINEAR,
    HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL, HELIOS_PACKAGE_GENERATION, HELIOS_SEGMENT_ID_HLM1,
};

use crate::adapter::allocation_object;
use crate::adapter::{AdapterContext, ScanoutGuard};
use crate::ddi::display::ScanoutReject;
use crate::dxgk::_D3DDDIFORMAT::{D3DDDIFMT_A8B8G8R8, D3DDDIFMT_A8R8G8B8, D3DDDIFMT_X8R8G8B8};
use crate::dxgk::_D3DKMDT_STANDARDALLOCATION_TYPE::{
    D3DKMDT_STANDARDALLOCATION_GDISURFACE, D3DKMDT_STANDARDALLOCATION_SHADOWSURFACE,
    D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE, D3DKMDT_STANDARDALLOCATION_STAGINGSURFACE,
};
use crate::dxgk::*;
use crate::irql::PassiveLevel;
use helios_kmd_logic::snapshot_bind::SnapshotDescriptor;
use helios_kmd_logic::ScanoutFormat;

/// `AllocationContext::magic` — validates `hAllocation` casts in paging DDIs
/// (a garbage dereference in BuildPagingBuffer is a bugcheck).
const ALLOCATION_CTX_MAGIC: u32 = 0x4841_4C43; // "HALC"

/// Sentinel for [`AllocationContext::bar_placed`]: not placed in the BAR segment.
pub(crate) const BAR_UNPLACED: u64 = u64::MAX;

/// Per-allocation KMD state: the venus context + virtio resource backing it, plus
/// the host-visible window mapping (filled in Stage 2b by BuildPagingBuffer).
/// ⛔ `#[repr(C)]` for the reason `ContextContext` carries it: the magic is
/// probed on handles that may not be ours, and Rust's default repr may put a
/// lone `u32` anywhere — including past the end of a smaller foreign object.
#[repr(C)]
struct AllocationContext {
    /// [`ALLOCATION_CTX_MAGIC`] — must be the FIRST field (paging-DDI cast check).
    magic: u32,
    ctx_id: u32,
    /// The host resource id. §10.3:1087-1093 — "a host `resid` may live
    /// **solely** inside the KMD allocation object"; this field is that sole
    /// home. It is never written into any private buffer, protocol message, or
    /// log-visible descriptor, and there is no path from a UMD/ICD value to it.
    resource_id: u32,
    /// The allocation generation minted once at create
    /// (`crate::adapter::allocation_object::mint`), stamped into the descriptor
    /// this create wrote back, and `const` for the object's whole life.
    ///
    /// ⛔ Stale-validation and diagnostics only. Nothing resolves an allocation
    /// *from* it (§10.3:1049, §15:3516-3520); K6's HOB1 use records repeat it as
    /// `expected_allocation_generation` so a stale batch is refused.
    generation: u64,
    /// The exact `HELIOS_HWA2_KIND_*` this allocation was created with, or
    /// [`ALLOC_KIND_HVM1`] / [`ALLOC_KIND_HOC1`] for the two records that are
    /// not HWA2. The classification is recorded rather than re-derived: every
    /// later decision that used to sniff geometry or memory visibility now names
    /// this instead.
    kind: u32,
    /// Nonzero for KMD-backed standard allocations: the kernel venus client's
    /// `VkDeviceMemory` object id behind the blob, freed (`vkFreeMemory`) at
    /// DestroyAllocation after the resource unref.
    venus_memory_id: u64,
    /// Nonzero when the standard allocation's memory is bound to a kernel-created
    /// Venus `VkImage` (the shared-primary scanout path).
    venus_image_id: u64,
    /// Lazily-created kernel-Venus alias of an adopted UMD OPTIMAL image. The
    /// alias imports `resource_id` memory and exists solely so the KMD can copy
    /// the exact SetVidPn primary into its durable LINEAR scanout image.
    scanout_copy_image_id: core::sync::atomic::AtomicU64,
    scanout_copy_memory_id: core::sync::atomic::AtomicU64,
    scanout_copy_conversion_image_id: core::sync::atomic::AtomicU64,
    scanout_copy_conversion_memory_id: core::sync::atomic::AtomicU64,
    scanout_copy_conversion_init_pool_id: core::sync::atomic::AtomicU64,
    scanout_copy_pool_id: core::sync::atomic::AtomicU64,
    scanout_copy_command_buffer_id: core::sync::atomic::AtomicU64,
    scanout_copy_target_image_id: core::sync::atomic::AtomicU64,
    /// DIAGNOSTIC MIRROR ONLY since R609. The authoritative drain fence lives on
    /// the VenusClient that submitted it, where writer and reader are both
    /// inside the venus mutex by construction. This copy is kept because it is
    /// the per-allocation value a dump wants; nothing reads it to decide
    /// anything.
    scanout_copy_last_fence: core::sync::atomic::AtomicU64,
    scanout_copy_owns_source_alias: AtomicU32,
    scanout_copy_orphaned: AtomicU32,
    /// Exact segment-relative address supplied by Windows in
    /// `DXGKARG_SETVIDPNSOURCEADDRESS` for this allocation. Keeping it on the
    /// allocation makes the raised-IRQL callback's deferred handle and address
    /// one identity; the worker never combines an allocation with a global
    /// "latest address" from another flip.
    vidpn_primary_address: AtomicU64,
    /// Exact WDDM segment containing `vidpn_primary_address`, supplied in the
    /// same `DXGKARG_SETVIDPNSOURCEADDRESS` callback.
    vidpn_primary_segment: AtomicU32,
    /// Exact `DXGK_SETVIDPNSOURCEADDRESS_FLAGS::Value` paired with the callback.
    vidpn_primary_flags: AtomicU32,
    /// Presentation epoch this allocation's pending flip published, or
    /// `NO_LEASE` (ROADMAP defect 0ab-B).
    ///
    /// It rides on the ALLOCATION, exactly like `vidpn_primary_address` above
    /// and for exactly the same reason: `pending_vidpn_allocation` is a single
    /// slot that coalesces, so a parallel "latest epoch" atomic would let the
    /// display worker pair one flip's handle with another flip's epoch. Here the
    /// pairing is by construction. Nonzero only on the DMA-buffer flip contract;
    /// the MMIO/`FlipOnVSyncMmIo` desktop path stores 0 and is unchanged.
    vidpn_present_epoch: AtomicU64,
    /// This flip's own frame-completion boundary, taken out of the per-buffer
    /// mark table at flip-arm time, or 0 (ROADMAP defect 0ab-B, D1(i)).
    ///
    /// It rides on the allocation for the same reason the epoch above does, and
    /// it is taken at the FLIP rather than read at the BIND because the table
    /// holds one mark per resource: a bind more than two frame periods after its
    /// present finds the mark already replaced by the same buffer's next
    /// present, and then waits a frame too long. Written by the flip arm only,
    /// on every arm, so a bind can never read a mark from an older flip.
    vidpn_frame_watermark: AtomicU64,
    /// D4b snapshot BIND-TARGET substitution stamped by this allocation's most
    /// recent flip, BY VALUE — never a pointer, never the snapshot's own
    /// `AllocationContext` (the snapshot has no WDDM allocation to resolve).
    /// `vidpn_snap_resid == 0` means no substitution. Rides on the allocation
    /// for the same coalescing reason the epoch/watermark above do, and is
    /// stored on EVERY `set_vidpn_primary_address` (zeroed on the MMIO path)
    /// so a bind can never read a descriptor left by an older flip. Like the
    /// epoch pair, the fields publish under the address's Release store and
    /// are read after its Acquire; a torn read across two concurrent flips of
    /// the same allocation can at worst mix two VALIDATED descriptors, which
    /// the executor's `resource_is_live` arm and the extent gates absorb.
    vidpn_snap_resid: AtomicU32,
    vidpn_snap_width: AtomicU32,
    vidpn_snap_height: AtomicU32,
    vidpn_snap_pitch: AtomicU32,
    vidpn_snap_dxgi_format: AtomicU32,
    /// Validated `<= u32::MAX` at Present; stored narrow like the flip record.
    vidpn_snap_plane_offset: AtomicU32,
    vidpn_snap_alloc_size: AtomicU64,
    size: SIZE_T,
    /// Surface geometry for `DxgkDdiDescribeAllocation` (0 for UMD blob allocations
    /// that carry no dimensions). Populated from the standard-allocation trailer.
    width: u32,
    height: u32,
    format: u32, // D3DDDIFORMAT
    /// Exact D3D11 DDI bind flags supplied by the creator. The KMD Venus
    /// source alias must reproduce the OPTIMAL image's usage contract for
    /// external-memory aliasing on NVIDIA.
    bind_flags: u32,
    /// Row pitch in bytes as the UMD laid the surface out (`cross_adapter_pitch`,
    /// 256-aligned — NOT `width*4`). `SetVidPnSourceAddress`'s `SET_SCANOUT_BLOB`
    /// must use THIS stride so the host reads rows at the right offset (a `width*4`
    /// stride shears the scan-out: 1896×4=7584 vs the real 7680). 0 for allocations
    /// with no geometry trailer.
    pitch: u32,
    /// Exact DXGI format the creator used (`meta.dxgi_format`) — the D3DDDIFORMAT
    /// `format` field above is lossy (both B8G8R8A8 and R8G8B8A8 collapse to
    /// A8R8G8B8), so the scan-out format is resolved from this.
    dxgi_format: u32,
    /// The UMD created this exact `pPrimaryDesc` allocation in the shape
    /// `SET_SCANOUT_BLOB` binds with no intermediate copy, and said so with
    /// `HELIOS_HWA2_FLAG_DISPLAYABLE`. See [`hwa2_is_direct_scanout_primary`],
    /// which is the ONE derivation.
    ///
    /// ⛔ The old wording here — "as a plain LINEAR DMA_BUF" — described the arm
    /// this flag is FALSE for. The direct arm is the UMD's OPAQUE_OPTIMAL export
    /// (`umd/src/forward/alloc.rs` sets `DISPLAYABLE` and
    /// `HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL` together on it), which the QEMU fork
    /// reconstructs natively; the LINEAR primary is the one that gets copied
    /// into the adapter-owned scan-out target.
    direct_scanout: bool,
    /// Byte offset of the plain-LINEAR COLOR plane within the backing allocation
    /// (from the UMD's `vkGetImageSubresourceLayout` on a direct primary).
    /// `SetVidPnSourceAddress`'s `SET_SCANOUT_BLOB` uses it as the plane offset;
    /// 0 for surfaces whose data starts at offset 0.
    plane_offset: u64,
    /// C1 identity: the creator's exact `vkAllocateMemory` size + memory type
    /// (what a cross-process opener must import with). Diagnostic copies — the
    /// authoritative record travels in the private-data trailer / open identity.
    venus_alloc_size: u64,
    memory_type_index: u32,
    /// VidMm-assigned SegmentAddress in the CPU-visible BAR segment, or
    /// [`BAR_UNPLACED`]. Written by `BuildPagingBuffer` when it maps the blob at
    /// the assigned offset; atomic because paging DDIs run concurrently with
    /// allocation DDIs. Only meaningful for `bar_eligible` allocations.
    ///
    /// ⚠ PLACEMENT-CHANGE DETECTOR FOR `PgUn`, not a mapping decision. Nothing
    /// consults it to decide whether to map or unmap: `build_paging_buffer.rs`
    /// compares it against the incoming offset purely to count
    /// `BAR_PT_HARVESTS`, surfaced as the `PgUn` registry value. Deleting it or
    /// re-basing that comparison would silently change what `PgUn` means, which
    /// is why T6/R915 kept it while deleting its write-only neighbours.
    bar_placed: core::sync::atomic::AtomicU64,
    /// K2a: this allocation's contract gives it a CPU view in HLM1, so its blob
    /// must be fixed-mapped at whatever window offset VidMm places it at.
    /// Const — the role and its placement are decided once, at admit.
    hlm1_eligible: bool,
    /// The window offset this allocation's blob is currently fixed-mapped at, or
    /// [`BAR_UNPLACED`].
    ///
    /// ⛔ NOT `bar_placed`. That one is a placement-change detector whose only
    /// consumer is `PgUn`'s count; re-basing it would silently change what `PgUn`
    /// means. This one decides whether a host round-trip is issued.
    hlm1_bound: core::sync::atomic::AtomicU64,
    /// This allocation was reported to VidMm as BAR-segment-only (KMD-backed
    /// standard allocation with a mappable venus blob, BAR segment active).
    bar_eligible: bool,
    /// Provenance of `size`. See [`BackingSize`].
    size_provenance: BackingSize,
}

/// Pseudo-kinds for the two records that are not HWA2, so
/// [`AllocationContext::kind`] is total over every admitted create.
///
/// Chosen above [`helios_protocol::HELIOS_HWA2_KIND_MAX`] and asserted distinct
/// from every real kind below, so a reader can never confuse one with an HWA2
/// value that happens to collide.
pub(crate) const ALLOC_KIND_HVM1: u32 = 0x8000_0001;
/// See [`ALLOC_KIND_HVM1`].
const ALLOC_KIND_HOC1: u32 = 0x8000_0002;

const _: () = {
    assert!(ALLOC_KIND_HVM1 > helios_protocol::HELIOS_HWA2_KIND_MAX);
    assert!(ALLOC_KIND_HOC1 > helios_protocol::HELIOS_HWA2_KIND_MAX);
    assert!(ALLOC_KIND_HVM1 != ALLOC_KIND_HOC1);
};

/// Per-resource KMD state. Dxgkrnl requires a non-null KMD resource handle for
/// `Flags.Resource` CreateAllocation calls (not just per-allocation handles);
/// the handle is opaque to us until DestroyAllocation carries it back.
struct ResourceContext {
    _marker: u32,
}

/// `"HERC"` — the marker that says a `hResource` is one this driver minted.
const RESOURCE_CTX_MARKER: u32 = 0x4845_5243;

/// CreateAllocation calls that arrived with a non-null `hResource` (`RcIn`) and
/// how many of those did not carry our marker (`RcBad`). `RcIn` staying 0 is
/// what makes the "mint only over null" rule a provable no-op.
static RESOURCE_INPUT_HANDLES: AtomicU32 = AtomicU32::new(0);
static RESOURCE_FOREIGN_HANDLES: AtomicU32 = AtomicU32::new(0);

/// OpenAllocation calls carrying more than one entry (`OaMulti`). The call-level
/// OUT fields describe exactly one of them, so this is the population that would
/// have to exist before anything is tuned for multi-surface opens. Every writer
/// in this tree sets NumAllocations = 1.
static MULTI_ENTRY_OPENS: AtomicU32 = AtomicU32::new(0);

/// Ticks the per-CreateAllocation breadcrumb throttle (R317 / k-alloc-05).
static CREATE_BREADCRUMB_TICKS: AtomicU32 = AtomicU32::new(0);

// ── Retirement refusal counters (CLAUDE.md operating rule 2) ────────────────
//
// Every refused create/open below increments exactly one of these and returns a
// documented NTSTATUS. They are plain atomics mirrored through
// `diag::record_named_bytes` on the file's existing 1st-then-every-64th cadence
// ([`bump`]) rather than `diag::record` breadcrumbs, because `record` is
// DiagLevel-gated: a refusal reported only through it leaves no trace on a
// default boot. The cadence matters — every one of these paths is
// guest-reachable, so an unthrottled synchronous `RtlWriteRegistryValue` per
// call is a registry-write storm a caller could trigger deliberately.
//
// ⚠ They are NOT flushed from a central dump site. `ApMiss` and `BlbSzD` are,
// from `cpu_host_aperture.rs:141-151`, and K1 deletes that file — which is
// exactly the trap this shape avoids: an inline flush cannot lose its home.

/// Successful `DxgkDdiCreateAllocation` allocations, all record types (`AcOk`).
/// The denominator every refusal counter below needs.
static CREATE_ADMITTED: AtomicU32 = AtomicU32::new(0);
/// Per-allocation private data that was null, shorter than the 4-byte magic, or
/// carried no magic this driver knows (`AcMagic`). §10.3:1079-1080 — a malformed
/// or unknown descriptor fails the create and **never selects a legacy parser**;
/// the retired `HeliosWddmAllocPrivate` path is one of the things it must not
/// select, which is why an unrecognised magic lands here rather than in a
/// fallback.
static CREATE_UNKNOWN_RECORD: AtomicU32 = AtomicU32::new(0);
/// HWA2 create-input records refused by
/// `HeliosWddmAllocationDescV2::validate_create_input` (`AcHwa2Rej`), including
/// the exact-length arm.
static CREATE_HWA2_REJECT: AtomicU32 = AtomicU32::new(0);
/// HWA2 create-**output** records this KMD built and its own
/// `validate_create_output` then refused (`AcHwa2Out`). **Must read 0** — a
/// nonzero value is a driver bug, not a guest one: the KMD produced a descriptor
/// it would itself reject on open.
static CREATE_HWA2_OUTPUT_REJECT: AtomicU32 = AtomicU32::new(0);
/// HVM1 create-input records refused, packed `(count << 16) | Hvm1Reject::code()`
/// so one registry value names both how often and which field (`AcHvm1Rej`).
static CREATE_HVM1_REJECT: AtomicU32 = AtomicU32::new(0);
/// HVM1 create-output records this KMD built and its own `CreateOutput`
/// validation refused (`AcHvm1Out`). Must read 0; same reasoning as
/// [`CREATE_HWA2_OUTPUT_REJECT`].
static CREATE_HVM1_OUTPUT_REJECT: AtomicU32 = AtomicU32::new(0);
/// HOC1 create-input records refused (`AcHoc1Rej`). §17.6:4419-4420 — "every
/// other size/flag/cache/node/role combination fails allocation".
static CREATE_HOC1_REJECT: AtomicU32 = AtomicU32::new(0);
/// HOC1 create-output records this KMD built and its own `validate_create_output`
/// refused (`AcHoc1Out`). Must read 0.
static CREATE_HOC1_OUTPUT_REJECT: AtomicU32 = AtomicU32::new(0);
/// HVM1 creates refused because the role's `preferred_segment` is not a segment
/// this driver actually reports — ONE COUNTER PER ROLE, `AcSegRole1` (reply
/// pool) … `AcSegRole4` (Vulkan device-local).
///
/// `K4-CONTRACT.md` §4, **as amended 2026-08-10**. The first draft of that clause
/// said "role 4 is admitted and counted, not satisfied", and this file
/// implemented exactly that: it refused `VulkanDeviceLocal` and admitted roles
/// 1-3. Both halves were wrong, and the admitting half is the dangerous one.
/// `Hvm1Role::placement()` (`native_render.rs:1763-1775`) hardcodes
/// `HELIOS_SEGMENT_ID_HLM1` for **every** role, so a role-1..3 create was handed
/// a `PreferredSegment` that `QUERYSEGMENT4` may never have reported — and
/// dxgkrnl then refuses that create **outside this driver, with no Helios
/// counter at all**. That is exactly the invisible refusal CLAUDE.md's "every
/// skipped/refused path gets a named counter" rule exists to prevent, and it is
/// invisible in the way this project has repeatedly been burned by.
///
/// ⇒ The rule is neither "role 4" nor "all roles": the KMD asks the segment
/// table it actually reported ([`segment_is_reported`]) and refuses per role when
/// the answer is no. A hardcoded role number is a claim about K2's schedule
/// embedded in kernel code and goes stale silently; a runtime question about the
/// reported table cannot. `FINDINGS.md` F2 measured that the HLM1 *flag shape* is
/// already admitted by the OS (`BarSegFlags=0x02` starts `OK/CM_PROB_NONE` with a
/// fully composited desktop), so segment id 2 may well be reported before K2
/// lands at all — which is precisely why this is read at runtime instead of
/// assumed in either direction.
///
/// There is no honest substitution to fall back to: §10.7:1999-2002 fixes HLM1 as
/// the preferred segment for every role, and quietly preferring the aperture
/// instead would hand VidMm a placement the doc forbids, on a role-4 object that
/// has `CpuVisible=0` and no CPU VA at all, and would hide the sequencing gap
/// behind a create that looked like success.
///
/// ⚠ Nonzero here is EXPECTED until K2 reports the segment; it measures that gap
/// rather than a fault. Which of the four moves also names which client is
/// asking, which one packed reason code could not: 1 reply pool, 2 host-visible
/// Vulkan, 3 feedback, 4 device-local.
static CREATE_ROLE1_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
static CREATE_ROLE2_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
static CREATE_ROLE3_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
static CREATE_ROLE4_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
/// The HOC1 pool refused by the same check for the same reason (`AcSegHoc1`).
///
/// ⚠ BEYOND THE LETTER of `K4-CONTRACT.md` §4, which is written about HVM1 roles,
/// and recorded here as such. [`hoc1_placement`] hardcodes the identical
/// `HELIOS_SEGMENT_ID_HLM1` (§10.6:1510-1512), so an HOC1 create carries the
/// identical exposure: refused by dxgkrnl, outside this driver, with nothing in
/// the guest naming it. Applying §4's argument to the one other placement in this
/// file that shares its shape is the whole change; the alternative was to
/// knowingly leave one uncounted external refusal sitting beside the one just
/// fixed. Revert by deleting the check in [`admit_hoc1`] — the placement itself
/// is untouched.
static CREATE_HOC1_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
/// Creates refused because the OS call shape contradicts the record
/// (`AcShape`). HVM1 (§10.7:1964-1972) and HOC1 (§10.6:1489-1494) both fix the
/// exact `pfnAllocateCb` shape: `hResource = NULL`, one allocation, outer flags
/// zero, and no resource-level private bytes. §18.1:4750-4751 tests those
/// properties; a shape checked only by the guest is not checked (CLAUDE.md).
static CREATE_CALL_SHAPE: AtomicU32 = AtomicU32::new(0);
/// Creates refused because the KMD has no backing constructor for the descriptor
/// (`AcKind`): an HWA2 `PAGING_OBJECT`, or a geometry/format the kernel venus
/// client cannot build.
static CREATE_UNSUPPORTED_KIND: AtomicU32 = AtomicU32::new(0);
/// Creates refused because the descriptor's KIND and its swizzle/layout class
/// name a surface no producer in this package authors and no kernel venus
/// constructor builds (`AcLayout`): today that is exactly a `STANDARD_SHADOW` or
/// `STANDARD_STAGING` kind in the `OPAQUE_OPTIMAL` class.
///
/// ⚠ **Must read 0.** `dxgkddi_get_standard_allocation_driver_data` is the only
/// author of those two kinds and it gives both `HELIOS_HWA2_SWIZZLE_LINEAR`
/// unconditionally — `is_optimal_gdi_texture` can only be true on the GDI-surface
/// arm, which carries `HELIOS_HWA2_KIND_IMAGE`. A nonzero value here therefore
/// means the two halves of this file have drifted apart, in the direction
/// `StdSelf` cannot see (the record is well-formed; it just describes a surface
/// the KMD has no way to make).
static CREATE_UNSUPPORTED_LAYOUT: AtomicU32 = AtomicU32::new(0);
/// Creates refused because `byte_size` contradicts the KMD's own size rule, or
/// because the backing the KMD created is SMALLER than the descriptor's
/// `byte_size` (`AcSize`).
///
/// The undersize arm is the Xid-31 class: a blob smaller than the image
/// requirement binds "successfully" and then MMU-faults when the sampler reads
/// the slack region (host FAULT_PTE VIRT_READ, killed the IDD feed live
/// 2026-07-04). §10.3:1050 makes `byte_size` bound every plane, so admitting a
/// short backing would make the descriptor lie about its own planes.
static CREATE_SIZE_REJECT: AtomicU32 = AtomicU32::new(0);
/// Creates refused because the host/transport could not produce the backing
/// (`AcBackFail`). Distinct from [`CREATE_UNSUPPORTED_KIND`]: the KMD knew how
/// to ask and the ask failed.
static CREATE_BACKING_FAILED: AtomicU32 = AtomicU32::new(0);
/// Creates refused because the allocation-generation ordinal is exhausted
/// (`AcGenExh`); see `adapter::allocation_object::GENERATION_EXHAUSTED`.
static CREATE_GENERATION_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
/// Opens that could NOT publish a [`PresentAllocInfo`] because HWA2 carries no
/// host resource id (`OaNoRid`) — the A3 gap named in the module doc and in
/// `K4-CONTRACT.md` §5.
///
/// ⚠ This counter is expected to be LARGE and rising until mesa lane unit **A3**
/// and K6 land. It is not a fault; it is the measurement of how much of the
/// present path is waiting on them. It must reach 0 when they do.
///
/// ⛔ A PLAIN `fetch_add`, NOT [`bump`], and the distinction is the point.
/// [`bump`] mirrors on n==1 and every 64th hit, forever, through
/// `diag::record_named_bytes` — a synchronous `RtlWriteRegistryValue`. Every
/// OTHER name in [`RETIREMENT_COUNTER_NAMES`] marks a REFUSAL, which is rare by
/// construction and stays rare; this one fires on every SUCCESSFUL open, i.e. on
/// DWM's own path, for as long as the A3 gap is open. Putting a registry write
/// there would make the throttle's period the only thing between this driver and
/// a per-open kernel registry round-trip on the hot path, and it would make one
/// array mean two different things. It is mirrored instead from [`ALLOC_COUNTERS`]
/// on a PASSIVE create-path cadence — the same shape
/// [`APERTURE_MISSING_CPU_VISIBLE`] uses.
static OPEN_NO_RESOURCE_ID: AtomicU32 = AtomicU32::new(0);
/// Opens whose per-allocation private data failed HWA2 create-**output**
/// validation (`OaHwa2Rej`). §10.3:1079-1080 makes a malformed descriptor fail
/// the OPEN as well as the create; the open is not a place to be lenient,
/// because the receiving UMD reads the identical bytes.
static OPEN_HWA2_REJECT: AtomicU32 = AtomicU32::new(0);
/// HVM1 records stamped with their create-output at OPEN (`OaHvm1Stamp`), and
/// the ones that could not be (`OaHvm1Rej`: a mint failure, or bytes that were
/// neither a valid create-input nor an already-stamped create-output).
///
/// ⛔ MEASURED 2026-08-11, and the reason the stamp is here rather than at
/// create: dxgkrnl DISCARDS a KMD write into `DXGK_ALLOCATIONINFO::
/// pPrivateDriverData` when the buffer came from user mode. `tools/hwa2_writeback_probe.c`
/// on 22.22.266.0 created 7 allocations across both thunks, both record types,
/// bare and resource-associated, and every one came back unstamped in the
/// caller's buffer *and* arrived unstamped at this DDI (5 × `0x0C02_00E6` in the
/// diag ring). `DXGK_OPENALLOCATIONINFO::pPrivateDriverData` is the field the
/// WDK annotates `in/out`; `DXGK_ALLOCATIONINFO::pPrivateDriverData` is `in`.
static OPEN_HVM1_STAMPED: AtomicU32 = AtomicU32::new(0);
static OPEN_HVM1_REJECT: AtomicU32 = AtomicU32::new(0);
/// Presents refused because [`present_alloc_info`] answered `None` — the A3 gap
/// reaching the DISPLAY path (`PrNoRid`).
///
/// ⭐ Added by round 3 of the Phase-2 review, which found the symptom site
/// silent. [`PresentAllocationStorage`] is permanently `None`
/// ([`PresentAllocInfo`]'s doc has the argument), so BOTH of `ddi/display.rs`'s
/// present-side consumers now take their `else` arm on every call — and they
/// did so through the pre-existing last-value breadcrumbs `PBFlip`/`PBCpy =
/// 0xE1`, whose meaning in that file is "dxgkrnl handed us a source handle we
/// could not resolve", i.e. a handle-lifetime bug. An operator reading
/// `PBFlip = 0xE1` after this changeset would have been looking for the wrong
/// defect, and because those are last-value writes rather than counts, could not
/// even tell whether it fired once or per frame.
///
/// So the two sites now write a distinct breadcrumb (`0xEA`, unused in both
/// families) *and* bump this. [`OPEN_NO_RESOURCE_ID`] is the same gap measured
/// one stage earlier, at the open; this is the stage the desktop actually dies
/// at, and the pair localises whether an open ever produced usable state.
///
/// ⛔ Expected LARGE and rising until mesa unit **A3** plus K6 land. Like
/// `OaNoRid` it must be **revisited, not merely zeroed**, when the KMD gains a
/// way to name the real host image.
///
/// ⛔ A PLAIN `fetch_add`, never [`bump`]: `DxgkDdiPresent` is a per-frame path
/// and a registry write there is the producer-side CPU stall this project has
/// already paid for once. Mirrored from [`ALLOC_COUNTERS`] on the create-path
/// cadence.
pub(crate) static PRESENT_NO_ALLOC_INFO: AtomicU32 = AtomicU32::new(0);
/// HWA2 descriptors whose `RESOURCE_ASSOCIATED` claim disagreed with
/// `DXGK_CREATEALLOCATIONFLAGS::Resource` on the call that carried them
/// (`AcRcAssoc`). See the read site for why this is counted and not refused.
///
/// ⚠ Expected NONZERO for OS standard allocations, exactly once per
/// resource-associated standard create. A nonzero value here is a measurement of
/// the field-definition gap, not a fault — but a value that tracks EVERY create
/// rather than the standard ones would mean a UMD is not filling the field at
/// all.
static CREATE_RESOURCE_ASSOC_DIVERGENCE: AtomicU32 = AtomicU32::new(0);
/// Ordinary tiled UMD images backed by a plain venus `VkDeviceMemory` blob
/// instead of a real `VkImage` (`AcOptLin`). A DOWNGRADE that is counted, in the
/// same class as [`LINEAR_BLOB_SIZE_DIVERGENCE`] and
/// [`CREATE_RESOURCE_ASSOC_DIVERGENCE`] — not a refusal, and not a silence.
///
/// The population is `helios_umd12.dll`'s committed textures: `KIND_IMAGE` +
/// `OPAQUE_OPTIMAL` with neither the `STANDARD` flag nor `PRIMARY | DISPLAYABLE`
/// (`umd12/src/forward12/resource12.rs::hwa2_swizzle_class` maps `TL_UNDEFINED`
/// and `TL_64KB_TILE_UNDEFINED_SWIZZLE` onto that class). The KMD's only OPTIMAL
/// constructor is `allocate_optimal_gdi_image_blob`, which builds a
/// 1-mip/1-layer BGRA cross-context present alias and refuses every DXGI format
/// outside {87, 88} — the right image for the two producers named above and the
/// WRONG image for an arbitrary D3D12 resource, whose format, mip chain, array
/// length and sample count it cannot reproduce.
///
/// So the extent is honoured and the tiling is not: `byte_size` on this arm is
/// the engine's exact `VkDeviceMemory` size (`resource12.rs` passes
/// `id.memory_size`), so the blob is neither short (the Xid-31 class) nor a
/// guess. ⛔ Nothing may read this allocation as an image, and nothing does —
/// until mesa unit **A3** and K6 land, the kernel allocation is not the host
/// object vkd3d rendered into at all and `present12` refuses by name
/// (`K4-CONTRACT.md` §5). This counter is the measurement of that gap on the
/// create side, exactly as [`OPEN_NO_RESOURCE_ID`] is on the open side, and it
/// must be revisited — not merely zeroed — when A3 gives the KMD a way to name
/// the real image.
///
/// ⛔ A PLAIN `fetch_add`, NOT [`bump`], for [`OPEN_NO_RESOURCE_ID`]'s reason: it
/// fires on every SUCCESSFUL D3D12 texture create. It is mirrored from
/// [`ALLOC_COUNTERS`].
static CREATE_OPTIMAL_AS_LINEAR: AtomicU32 = AtomicU32::new(0);
/// HVM1 creates refused because the role asks for memory this KMD cannot
/// allocate (`AcHvm1Mem`); the low word is the WIRE role number.
///
/// The only role that can reach it today is 4, `VulkanDeviceLocal`, whose
/// `placement()` publishes `cpu_visible = false` and whose `cache_policy` is
/// `HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE`. This venus client allocates every plain
/// memory blob from the single host-visible/host-coherent memory type chosen at
/// bring-up — a memory blob has no `vkGet*MemoryRequirements` query to feed
/// [`helios_kmd_logic::choose_device_local_memory_type`] — so honouring the role
/// is not currently expressible and substituting host-visible memory would be a
/// silent contradiction of a field the KMD had just validated.
///
/// ⚠ NOT a statement about K2's schedule, and must not be read as one: the
/// segment-reporting question is `segment_is_reported`, checked at runtime per
/// `K4-CONTRACT.md` §4. This is a capability of the venus client, and it is
/// lifted by mesa unit A3 plus K6 giving the KMD a requirements query — at which
/// point this arm is deleted, not widened.
///
/// ⛔ **Must read absent** at HEAD, and absence proves nothing: no component in
/// this package produces an HVM1 record at all, so this refusal — like every
/// other `AcHvm1*` and `AcSegRole*` value — cannot fire yet. See
/// [`admit_hvm1`]'s banner.
static CREATE_HVM1_MEMORY_CLASS_REFUSED: AtomicU32 = AtomicU32::new(0);
/// `DxgkDdiGetStandardAllocationDriverData` calls refused because the runtime's
/// `D3DDDIFORMAT` has no DXGI peer (`StdFmt`).
///
/// BEHAVIOUR CHANGE, recorded here because it is easy to mistake for a
/// regression: the retired trailer wrote `dxgi_format = 0` for such a surface
/// and let the opener fall back to BGRA. HWA2 §10.3:1055 requires an image kind
/// to carry an exact DXGI format and `validate` rejects `UNKNOWN` outright, so
/// the zero hint is no longer expressible. Refusing here is the fail-closed
/// reading; the alternative is authoring a format the KMD guessed.
static STANDARD_FORMAT_REFUSED: AtomicU32 = AtomicU32::new(0);
/// `DxgkDdiGetStandardAllocationDriverData` authored a descriptor its own
/// `validate_create_input` refused (`StdSelf`). **Must read 0** — a nonzero
/// value means this driver produces a record the create path will reject, i.e.
/// the two halves of one file disagree.
static STANDARD_SELF_REJECT: AtomicU32 = AtomicU32::new(0);
static PRIMARY_COPY_ORPHAN_REFUSED: AtomicU32 = AtomicU32::new(0);
static PRIMARY_COPY_ORPHAN_TRANSITION_FAILED: AtomicU32 = AtomicU32::new(0);
static PRIMARY_COPY_ORPHAN_RETAINED: AtomicU32 = AtomicU32::new(0);

/// Every registry name the counters above publish, so the compile-time
/// no-truncation proof below has one list to check.
///
/// `diag::record_named_bytes` clamps silently at `diag::MAX_CONFIG_NAME`, so two
/// names sharing a 14-byte prefix would MERGE into one registry value — a
/// refusal counter reading someone else's number. Same guard
/// `diag::FaultCounter` and `native_fence.rs` use.
const RETIREMENT_COUNTER_NAMES: [&[u8]; 31] = [
    b"AcOk",
    b"AcMagic",
    b"AcHwa2Rej",
    b"AcHwa2Out",
    b"AcHvm1Rej",
    b"AcHvm1Mem",
    b"AcHvm1Out",
    b"AcHoc1Rej",
    b"AcHoc1Out",
    b"AcSegRole1",
    b"AcSegRole2",
    b"AcSegRole3",
    b"AcSegRole4",
    b"AcSegHoc1",
    b"AcShape",
    b"AcKind",
    b"AcLayout",
    b"AcSize",
    b"AcBackFail",
    b"AcGenExh",
    b"AcRcAssoc",
    // The three [`ALLOC_COUNTERS`] names, published by that block rather than by
    // [`bump`] — see its own doc for why. Listed here anyway because this array
    // is the file's ONE truncation proof, and a name that skips it is a name
    // nothing checks. `AcGenEpoch` is 10 bytes; `diag::CounterBlock` has no
    // truncation assert of its own, so this list is the only thing standing
    // between it and a silent merge with another value.
    b"OaNoRid",
    b"PrNoRid",
    b"AcGenEpoch",
    b"AcOptLin",
    b"OaHwa2Rej",
    b"OaHvm1Stamp",
    b"OaHvm1Rej",
    b"CpOrRef",
    b"CpOrFail",
    b"CpOrKeep",
];

const _: () = {
    let mut i = 0;
    while i < RETIREMENT_COUNTER_NAMES.len() {
        assert!(
            RETIREMENT_COUNTER_NAMES[i].len() <= crate::diag::MAX_CONFIG_NAME,
            "allocation counter name exceeds MAX_CONFIG_NAME and would merge with another"
        );
        i += 1;
    }
    // The two GetStandardAllocationDriverData names are not in the array above
    // because they are authored on a different DDI; check them here.
    assert!(b"StdFmt".len() <= crate::diag::MAX_CONFIG_NAME);
    assert!(b"StdSelf".len() <= crate::diag::MAX_CONFIG_NAME);
};

/// Count one refusal and mirror it on the file's bounded cadence.
///
/// Returns the new count so a caller that wants to pack a reason code into the
/// mirrored value can do so.
///
/// PASSIVE_LEVEL only — `diag::record_named_bytes` is a synchronous
/// `RtlWriteRegistryValue`. Every caller is a PASSIVE allocation DDI.
fn bump(counter: &AtomicU32, name: &[u8]) -> u32 {
    let n = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if n == 1 || n % 64 == 0 {
        crate::diag::record_named_bytes(name, n);
    }
    n
}

static ALLOC_FLUSH_TICKS: AtomicU32 = AtomicU32::new(0);
static ALLOC_FLUSH_FAILURES: AtomicU32 = AtomicU32::new(0);

/// The counters this file publishes WITHOUT [`bump`], because their sites are
/// success paths rather than refusals.
///
/// [`bump`] is the right emitter for a refusal: refusals are rare, so a mirror on
/// the 1st and every 64th is bounded by construction. It is the WRONG emitter for
/// anything that fires on success — `diag::record_named_bytes` is a synchronous
/// `RtlWriteRegistryValue`, and a success-path counter's period is set by the
/// workload, not by the driver. `OaNoRid` is exactly that: one hit per successful
/// open, on DWM's path, for as long as the A3 gap stays open.
///
/// So the atomic stays hot-path-free and this block mirrors it, which is the
/// shape [`APERTURE_MISSING_CPU_VISIBLE`] already uses (`diag::CounterBlock`,
/// x-dup-dead-27: three modules had each hand-rolled the same dump with
/// different throttles).
///
/// ⚠ The flush site is the CREATE DDI, not the open DDI, and that is deliberate:
/// flushing from the site being measured would just re-impose the same period on
/// the same path. An allocation is created once and opened at least once per
/// device that binds it, so creates are the rarer event — one block flush per 64
/// creates, at `PASSIVE_LEVEL`, off the open path entirely. The value is a
/// cumulative atomic, so a create-less stretch costs mirror LATENCY and never a
/// wrong number, and `DiagLevel >= 1` flushes every call regardless.
static ALLOC_COUNTERS: crate::diag::CounterBlock = crate::diag::CounterBlock {
    entries: &[
        crate::diag::CounterEntry {
            name: b"PrNoRid",
            value: crate::diag::CounterRef::U32(&PRESENT_NO_ALLOC_INFO),
            // A VALUE entry for [`OPEN_NO_RESOURCE_ID`]'s reason, one level
            // further along: while the A3 gap is open this fires on EVERY
            // present, so a failure entry would force a registry flush per
            // frame.
            failure: false,
        },
        crate::diag::CounterEntry {
            name: b"OaNoRid",
            value: crate::diag::CounterRef::U32(&OPEN_NO_RESOURCE_ID),
            // A VALUE entry, not a failure: it is expected to be large and
            // rising until A3/K6 land, and marking it a failure would force an
            // immediate flush on every single open — reintroducing precisely the
            // per-open registry write this block exists to remove.
            failure: false,
        },
        crate::diag::CounterEntry {
            name: b"AcGenEpoch",
            value: crate::diag::CounterRef::U32(
                &crate::adapter::allocation_object::GENERATION_EPOCH_BUMPS,
            ),
            // Homed HERE, in a K4 file, rather than left as a second cross-lane
            // request: `adapter/allocation_object.rs` owns the epoch but owns no
            // dump site, and its own doc named that as an open item. This block
            // closes it. The atomic's OWN gap — `invalidate_all` still has no
            // caller — is unaffected and stays reported; what changes is that
            // when K9/K10 wire it, the evidence is readable with `reg query`
            // instead of only under a debugger.
            //
            // A FAILURE entry, so any movement flushes on the next create rather
            // than up to 63 creates later. Adapter resets are rare by
            // construction, so the forced flush is bounded — unlike `OaNoRid`
            // above, which is why the two entries differ.
            failure: true,
        },
        crate::diag::CounterEntry {
            name: b"AcOptLin",
            value: crate::diag::CounterRef::U32(&CREATE_OPTIMAL_AS_LINEAR),
            // A VALUE entry for `OaNoRid`'s reason exactly: it fires on every
            // successful D3D12 committed-texture create, so marking it a failure
            // would force a registry write per create on that lane's hot path.
            failure: false,
        },
    ],
    ticks: &ALLOC_FLUSH_TICKS,
    failures: &ALLOC_FLUSH_FAILURES,
    policy: crate::diag::FlushPolicy::EveryNth(64),
};

/// Mirror [`ALLOC_COUNTERS`] on its own throttle. PASSIVE_LEVEL only.
fn dump_alloc_counters() {
    ALLOC_COUNTERS.flush();
}

/// [`bump`] for a counter whose mirrored value carries a reason code in the low
/// 16 bits, so one registry read names both how often and why.
fn bump_with_code(counter: &AtomicU32, name: &[u8], code: u32) {
    let n = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if n == 1 || n % 64 == 0 {
        crate::diag::record_named_bytes(name, (n << 16) | (code & 0xFFFF));
    }
}

/// Per-device open state for an allocation. Dxgkrnl's `hAllocation` in
/// `DXGK_OPENALLOCATIONINFO` is its non-device-specific allocation handle; the
/// miniport must return its own device-specific handle here and later receives it
/// in command allocation lists / CloseAllocation.
const OPEN_ALLOCATION_CTX_MAGIC: u32 = 0x484F_504E; // "HOPN"

/// `DXGK_OPENALLOCATIONFLAGS::Create` — "Indicates that this allocation is being
/// created, if not set then allocation is being opened" (`d3dkmddi.h`). Read
/// from the union's `Value` because the bitfield accessor's bindgen name is not
/// stable across kits, and the bit position is.
const DXGK_OPENALLOCATION_FLAG_CREATE: u32 = 0x0000_0001;

/// `#[repr(C)]` for [`AllocationContext`]'s reason: `open_allocation_context`
/// probes `magic` on a handle it has no other evidence about.
#[repr(C)]
struct OpenAllocationContext {
    magic: u32,
    /// Validated immutable view captured from open-time private data. Present
    /// receives only this device-specific open handle, so it must use this
    /// snapshot rather than trying to reinterpret dxgkrnl's runtime token as an
    /// `AllocationContext*`.
    present: Option<PresentAllocInfo>,
    /// Trace-only companion; never read by a decision path.
    present_diag: Option<PresentAllocDiag>,
    /// Exactly what this open PUBLISHED to the guest, for K6's Render/Patch
    /// staleness check and its capability records ([`open_allocation_identity`]).
    ///
    /// ⛔ It lives here and not on [`AllocationContext`] because
    /// `DXGK_ALLOCATIONLIST::hDeviceSpecificAllocation` — the handle Render and
    /// Patch resolve — is THIS object, and the create-time handle never appears
    /// in an allocation list at all. Storing what the guest was told, rather
    /// than re-deriving it, is what makes the two sides unable to disagree.
    identity: Option<OpenIdentity>,
}

/// What an open published, as K6's Render and Patch read it back.
#[derive(Clone, Copy)]
pub(crate) struct OpenIdentity {
    /// The allocation generation in the bytes this open handed the guest.
    pub generation: u64,
    /// [`ALLOC_KIND_HVM1`] / [`ALLOC_KIND_HOC1`], or the `HELIOS_HWA2_KIND_*`
    /// the descriptor carried.
    pub kind: u32,
    /// The allocation's own size, from the same bytes.
    ///
    /// ⛔ It has to come from here. `DXGK_ALLOCATIONLIST` carries a handle, a
    /// `WriteOperation` bit, a `SegmentId` and an address — no length — so a
    /// capability record's `byte_length` (§10.7's 48-byte
    /// `Hnr2PhysicalCapability`) has no other source that is not a guess.
    pub byte_size: u64,
}

/// Surface identity + geometry for a Present allocation-list entry, resolved from
/// its `hDeviceSpecificAllocation` ([`present_alloc_info`]).
///
/// ⚠ **NOTHING CONSTRUCTS THIS TODAY, and that is the A3 gap, not an oversight.**
/// Its `resource_id` is a host resource id, HWA2 deliberately carries none
/// (the "No host resource token, `resid`, PID, …" paragraph on
/// [`helios_protocol::HeliosWddmAllocationDescV2`]; the line range this cite
/// used to carry is stale), and `DXGK_OPENALLOCATIONINFO::hAllocation`
/// is dxgkrnl's runtime token rather than this driver's `AllocationContext*` —
/// so `dxgkddi_open_allocation` has nothing to build one FROM and refuses to
/// fabricate one (see that DDI's doc and the `OaNoRid` counter).
///
/// ⭐ CROSS-LANE, RESOLVED — round 3 of the Phase-2 review found the SYMPTOM
/// site silent. Both of `ddi/display.rs`'s consumers refuse on every call while
/// this is `None`, and they did so through the pre-existing last-value
/// breadcrumb `PBFlip`/`PBCpy = 0xE1`, which in that file already means
/// "dxgkrnl handed us a handle we could not resolve" — so the retirement's
/// intended intermediate state was indistinguishable from a handle-lifetime bug,
/// and a last-value write could not even say whether it fired once or per frame.
/// The display sites now write `0xEA` and bump [`PRESENT_NO_ALLOC_INFO`]
/// (`PrNoRid`), the symptom-side pair to [`OPEN_NO_RESOURCE_ID`]'s cause side.
///
/// The type,
/// `present_alloc_info`, and `ddi/display.rs`'s exhaustive consumers are all
/// otherwise retained unchanged so that mesa lane unit **A3** plus K6 re-point the
/// producer and nothing else has to move.
#[derive(Clone, Copy)]
#[allow(dead_code)] // no producer until A3/K6; see the paragraph above.
pub struct PresentAllocInfo {
    pub resource_id: u32,
    /// Versioned allocation kind from the creator/open identity. Present uses
    /// this explicit contract to choose image-vs-buffer interpretation; a
    /// resource id is never guessed from geometry or memory visibility.
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    /// Authoritative byte stride of KMD-created standard allocations. Ordinary
    /// UMD OPTIMAL images leave this at zero because they have no linear row
    /// layout; Present destinations backed by the GDI staging contract carry
    /// the exact 256-byte-aligned pitch.
    pub pitch: u32,
    /// Exact memory-plane-0 offset carried in the allocation private data.
    pub plane_offset: u64,
    /// Authoritative legacy D3DDDIFORMAT supplied for KMD-created standard
    /// allocations. Some such allocations predate an exact DXGI trailer.
    pub format: u32,
    /// Exact creator-side DXGI format; unlike D3DDDIFORMAT, this preserves
    /// BGRA alpha-vs-X identity.
    pub dxgi_format: u32,
    /// Exact D3D11 DDI bind flags used to create the ordinary OPTIMAL image.
    pub bind_flags: u32,
    /// The creator's image/buffer storage contract, captured from authoritative
    /// private data. Present must match this exhaustively; a STANDARD allocation
    /// is not inherently a linear byte buffer.
    pub storage: PresentAllocationStorage,
    /// Exact external allocation contract required by Venus import.
    pub venus_alloc_size: u64,
    pub memory_type_index: u32,
    /// The allocation was created from the runtime's documented
    /// `pPrimaryDesc` contract and explicitly exported for direct scanout.
    pub direct_scanout: bool,
}

/// TRACE-ONLY companion to [`PresentAllocInfo`], resolved by
/// [`present_alloc_diag`].
///
/// These seven fields have no consumer outside the Present identity dump: they
/// are read, formatted and written to the registry, and nothing branches on
/// them. Splitting them out of the acted-upon struct is what makes that
/// visible — the Present path can no longer accidentally make a decision on a
/// value that exists only to be logged, and the trace resolves them only inside
/// its own sampling gate.
#[derive(Clone, Copy)]
pub struct PresentAllocDiag {
    /// Per-device runtime allocation token supplied by dxgkrnl in
    /// `DXGK_OPENALLOCATIONINFO::hAllocation`.
    pub runtime_allocation: u32,
    /// Exact `D3DKMDT_STANDARDALLOCATION_TYPE` supplied by Windows, or zero for
    /// a UMD-created allocation.
    pub standard_allocation_type: u32,
    /// Exact `D3DKMDT_GDISURFACETYPE` supplied by Windows, or zero when the
    /// standard allocation is not a GDI surface.
    pub standard_gdi_surface_type: u32,
    /// Exact `DXGK_OPENALLOCATIONFLAGS::Value` supplied by dxgkrnl.
    pub open_flags: u32,
    /// Whether `DXGK_CREATEALLOCATIONFLAGS::Resource` was set for the
    /// allocation's create call.
    pub resource_associated: bool,
    pub allocation_private_size: u32,
    pub resource_private_size: u32,
}

/// ⚠ No variant is constructed today — same A3 gap as [`PresentAllocInfo`],
/// which is the only thing that holds one.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // no producer until A3/K6; see `PresentAllocInfo`.
pub enum PresentAllocationStorage {
    /// Ordinary UMD shared OPTIMAL image, imported through OPAQUE_FD.
    OptimalOpaqueFdImage = 0,
    /// Cross-context DMA_BUF image (direct primary or a KMD-created GDI
    /// redirection texture).
    OptimalCrossContextImage = 1,
    /// KMD-created standard CPU-visible surface with an authoritative pitch.
    PitchedStandardBuffer = 2,
}

impl PresentAllocInfo {
    /// Resolve the exact Vulkan/DXGI Present format without
    /// geometry/content heuristics.
    ///
    /// UMD-created allocations carry an exact DXGI value. KMD-created standard
    /// allocations may carry only the authoritative D3DDDIFORMAT; use the same
    /// fixed mapping as UMD `d3d_format_to_dxgi`.
    pub fn resolved_dxgi_format(self) -> Option<u32> {
        match self.dxgi_format {
            exact if exact != 0 => Some(exact),
            // Same fixed mapping as UMD `d3d_format_to_dxgi`, now single-sourced.
            0 => d3dddi_to_dxgi(self.format).map(DxgiFormat::as_u32),
            _ => None,
        }
    }
}

/// A resolved byte row stride for a surface that HAS a linear row layout.
///
/// R1007. "What is this surface's row stride" had several independent answers:
/// `OpenAllocation` implemented the full three-arm rule, `ScanoutTarget::new`
/// the two-arm form without the OPTIMAL case, and
/// `GetStandardAllocationDriverData` re-implemented the authoring half four
/// times. They agree today only because KMD standard allocations are authored
/// with exactly `cross_adapter_pitch(width)` -- while the primary arm overwrites
/// `meta.pitch` with Vulkan's `scanout.row_pitch`, which is NOT derived from
/// width.
///
/// Constructible only through [`RowPitch::resolve`], so a byte-addressing
/// consumer cannot obtain a stride for a tiled surface: `resolve` answers `None`
/// there, and `None` is not 0.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowPitch(u32);

impl RowPitch {
    /// The single rule.
    ///
    ///   no linear row layout -> `None`  (a tiled/OPTIMAL surface)
    ///   authored pitch       -> that pitch
    ///   otherwise            -> `cross_adapter_pitch(width)`
    ///
    /// ⚠ The first parameter used to be the retired trailer's `misc_flags` word,
    /// tested against `HELIOS_WDDM_ALLOC_MISC_OPTIMAL_GDI_TEXTURE`. HWA2 has no
    /// such misc bit — §10.3 offset 88's swizzle/layout class is the field that
    /// says whether a linear row layout exists — so the parameter is now the
    /// PREDICATE itself and each caller states which field it derived it from.
    /// Passing a flags word here would silently be `true` for any nonzero value.
    ///
    /// ⚠ `cross_adapter_pitch` HARDCODES 32 bpp (`width * 4`, 256-aligned),
    /// while the UMD's equivalent uses `dxgi_bytes_per_pixel`. The fallback
    /// therefore assumes every KMD standard allocation is 4 bpp. That is true
    /// today -- `GetStandardAllocationDriverData` authors them all that way --
    /// and it is the assumption to revisit first if a non-32-bpp standard
    /// allocation ever appears.
    pub(crate) fn resolve(has_linear_row_layout: bool, pitch: u32, width: u32) -> Option<Self> {
        if !has_linear_row_layout {
            return None;
        }
        Some(Self(if pitch != 0 {
            pitch
        } else {
            cross_adapter_pitch(width)
        }))
    }

    /// The same rule for a surface KNOWN to have a linear row layout.
    ///
    /// A scan-out target is linear by construction -- an OPTIMAL GDI texture can
    /// never be a primary -- so the tiled arm cannot apply and this is total.
    pub(crate) fn linear(pitch: u32, width: u32) -> Self {
        match Self::resolve(true, pitch, width) {
            Some(p) => p,
            // Unreachable: `resolve` answers None only when the caller states
            // there is no linear row layout, which is not what is passed above.
            // Falls back to the value the pre-R1007 two-arm expression produced
            // rather than refusing; this tranche adds no refusals.
            None => Self(cross_adapter_pitch(width)),
        }
    }

    pub(crate) fn get(self) -> u32 {
        self.0
    }
}

/// Row-count alignment for an external LINEAR image, in rows.
///
/// EMPIRICAL, and named so it reads as one. NVIDIA's external-linear image
/// requirements round the row count up to GOB granularity; 128 is what the
/// measurements below produced. It is not derived from a documented rule.
const NV_LINEAR_ROW_ALIGN: u64 = 128;

/// Opaque tail slack an external LINEAR image requires beyond the padded rows.
///
/// Equally empirical. The measurements that produced both constants:
///   1896x48   -> 487424  vs 368640  tight
///   1896x1030 -> 8773632 vs 7913472 tight
///   1024x1872 -> 7864320 =  pitch * align(1872, 128)
const NV_LINEAR_TAIL_SLACK: u64 = 64 * 1024;

/// `D3DKMDT_GDISURFACETYPE` value for a GDI texture (OPTIMAL tiling, no linear
/// CPU byte view). It was a bare `1` compared against `gdi_surface_type`.
const GDI_SURFACE_TYPE_TEXTURE: u32 = 1;

/// Size a blob that will be imported as an external LINEAR VkImage on the host.
///
/// Deliberately LARGER than pitch x height. A blob smaller than the image
/// requirement binds "successfully" and then MMU-faults when the sampler reads
/// the slack region (host Xid 31, FAULT_PTE VIRT_READ — killed the IDD feed live
/// 2026-07-04).
///
/// The failure mode of a WRONG guess is at least loud: the importer refuses
/// undersized imports, so an insufficient bound surfaces as a failed open rather
/// than a GPU fault. But it surfaces at a DISTANT stage — as an import error, not
/// a sizing error — which is why `BlbSzD` counts the divergence between this
/// guess and the exact Vulkan requirement the create path later learns.
fn linear_blob_size(pitch: u64, height: u64) -> u64 {
    let padded_rows = (height + (NV_LINEAR_ROW_ALIGN - 1)) & !(NV_LINEAR_ROW_ALIGN - 1);
    pitch
        .saturating_mul(padded_rows)
        .saturating_add(NV_LINEAR_TAIL_SLACK)
        .max(PAGE as u64)
}

/// Allocations **this driver authored** whose pre-create size estimate differed
/// from the Vulkan memory requirement the create path later learned — value is
/// the count. Read as a VALUE, never as a failure: nothing acts on it.
///
/// ⭐ RE-GRADED by round 3 of the Phase-2 review, and the old grading is kept
/// below because reading the counter under it inverts what it means.
///
/// It used to read: *"the guess is currently AUTHORITATIVE for
/// shadow/staging/GDI-staging surfaces (`create_one` passes `ap.size` straight
/// to `allocate_memory_blob` and only back-fills `meta.venus_alloc_size`), so
/// this measures how good it is without changing it."* Neither `ap` nor `meta`
/// is code anywhere in this file any more — both records were retired with the
/// pre-retirement ABI — and the increment site had been left unscoped, so it
/// compared the host's **page-rounded** blob size against the **UMD's** resource
/// extent. `allocate_memory_blob` does `round_up_page(size.max(4096))`, and
/// `K4-CONTRACT.md` §1.3 rules that a UMD's `byte_size` is the RESOURCE's extent
/// and deliberately need not equal the backing's. So the counter had degenerated
/// into a census of allocations whose size is not a multiple of 4096 — most
/// D3D11 and D3D12 textures — while §1.3 and this doc both still described it as
/// a measurement of `NV_LINEAR_ROW_ALIGN`/`NV_LINEAR_TAIL_SLACK`. A reader would
/// have taken a large value as proof those constants were catastrophically
/// wrong.
///
/// What it measures now: for the two surfaces
/// `dxgkddi_get_standard_allocation_driver_data` authors — the LINEAR scan-out
/// primary and the `OPAQUE_OPTIMAL` GDI texture — the count of creates where the
/// KMD's own pre-create estimate disagreed with the host's measured requirement.
/// That is the question the constants are on trial for. The comparison is taken
/// against the estimate as authored, before `create_one`'s Tier-1 adoption can
/// overwrite it; comparing after the adoption would make it an identity on the
/// LINEAR arm and the counter a constant zero.
///
/// ⚠ It does NOT distinguish the two arms, and their consequences differ: on the
/// LINEAR arm the host's answer is adopted, on the OPTIMAL arm the estimate
/// survives. Split it before using it to argue about either arm alone.
pub(crate) static LINEAR_BLOB_SIZE_DIVERGENCE: AtomicU32 = AtomicU32::new(0);

/// The three DXGI formats this driver ever names.
///
/// The bare numbers 28 / 87 / 88 used to appear in four places, each with its own
/// mapping rule — `resolved_dxgi_format`, the GDI-texture arm, the
/// standard-allocation author, and a local const block — including one that
/// forces 88 for the primary for scan-out reasons. A silent divergence between
/// any two of those tables is a black or refused surface, and nothing made them
/// agree.
///
/// `TryFrom<u32>`, not `From`: the KMD receives arbitrary `u32` values from the
/// UMD trailer, so an unknown format must be `None` rather than a fabricated
/// variant.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DxgiFormat {
    R8G8B8A8Unorm = 28,
    B8G8R8A8Unorm = 87,
    B8G8R8X8Unorm = 88,
}

impl DxgiFormat {
    pub(crate) const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl TryFrom<u32> for DxgiFormat {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, ()> {
        match value {
            28 => Ok(Self::R8G8B8A8Unorm),
            87 => Ok(Self::B8G8R8A8Unorm),
            88 => Ok(Self::B8G8R8X8Unorm),
            _ => Err(()),
        }
    }
}

/// The one D3DDDIFORMAT -> DXGI mapping. `None` for a format with no DXGI peer.
pub(crate) fn d3dddi_to_dxgi(format: u32) -> Option<DxgiFormat> {
    if format == D3DDDIFMT_A8B8G8R8 as u32 {
        Some(DxgiFormat::R8G8B8A8Unorm)
    } else if format == D3DDDIFMT_A8R8G8B8 as u32 {
        Some(DxgiFormat::B8G8R8A8Unorm)
    } else if format == D3DDDIFMT_X8R8G8B8 as u32 {
        Some(DxgiFormat::B8G8R8X8Unorm)
    } else {
        None
    }
}

/// The scan-out primary's format, which is NOT a function of D3DDDIFORMAT.
///
/// Kept a distinct function rather than folded into [`d3dddi_to_dxgi`] because
/// the frozen baseline depends on it: the display primary must scan out as
/// XR24/XRGB on the virtio-gpu contract — the Linux virtio primary plane
/// advertises XRGB only, and the matching CachyOS dma-buf probe reached
/// egl-headless only with XR24.
pub(crate) const fn scanout_dxgi_for_primary() -> DxgiFormat {
    DxgiFormat::B8G8R8X8Unorm
}

/// Resolve a Present allocation-list entry's `hDeviceSpecificAllocation` (an
/// [`OpenAllocationContext`] we returned from `DxgkDdiOpenAllocation`) to the
/// backing venus resource id + geometry. Returns `None` for a null handle.
///
/// SAFETY: `h` must be an `hDeviceSpecificAllocation` value the KMD returned from
/// `DxgkDdiOpenAllocation` (dxgkrnl round-trips it unmodified in command/present
/// allocation lists) and still open (not yet `CloseAllocation`-freed).
pub unsafe fn present_alloc_info(h: HANDLE) -> Option<PresentAllocInfo> {
    // SAFETY: validated by `open_allocation_context`, which reads the magic
    // through an unaligned raw read before forming any reference.
    let open = unsafe { open_allocation_context(h)? };
    open.present
}

/// Validate an `hDeviceSpecificAllocation` BEFORE forming a reference to it.
///
/// The handle is an integer from dxgkrnl and the check cannot be encoded — but
/// the ORDER can. Both callers used to do `&*(h as *const OpenAllocationContext)`
/// and only then test the magic, so any non-null garbage handle was a kernel
/// dereference that bugchecked before the magic check could refuse it: a stale
/// handle after CloseAllocation, or a future DDI routing a different list
/// through here. Now alignment and the magic are checked through
/// `read_unaligned(addr_of!(..))`, which forms no reference to the whole struct,
/// and refusals are counted in `OaBadH`.
///
/// # Safety
/// `h`, when it passes the magic check, is one of our live
/// `OpenAllocationContext` pointers. That a magic-matching pointer really is
/// live is dxgkrnl's contract and is not encodable.
unsafe fn open_allocation_context<'a>(h: HANDLE) -> Option<&'a OpenAllocationContext> {
    if h.is_null() {
        return None;
    }
    let p = h as *const OpenAllocationContext;
    if !p.is_aligned() {
        refuse_open_allocation_handle();
        return None;
    }
    // SAFETY: `p` is non-null and correctly aligned. Reading ONLY the magic
    // field through `addr_of!` + `read_unaligned` does not assert that the whole
    // referent is initialized or dereferenceable, which is exactly the property
    // the old `&*` cast asserted before it had any evidence for it.
    let magic = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*p).magic)) };
    if magic != OPEN_ALLOCATION_CTX_MAGIC {
        refuse_open_allocation_handle();
        return None;
    }
    // SAFETY: the magic matched, so this is one of our contexts.
    Some(unsafe { &*p })
}

/// Non-null open-allocation handles that failed alignment or the magic check.
///
/// Must read 0: every handle reaching here came from our own
/// `DxgkDdiOpenAllocation`. Movement means dxgkrnl routed something else through
/// the present allocation list, or a handle outlived its CloseAllocation.
static OPEN_ALLOC_BAD_HANDLE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Count a refused handle, mirroring on a bounded cadence — this is the Present
/// path, so an unthrottled `record_named_bytes` would be a per-frame registry
/// write.
fn refuse_open_allocation_handle() {
    let n = OPEN_ALLOC_BAD_HANDLE.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
    if n == 1 || n % 64 == 0 {
        crate::diag::record_named_bytes(b"OaBadH", n);
    }
}

/// Trace-only identity for a Present allocation-list entry. Call ONLY from
/// inside a diag sampling gate — nothing here may influence a Present decision.
///
/// # Safety
/// As [`present_alloc_info`].
pub unsafe fn present_alloc_diag(h: HANDLE) -> Option<PresentAllocDiag> {
    // SAFETY: as `present_alloc_info` — validated before any reference is formed.
    let open = unsafe { open_allocation_context(h)? };
    open.present_diag
}

/// Snapshot of the [`AllocationContext`] fields `BuildPagingBuffer` needs to
/// service content/placement ops against the CPU-visible BAR segment.
#[derive(Clone, Copy)]
pub(crate) struct PagingAllocInfo {
    pub resource_id: u32,
    pub size: u64,
    /// Where `size` came from. Carried so the aperture path can eventually
    /// require [`BackingSize::HostAuthoritative`] in its signature rather than
    /// inferring it; today it only feeds the `ChSzMm` cross-check.
    pub size_provenance: BackingSize,
    pub bar_eligible: bool,
    /// Current placement ([`BAR_UNPLACED`] if none).
    pub bar_placed: u64,
    /// K2a. See [`AllocationContext::hlm1_eligible`].
    pub hlm1_eligible: bool,
    /// K2a. See [`AllocationContext::hlm1_bound`]. Read by the bind, which
    /// decides `Bind`/`Rebind`/`None` from this one snapshot.
    pub hlm1_bound: u64,
}

/// Allocation handles refused because they were null or failed the magic check
/// (`DsBad` — DescribeAllocation) and reclaim sites that refused to reconstruct
/// a `Box` from such a handle (`FreeBad`). Both must stay 0.
static DESCRIBE_BAD_HANDLE: AtomicU32 = AtomicU32::new(0);
pub(crate) static RECLAIM_BAD_HANDLE: AtomicU32 = AtomicU32::new(0);

/// The ONE place a dxgkrnl allocation handle becomes an `&AllocationContext`.
///
/// Every accessor in this file open-codes the same null + magic pair, and
/// `DxgkDdiDescribeAllocation` open-coded neither — it dereferenced the handle
/// raw. Honest note on the guarantee: a magic check does NOT reliably detect a
/// freed Box (freed non-paged pool often still reads back as "HALC"). What it
/// buys is one owner for the cast plus a counter if a foreign handle ever shows
/// up (k-alloc-02).
///
/// # Safety
/// `h` must be a handle this driver returned from `DxgkDdiCreateAllocation` and
/// that dxgkrnl still considers live.
unsafe fn resolve_alloc(h: HANDLE) -> Option<&'static AllocationContext> {
    if h.is_null() {
        return None;
    }
    // SAFETY: non-null; the magic word is checked before any other field is
    // trusted, and the caller guarantees the handle's provenance.
    let ctx = unsafe { &*(h as *const AllocationContext) };
    (ctx.magic == ALLOCATION_CTX_MAGIC).then_some(ctx)
}

/// Geometry `DxgkDdiDescribeAllocation` reports, from a magic-checked handle.
pub(crate) struct DescribeInfo {
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

/// Reclaim ownership of an allocation handle's `Box`, but only if it still
/// passes the magic check.
///
/// A handle that does not is LEAKED on purpose: reconstructing a `Box` from a
/// pointer this driver did not mint would free foreign pool. Counted as
/// `FreeBad`, which must stay 0.
///
/// # Safety
/// `h` must be a handle from `DxgkDdiCreateAllocation` that dxgkrnl is handing
/// back exactly once for reclamation.
unsafe fn take_alloc_ctx(h: HANDLE) -> Option<Box<AllocationContext>> {
    if unsafe { resolve_alloc(h) }.is_none() {
        RECLAIM_BAD_HANDLE.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    // SAFETY: the magic check just proved this is one of our boxes, and the
    // caller guarantees dxgkrnl hands each handle back once.
    Some(unsafe { Box::from_raw(h as *mut AllocationContext) })
}

/// Reclaim an open-allocation handle's `Box`, magic-checked like
/// [`take_alloc_ctx`]. A handle that fails is leaked and counted (`FreeBad`).
///
/// # Safety
/// `h` must be a `hDeviceSpecificAllocation` this driver published, handed back
/// exactly once.
unsafe fn take_open_ctx(h: HANDLE) -> Option<Box<OpenAllocationContext>> {
    if h.is_null() {
        return None;
    }
    // SAFETY: non-null; magic is read before any other field is trusted.
    let magic_ok =
        unsafe { (*(h as *const OpenAllocationContext)).magic } == OPEN_ALLOCATION_CTX_MAGIC;
    if !magic_ok {
        RECLAIM_BAD_HANDLE.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    // SAFETY: as above, plus the caller's once-only guarantee.
    Some(unsafe { Box::from_raw(h as *mut OpenAllocationContext) })
}

/// # Safety
/// As [`resolve_alloc`].
unsafe fn describe_alloc_info(h: HANDLE) -> Option<DescribeInfo> {
    let ctx = unsafe { resolve_alloc(h) }?;
    Some(DescribeInfo {
        width: ctx.width,
        height: ctx.height,
        format: ctx.format,
    })
}

/// Resolve a paging-op `hAllocation` (the handle this driver returned from
/// `DxgkDdiCreateAllocation`) to its paging view. Returns `None` for null or
/// magic-mismatched handles — a garbage dereference here would bugcheck.
///
/// SAFETY: `h` must be an in-flight paging op's `hAllocation` (dxgkrnl keeps
/// the allocation alive across its paging operations).
pub(crate) unsafe fn paging_alloc_info(h: HANDLE) -> Option<PagingAllocInfo> {
    let ctx = unsafe { resolve_alloc(h) }?;
    Some(PagingAllocInfo {
        resource_id: ctx.resource_id,
        size: ctx.size as u64,
        size_provenance: ctx.size_provenance,
        bar_eligible: ctx.bar_eligible,
        bar_placed: ctx.bar_placed.load(Ordering::Acquire),
        hlm1_eligible: ctx.hlm1_eligible,
        hlm1_bound: ctx.hlm1_bound.load(Ordering::Acquire),
    })
}

/// The Windows-supplied identity of one specific `hAllocation`, plus the
/// geometry and layout the UMD created it with.
///
/// This is the *unvalidated* half. It says what Windows named and what the
/// allocation claims about itself; it does NOT say that any of it is a legal
/// scan-out target. Produced only by [`scanout_alloc_info`].
///
/// It used to be the same type as the scan-out target
/// (`ScanoutInfo`), which meant `production_linear_scanout` returned a value
/// whose `primary_*` fields were meaningless zeros — twice — and the programming
/// path then juggled a `source` and a `target` whose fields were valid in
/// different subsets, with correctness resting on the author remembering to read
/// the address from `source`. Writing `last_primary_address.store(
/// target.primary_address, ..)` compiled and published 0 as the displayed
/// address, making the flip unretirable.
#[derive(Clone, Copy)]
pub(crate) struct WindowsPrimary {
    pub resource_id: u32,
    pub width: u32,
    pub height: u32,
    /// Row pitch the UMD laid the surface out with (bytes) — the stride
    /// `SET_SCANOUT_BLOB` must use, NOT `width*4`. 0 if unknown.
    pub pitch: u32,
    /// Exact DXGI format (lossless) for resolving the virtio scan-out format.
    pub dxgi_format: u32,
    /// Memory-plane-0 byte offset for `SET_SCANOUT_BLOB` (0 if data starts at 0).
    pub plane_offset: u64,
    /// Exact Venus allocation identity used by cross-context imports.
    pub venus_alloc_size: u64,
    pub memory_type_index: u32,
    /// Whether the UMD created this primary in the proven directly-scannable
    /// shape. Kept HERE and not on the target: the programming path still
    /// branches on it to decide whether to publish the fallback cache.
    pub direct_scanout: bool,
    /// Exact `PrimarySegment` paired with this hAllocation by Windows.
    pub primary_segment: u32,
    /// Exact `PrimaryAddress` paired with this hAllocation by Windows. The ONLY
    /// address that may ever be published as displayed.
    pub primary_address: u64,
    /// Exact `DXGK_SETVIDPNSOURCEADDRESS_FLAGS::Value` supplied by Windows.
    pub primary_flags: u32,
    /// The presentation epoch the flip that named this allocation minted, or
    /// `NO_LEASE` on the MMIO path. Paired with the allocation rather than read
    /// from a global, because the pending-flip slot coalesces (ROADMAP 0ab-B).
    pub present_epoch: u64,
    /// The frame-completion boundary that flip took out of the mark table, or 0.
    /// Same pairing argument as `present_epoch`, and the same reason: the single
    /// pending-flip slot coalesces.
    pub frame_watermark: u64,
    /// D4b: the validated snapshot descriptor that flip carried, or `None`.
    /// Same pairing argument again — the descriptor must travel with the exact
    /// handle it was flipped with, not through a coalescing global. When
    /// present, the bind paths build the `ScanoutTarget` from IT instead of
    /// from this primary's own layout; everything else here (address, epoch,
    /// retirement) still describes the flipped allocation.
    pub snapshot: Option<SnapshotDescriptor>,
}

/// A scan-out surface that has been validated as legal for `SET_SCANOUT_BLOB`.
///
/// Private fields and exactly two constructors, both returning
/// `Result<Self, ScanoutReject>`: [`Self::from_direct_primary`] and
/// [`Self::adapter_linear`]. There is no way to partially initialise one, and it
/// carries no `primary_address` — the fallback path cannot construct the type
/// that publication needs.
///
/// The arm IS the constructor, so there is no `direct_scanout` flag here either.
#[derive(Clone, Copy)]
pub(crate) struct ScanoutTarget {
    resource_id: u32,
    width: u32,
    height: u32,
    /// Already resolved: the allocation's own pitch if it carried one, else the
    /// same 256-byte alignment the UMD uses. Never 0.
    pitch: u32,
    plane_offset: u32,
    venus_alloc_size: u64,
    memory_type_index: u32,
    format: ScanoutFormat,
    /// The DXGI value this target was built from, preserved verbatim for the
    /// fallback cache (`remember_primary_scanout`) so the published identity is
    /// byte-identical to what it was before R507.
    dxgi_format: u32,
}

impl ScanoutTarget {
    /// Validate a UMD-created primary for DIRECT scan-out.
    ///
    /// ⚠ These checks are the guard that keeps QEMU from reading past the blob
    /// (the undersize-guard lesson from the 38th session). They are moved
    /// VERBATIM, saturating arithmetic included. Do not "simplify" them.
    pub(crate) fn from_direct_primary(
        primary: &WindowsPrimary,
        width: u32,
        height: u32,
    ) -> Result<Self, ScanoutReject> {
        let min_size = primary
            .plane_offset
            .saturating_add((primary.pitch as u64).saturating_mul(height as u64));
        let valid = primary.pitch >= width.saturating_mul(4)
            && primary.pitch & 3 == 0
            && primary.plane_offset <= u32::MAX as u64
            && primary.venus_alloc_size >= min_size
            && ScanoutFormat::from_dxgi(primary.dxgi_format).is_some();
        if !valid {
            return Err(ScanoutReject::Layout);
        }
        Self::new(
            primary.resource_id,
            width,
            height,
            primary.pitch,
            primary.plane_offset,
            primary.venus_alloc_size,
            primary.memory_type_index,
            primary.dxgi_format,
        )
    }

    /// Validate a D4b snapshot descriptor as the bind target (beside
    /// [`Self::from_direct_primary`], same validation, same
    /// `fill_set_scanout_blob` inputs: resid/width/height/format/stride/
    /// offset).
    ///
    /// The layout predicate is the SHARED one in
    /// `helios_kmd_logic::snapshot_bind` — the identical arithmetic the direct
    /// arm spells inline, so the undersize guard cannot be restated here in a
    /// weakened form. The descriptor was already validated at the Present DDI;
    /// re-running it costs a few compares and keeps this constructor
    /// impossible to reach with an unchecked layout.
    ///
    /// `memory_type_index` is 0: the target's memory type is only ever
    /// consumed by the LINEAR-fallback cache (`remember_primary_scanout`),
    /// which a snapshot target never feeds — the snapshot has no cross-process
    /// import identity to remember.
    pub(crate) fn from_snapshot_descriptor(
        snap: &SnapshotDescriptor,
    ) -> Result<Self, ScanoutReject> {
        if helios_kmd_logic::snapshot_bind::validate_layout(snap).is_err() {
            return Err(ScanoutReject::Layout);
        }
        Self::new(
            snap.resource_id,
            snap.width,
            snap.height,
            snap.pitch,
            snap.plane_offset,
            snap.venus_alloc_size,
            0,
            snap.dxgi_format,
        )
    }

    /// Build the adapter-owned LINEAR fallback target.
    ///
    /// Same pitch resolution as the direct arm so the two behave identically,
    /// even though the LINEAR pitch is never 0 in practice.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn adapter_linear(
        resource_id: u32,
        width: u32,
        height: u32,
        pitch: u32,
        plane_offset: u64,
        venus_alloc_size: u64,
        memory_type_index: u32,
        dxgi_format: u32,
    ) -> Result<Self, ScanoutReject> {
        Self::new(
            resource_id,
            width,
            height,
            pitch,
            plane_offset,
            venus_alloc_size,
            memory_type_index,
            dxgi_format,
        )
    }

    /// The shared tail of both constructors: resolve the pitch, then resolve the
    /// wire format.
    ///
    /// Order matters and matches the pre-R507 code: the pitch substitution ran
    /// AFTER the direct arm's checks (which is why `from_direct_primary`
    /// validates against the RAW pitch), and the format conversion ran after
    /// both.
    #[allow(clippy::too_many_arguments)]
    fn new(
        resource_id: u32,
        width: u32,
        height: u32,
        pitch: u32,
        plane_offset: u64,
        venus_alloc_size: u64,
        memory_type_index: u32,
        dxgi_format: u32,
    ) -> Result<Self, ScanoutReject> {
        // Stride MUST match the UMD's actual row pitch (`cross_adapter_pitch`,
        // 256-aligned), NOT `width*4`: for 1896 wide that is 7680 vs 7584, and a
        // wrong stride shears the scan-out so the host reads each row 96 bytes
        // short. Fall back to the same alignment the UMD uses if the allocation
        // carried no pitch. R1007: one resolver, shared with OpenAllocation.
        let pitch = RowPitch::linear(pitch, width).get();
        // Resolve the scan-out format from the creator's EXACT DXGI format (the
        // KMD D3DDDIFORMAT is lossy — B8G8R8A8 and R8G8B8A8 both collapse to
        // A8R8G8B8). The legacy-zero arm is what the converter has always
        // accepted; the direct arm's stricter validator already ran above.
        let Some(format) = ScanoutFormat::from_dxgi_or_legacy_zero(dxgi_format) else {
            return Err(ScanoutReject::Format(dxgi_format));
        };
        Ok(Self {
            resource_id,
            width,
            height,
            pitch,
            plane_offset: plane_offset as u32,
            venus_alloc_size,
            memory_type_index,
            format,
            dxgi_format,
        })
    }

    pub(crate) fn resource_id(&self) -> u32 {
        self.resource_id
    }
    pub(crate) fn width(&self) -> u32 {
        self.width
    }
    pub(crate) fn height(&self) -> u32 {
        self.height
    }
    pub(crate) fn pitch(&self) -> u32 {
        self.pitch
    }
    pub(crate) fn plane_offset(&self) -> u32 {
        self.plane_offset
    }
    pub(crate) fn venus_alloc_size(&self) -> u64 {
        self.venus_alloc_size
    }
    pub(crate) fn memory_type_index(&self) -> u32 {
        self.memory_type_index
    }
    pub(crate) fn format(&self) -> ScanoutFormat {
        self.format
    }
    pub(crate) fn dxgi_format(&self) -> u32 {
        self.dxgi_format
    }
}

/// Preserve the exact segment, address, flags, presentation epoch, frame
/// boundary and D4b snapshot descriptor Windows (or the DMA-flip record)
/// paired with a SetVidPn allocation.
///
/// `present_epoch` is `NO_LEASE` on the MMIO path, where dxgkrnl retires the
/// flip before calling us and there is nothing to gate, and the minted epoch on
/// the DMA-buffer flip contract. `frame_watermark` is 0 on the MMIO path for the
/// matching reason: that path has no earlier capture point to carry from, so its
/// bind samples the boundary exactly as it always has. `snapshot` is `None`
/// there too (the desktop always binds the allocation itself).
///
/// All are stored on EVERY call, including with 0/`None`: this is the only
/// writer, so an unconditional store is what guarantees a bind cannot read a
/// value left behind by an older flip of the same allocation.
///
/// SAFETY: `h` is the live KMD allocation handle supplied by dxgkrnl to
/// `DxgkDdiSetVidPnSourceAddress`, or the one this driver copied into the
/// kernel-only DMA private data for a flip.
pub(crate) unsafe fn set_vidpn_primary_address(
    h: HANDLE,
    primary_segment: u32,
    primary_address: u64,
    primary_flags: u32,
    present_epoch: u64,
    frame_watermark: u64,
    snapshot: Option<SnapshotDescriptor>,
) -> bool {
    if h.is_null() {
        return false;
    }
    let ctx = unsafe { &*(h as *const AllocationContext) };
    if ctx.magic != ALLOCATION_CTX_MAGIC {
        return false;
    }
    ctx.vidpn_primary_segment
        .store(primary_segment, Ordering::Relaxed);
    ctx.vidpn_primary_flags
        .store(primary_flags, Ordering::Relaxed);
    ctx.vidpn_present_epoch
        .store(present_epoch, Ordering::Relaxed);
    ctx.vidpn_frame_watermark
        .store(frame_watermark, Ordering::Relaxed);
    // The snapshot stamp, resid LAST among its fields so a reader that races
    // this store observes either the old descriptor, the new one, or a mix of
    // two VALIDATED descriptors — never a nonzero resid paired with wholly
    // unwritten geometry from a zeroed stamp.
    let snap = snapshot.unwrap_or(SnapshotDescriptor {
        resource_id: 0,
        width: 0,
        height: 0,
        pitch: 0,
        dxgi_format: 0,
        plane_offset: 0,
        venus_alloc_size: 0,
        memory_type_index: 0,
        purpose: 0,
    });
    ctx.vidpn_snap_width.store(snap.width, Ordering::Relaxed);
    ctx.vidpn_snap_height.store(snap.height, Ordering::Relaxed);
    ctx.vidpn_snap_pitch.store(snap.pitch, Ordering::Relaxed);
    ctx.vidpn_snap_dxgi_format
        .store(snap.dxgi_format, Ordering::Relaxed);
    ctx.vidpn_snap_plane_offset
        .store(snap.plane_offset as u32, Ordering::Relaxed);
    ctx.vidpn_snap_alloc_size
        .store(snap.venus_alloc_size, Ordering::Relaxed);
    ctx.vidpn_snap_resid
        .store(snap.resource_id, Ordering::Relaxed);
    ctx.vidpn_primary_address
        .store(primary_address, Ordering::Release);
    true
}

/// The venus resource behind an `hAllocation`, or 0 for a null/foreign handle or
/// an unbacked allocation.
///
/// Exists so the DISPATCH-level flip arm can name the buffer whose frame mark it
/// must take without building a whole [`WindowsPrimary`] for one field.
///
/// # Safety
/// Same contract as [`scanout_alloc_info`].
pub(crate) unsafe fn allocation_resource_id(h: HANDLE) -> u32 {
    if h.is_null() {
        return 0;
    }
    let ctx = unsafe { &*(h as *const AllocationContext) };
    if ctx.magic != ALLOCATION_CTX_MAGIC {
        return 0;
    }
    ctx.resource_id
}

/// The allocation generation and record kind held by one KMD allocation object.
///
/// The generation is the value this unit minted at create and stamped into the
/// descriptor it wrote back; the kind is the `HELIOS_HWA2_KIND_*` it was created
/// with, or [`ALLOC_KIND_HVM1`] / [`ALLOC_KIND_HOC1`]. `None` for a null or
/// foreign handle.
///
/// # ⛔ NOT K6's READER — that is [`open_allocation_identity`]
///
/// This answers what the KERNEL OBJECT holds, keyed on the create-time
/// `hAllocation`. Render and Patch never see that handle:
/// `DXGK_ALLOCATIONLIST::hDeviceSpecificAllocation` is the OPEN handle
/// (`d3dkmddi.h`), so a K6 check written against this accessor would compare the
/// create-minted generation with the OPEN-minted one the guest actually holds
/// and refuse every use record. The doc here used to name K6 as the reader; that
/// was written before the create-time write was measured to go nowhere
/// (`FINDINGS.md` F11), and it would have cost K6 a whole debugging round.
///
/// ⛔ It answers "what generation does this object hold", NEVER "which object has
/// this generation". §10.3:1049 forbids the second reading and nothing that
/// resolves an allocation *from* a generation may be added beside it.
///
/// # Safety
/// Same contract as [`scanout_alloc_info`]: `h` is either null or an
/// `hAllocation` this driver returned from `DxgkDdiCreateAllocation` and
/// dxgkrnl has round-tripped unmodified.
#[allow(dead_code)] // reader is K6 (`ddi/native_render.rs`); see the note above.
pub(crate) unsafe fn allocation_identity(h: HANDLE) -> Option<(u64, u32)> {
    if h.is_null() {
        return None;
    }
    let ctx = unsafe { &*(h as *const AllocationContext) };
    if ctx.magic != ALLOCATION_CTX_MAGIC {
        return None;
    }
    Some((ctx.generation, ctx.kind))
}

/// Resolve a primary allocation's `hAllocation` (the CreateAllocation handle
/// dxgkrnl passes in `SetVidPnSourceAddress`) to its scan-out geometry + layout
/// for `SET_SCANOUT_BLOB`. Returns `None` for a null/foreign handle or an
/// unbacked allocation. SAFETY: same contract as [`paging_alloc_info`].
pub(crate) unsafe fn scanout_alloc_info(h: HANDLE) -> Option<WindowsPrimary> {
    if h.is_null() {
        return None;
    }
    let ctx = unsafe { &*(h as *const AllocationContext) };
    if ctx.magic != ALLOCATION_CTX_MAGIC || ctx.resource_id == 0 {
        return None;
    }
    // Acquire on the address pairs with the Release in
    // `set_vidpn_primary_address`, so every companion field stored before it —
    // the presentation epoch, the watermark, and the D4b snapshot stamp — is
    // visible here. Loaded FIRST for that reason.
    let primary_address = ctx.vidpn_primary_address.load(Ordering::Acquire);
    let snap_resid = ctx.vidpn_snap_resid.load(Ordering::Relaxed);
    let snapshot = if snap_resid != 0 {
        Some(SnapshotDescriptor {
            resource_id: snap_resid,
            width: ctx.vidpn_snap_width.load(Ordering::Relaxed),
            height: ctx.vidpn_snap_height.load(Ordering::Relaxed),
            pitch: ctx.vidpn_snap_pitch.load(Ordering::Relaxed),
            dxgi_format: ctx.vidpn_snap_dxgi_format.load(Ordering::Relaxed),
            plane_offset: ctx.vidpn_snap_plane_offset.load(Ordering::Relaxed) as u64,
            venus_alloc_size: ctx.vidpn_snap_alloc_size.load(Ordering::Relaxed),
            memory_type_index: 0,
            purpose: 0,
        })
    } else {
        None
    };
    Some(WindowsPrimary {
        resource_id: ctx.resource_id,
        width: ctx.width,
        height: ctx.height,
        pitch: ctx.pitch,
        dxgi_format: ctx.dxgi_format,
        plane_offset: ctx.plane_offset,
        venus_alloc_size: ctx.venus_alloc_size,
        memory_type_index: ctx.memory_type_index,
        direct_scanout: ctx.direct_scanout,
        primary_segment: ctx.vidpn_primary_segment.load(Ordering::Relaxed),
        primary_address,
        primary_flags: ctx.vidpn_primary_flags.load(Ordering::Relaxed),
        present_epoch: ctx.vidpn_present_epoch.load(Ordering::Relaxed),
        frame_watermark: ctx.vidpn_frame_watermark.load(Ordering::Relaxed),
        snapshot,
    })
}

/// Rebuild the published [`PreparedImageCopy`] snapshot from its atomic mirror.
///
/// The atomics stay raw `u64` — that is what an `AtomicU64` can hold — so this
/// is the ONE place raw words become typed handles, and it is a *validating*
/// restore: a snapshot missing any of the three handles it cannot function
/// without is no snapshot at all and reads as `None`. Before the handle
/// newtypes those three were `!= 0` tests scattered across the two consumers
/// (`submit_prepared_image_copy` had two of them; the third had none).
///
/// `scanout_copy_command_buffer_id` is the publish word: acquiring a nonzero
/// value there means the eight Relaxed payload stores that preceded its Release
/// store are visible, so the rest of the snapshot is coherent.
fn cached_prepared_copy(
    ctx: &AllocationContext,
) -> Option<crate::virtio::venus::PreparedImageCopy> {
    use crate::virtio::venus::{VkCommandBufferId, VkCommandPoolId, VkDeviceMemoryId, VkImageId};

    let command_buffer_id =
        VkCommandBufferId::from_raw(ctx.scanout_copy_command_buffer_id.load(Ordering::Acquire))?;
    let command_pool_id =
        VkCommandPoolId::from_raw(ctx.scanout_copy_pool_id.load(Ordering::Relaxed))?;
    let source_image_id = VkImageId::from_raw(ctx.scanout_copy_image_id.load(Ordering::Relaxed))?;
    let target_image_id =
        VkImageId::from_raw(ctx.scanout_copy_target_image_id.load(Ordering::Relaxed))?;
    let owns_source_alias = ctx.scanout_copy_owns_source_alias.load(Ordering::Relaxed) != 0;
    Some(crate::virtio::venus::PreparedImageCopy {
        owns_source_alias,
        source_resource_id: if owns_source_alias {
            ctx.resource_id
        } else {
            0
        },
        source_image_id,
        source_memory_id: VkDeviceMemoryId::from_raw(
            ctx.scanout_copy_memory_id.load(Ordering::Relaxed),
        ),
        conversion_image_id: VkImageId::from_raw(
            ctx.scanout_copy_conversion_image_id.load(Ordering::Relaxed),
        ),
        conversion_memory_id: VkDeviceMemoryId::from_raw(
            ctx.scanout_copy_conversion_memory_id
                .load(Ordering::Relaxed),
        ),
        conversion_init_pool_id: VkCommandPoolId::from_raw(
            ctx.scanout_copy_conversion_init_pool_id
                .load(Ordering::Relaxed),
        ),
        command_pool_id,
        command_buffer_id,
        target_image_id,
        width: ctx.width,
        height: ctx.height,
    })
}

/// `None` stores as 0, the value the mirror has always used for "absent".
fn raw<T: Into<u64>>(id: Option<T>) -> u64 {
    id.map_or(0, Into::into)
}

fn publish_prepared_copy(ctx: &AllocationContext, copy: &crate::virtio::venus::PreparedImageCopy) {
    // command_buffer_id is the publish word. A reader that acquires a nonzero
    // command id sees one coherent immutable PreparedImageCopy snapshot.
    ctx.scanout_copy_owns_source_alias
        .store(copy.owns_source_alias as u32, Ordering::Relaxed);
    ctx.scanout_copy_image_id
        .store(copy.source_image_id.get(), Ordering::Relaxed);
    ctx.scanout_copy_memory_id
        .store(raw(copy.source_memory_id), Ordering::Relaxed);
    ctx.scanout_copy_conversion_image_id
        .store(raw(copy.conversion_image_id), Ordering::Relaxed);
    ctx.scanout_copy_conversion_memory_id
        .store(raw(copy.conversion_memory_id), Ordering::Relaxed);
    ctx.scanout_copy_conversion_init_pool_id
        .store(raw(copy.conversion_init_pool_id), Ordering::Relaxed);
    ctx.scanout_copy_pool_id
        .store(copy.command_pool_id.get(), Ordering::Relaxed);
    ctx.scanout_copy_target_image_id
        .store(copy.target_image_id.get(), Ordering::Relaxed);
    ctx.scanout_copy_command_buffer_id
        .store(copy.command_buffer_id.get(), Ordering::Release);
}

/// The exact mirror of [`publish_prepared_copy`]: payload words Relaxed FIRST,
/// then the publish word with Release.
///
/// The clear used to run in the opposite order — publish word first, payload
/// after — so between the two a reader that acquired a *stale-nonzero* publish
/// word could read half-cleared payload. That reader is not constructible
/// today: the scanout-lifecycle mutex orders every access, and `take`-style
/// readers hold it for their whole critical section. This removes a trap rather
/// than fixing a race, and the trap is real — four call sites can each mutate
/// these ten words, so any new writer outside the mutex would tear the slot.
fn clear_prepared_copy(ctx: &AllocationContext) {
    ctx.scanout_copy_last_fence.store(0, Ordering::Relaxed);
    ctx.scanout_copy_target_image_id.store(0, Ordering::Relaxed);
    ctx.scanout_copy_pool_id.store(0, Ordering::Relaxed);
    ctx.scanout_copy_conversion_init_pool_id
        .store(0, Ordering::Relaxed);
    ctx.scanout_copy_conversion_memory_id
        .store(0, Ordering::Relaxed);
    ctx.scanout_copy_conversion_image_id
        .store(0, Ordering::Relaxed);
    ctx.scanout_copy_memory_id.store(0, Ordering::Relaxed);
    ctx.scanout_copy_image_id.store(0, Ordering::Relaxed);
    ctx.scanout_copy_owns_source_alias
        .store(0, Ordering::Relaxed);
    // The publish word LAST, with Release — the mirror of publish's
    // eight-Relaxed-then-one-Release protocol.
    ctx.scanout_copy_command_buffer_id
        .store(0, Ordering::Release);
}

fn orphaned_copy_requires_backing_retain(ctx: &AllocationContext) -> bool {
    let orphaned = ctx.scanout_copy_orphaned.load(Ordering::Acquire) != 0;
    if orphaned {
        bump(&PRIMARY_COPY_ORPHAN_RETAINED, b"CpOrKeep");
    }
    orphaned
}

/// Submit a GPU copy from the exact allocation selected by
/// `SetVidPnSourceAddress` into the durable adapter-owned LINEAR scanout image.
/// Setup (external-memory import + command recording) happens once per WDDM
/// allocation; the frame path only queues the reusable command buffer and
/// returns its ring-1 GPU-completion fence.
///
/// Takes the `WindowsPrimary` rather than a loose `(handle, address)` pair, so
/// the address it hands the copy is provably the one Windows paired with THIS
/// allocation instead of whatever the caller passed alongside the handle.
///
/// SAFETY: `h` is the live `hAllocation` passed by dxgkrnl to
/// SetVidPnSourceAddress, and `primary` is the identity resolved from that same
/// handle. PASSIVE_LEVEL only (the Venus client mutex may wait).
pub(crate) unsafe fn submit_primary_scanout_copy(
    adapter: &AdapterContext,
    lock: &ScanoutGuard<'_>,
    h: HANDLE,
    primary: &WindowsPrimary,
    target_image_id: u64,
    width: u32,
    height: u32,
    ticket: crate::adapter::ProgrammingTicket,
) -> Result<u64, NTSTATUS> {
    let primary_address = primary.primary_address;
    if h.is_null() || target_image_id == 0 || width == 0 || height == 0 {
        return Err(STATUS_INVALID_PARAMETER);
    }
    let ctx = unsafe { &*(h as *const AllocationContext) };
    if ctx.magic != ALLOCATION_CTX_MAGIC
        || ctx.resource_id == 0
        || ctx.width != width
        || ctx.height != height
    {
        crate::diag::record_named_bytes(b"CpCpy", 0xE1);
        return Err(STATUS_INVALID_PARAMETER);
    }
    if ScanoutFormat::from_dxgi(ctx.dxgi_format).is_none() {
        crate::diag::record_named_bytes(b"CpFmt", ctx.dxgi_format);
        crate::diag::record_named_bytes(b"CpCpy", 0xE2);
        return Err(STATUS_NOT_SUPPORTED);
    }
    if ctx.scanout_copy_orphaned.load(Ordering::Acquire) != 0 {
        bump(&PRIMARY_COPY_ORPHAN_REFUSED, b"CpOrRef");
        crate::diag::record_named_bytes(b"CpCpy", 0xE3);
        return Err(STATUS_DEVICE_NOT_READY);
    }

    // Through the scanout token: the second of the two Venus acquisitions that
    // run under `scanout_mutex` (see `ScanoutGuard`).
    let mut orphan_refused = false;
    let mut orphan_transition_failed = false;
    let result = lock.with_venus_client(|client| {
        // Retarget: a cached copy baked against a *different* destination image
        // is destroyed and rebuilt. Matching on the option directly replaces a
        // map-then-unwrap_or guard followed by a take-then-unwrap — two
        // statements that had to agree for the unwrap to be sound. Note the
        // cache-HIT path must fall through with the value still in place; a bare
        // `if let Some(old) = prepared.take()` would destroy it every frame.
        let prepared = match cached_prepared_copy(ctx) {
            Some(old)
                if Some(old.target_image_id)
                    != crate::virtio::venus::VkImageId::from_raw(target_image_id) =>
            {
                if ctx
                    .scanout_copy_orphaned
                    .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    orphan_refused = true;
                    return Err(crate::virtio::VirtioError::DeviceError);
                }
                // Poison before withdrawing: a partial destructor cannot be retried,
                // and DestroyAllocation must retain its backing for context teardown.
                clear_prepared_copy(ctx);
                match client.destroy_prepared_image_copy(adapter, old) {
                    Ok(()) => {
                        ctx.scanout_copy_orphaned.store(0, Ordering::Release);
                        None
                    }
                    Err(e) => {
                        orphan_transition_failed = true;
                        return Err(e);
                    }
                }
            }
            other => other,
        };
        let copy = match prepared {
            Some(copy) => copy,
            None => {
                let copy = if ctx.venus_image_id != 0 {
                    client.prepare_existing_linear_source_copy(
                        adapter,
                        ctx.venus_image_id,
                        width,
                        height,
                        ctx.dxgi_format,
                        target_image_id,
                    )?
                } else {
                    client.prepare_optimal_scanout_copy(
                        adapter,
                        ctx.resource_id,
                        ctx.venus_alloc_size,
                        ctx.memory_type_index,
                        width,
                        height,
                        ctx.dxgi_format,
                        ctx.bind_flags,
                        target_image_id,
                    )?
                };
                publish_prepared_copy(ctx, &copy);
                copy
            }
        };
        let fence = client.submit_prepared_image_copy(adapter, &copy, primary_address, ticket)?;
        ctx.scanout_copy_last_fence.store(fence, Ordering::Release);
        Ok::<u64, crate::virtio::VirtioError>(fence)
    });

    if orphan_refused {
        bump(&PRIMARY_COPY_ORPHAN_REFUSED, b"CpOrRef");
    }
    if orphan_transition_failed {
        bump(&PRIMARY_COPY_ORPHAN_TRANSITION_FAILED, b"CpOrFail");
    }

    match result {
        Ok(Ok(fence)) => {
            let n = PRIMARY_COPY_SUBMIT_COUNT
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1);
            if n == 1 || n % 600 == 0 {
                crate::diag::record_named_bytes(b"CpCpy", 1);
                crate::diag::record_named_bytes(b"CpFnc", fence as u32);
                crate::diag::record_named_bytes(b"CpCnt", n);
            }
            Ok(fence)
        }
        Ok(Err(_)) => {
            crate::diag::record_named_bytes(b"CpCpy", 0xE3);
            Err(STATUS_DEVICE_NOT_READY)
        }
        Err(_) => {
            crate::diag::record_named_bytes(b"CpCpy", 0xE4);
            Err(STATUS_DEVICE_NOT_READY)
        }
    }
}

/// Record (or clear, with [`BAR_UNPLACED`]) an allocation's VidMm-assigned BAR
/// SegmentAddress. SAFETY: same contract as [`paging_alloc_info`].
pub(crate) unsafe fn set_bar_placement(h: HANDLE, offset: u64) {
    if h.is_null() {
        return;
    }
    let ctx = unsafe { &*(h as *const AllocationContext) };
    if ctx.magic == ALLOCATION_CTX_MAGIC {
        ctx.bar_placed.store(offset, Ordering::Release);
    }
}

/// Record the window offset an HLM1 allocation's blob is now fixed-mapped at
/// (or [`BAR_UNPLACED`] to clear). SAFETY: as [`set_bar_placement`].
///
/// ⛔ Called only with an offset `map_blob_at` actually returned `Ok` for. A
/// store on the failure path would make the next observation report
/// `Action::None` and skip the retry — see `hlm1_placement::BindingState`.
pub(crate) unsafe fn set_hlm1_binding(h: HANDLE, offset: u64) {
    if h.is_null() {
        return;
    }
    let ctx = unsafe { &*(h as *const AllocationContext) };
    if ctx.magic == ALLOCATION_CTX_MAGIC {
        ctx.hlm1_bound.store(offset, Ordering::Release);
    }
}

/// Direct-scan-out allocations, keyed by venus resource id.
///
/// WHY IT EXISTS. `DxgkDdiPresent` receives only `hDeviceSpecificAllocation`
/// (an `OpenAllocationContext*`), while the scan-out path keys on the GLOBAL
/// allocation handle (`AllocationContext*`) that `DxgkDdiSetVidPnSourceAddress`
/// supplies. On the MMIO flip path that DDI hands the global handle over; on
/// the DMA-BUFFER FLIP path it is never called, so Present has to bridge the
/// two itself. There is no back-pointer to bridge with — `DXGK_OPENALLOCATIONINFO`
/// carries a `D3DKMT_HANDLE`, dxgkrnl's runtime token, NOT this driver's
/// pointer — and the create-time private data is UMD-visible, so smuggling a
/// kernel pointer through it would be both a leak and forgeable. The venus
/// resource id is the one identity both sides already hold honestly.
///
/// Only DIRECT-SCAN-OUT allocations are registered, which is what keeps a fixed
/// table adequate: DWM rotates 3 and an app's flip chain 2-4, so the live set is
/// under ten even across a fullscreen transition.
const SCANOUT_ALLOC_SLOTS: usize = 32;

struct ScanoutAllocSlot {
    resource_id: AtomicU32,
    /// `AllocationContext*` as a `usize`. Written under the same
    /// create/destroy discipline as the Box itself: published here after the
    /// Box is leaked into `info.hAllocation`, and cleared in
    /// `destroy_allocation_ctx` BEFORE the Box is dropped.
    allocation: core::sync::atomic::AtomicUsize,
}

impl ScanoutAllocSlot {
    const NEW: Self = Self {
        resource_id: AtomicU32::new(0),
        allocation: core::sync::atomic::AtomicUsize::new(0),
    };
}

static SCANOUT_ALLOCS: [ScanoutAllocSlot; SCANOUT_ALLOC_SLOTS] =
    [ScanoutAllocSlot::NEW; SCANOUT_ALLOC_SLOTS];

/// Registrations refused because every slot was taken (diag `ScAlcFul`). Each
/// one is a direct primary the DMA-flip path cannot resolve, so it must read 0.
pub(crate) static SCANOUT_ALLOC_FULL: AtomicU32 = AtomicU32::new(0);

/// Publish `allocation` as the global handle for `resource_id`.
fn register_scanout_allocation(resource_id: u32, allocation: usize) {
    if resource_id == 0 || allocation == 0 {
        return;
    }
    for slot in SCANOUT_ALLOCS.iter() {
        // Claim by resource id. Venus resource ids are monotonic and never
        // recycled (`virtio/gpu.rs`), so a successful CAS from 0 can never be
        // confused with a stale entry for a different surface.
        if slot
            .resource_id
            .compare_exchange(0, resource_id, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            slot.allocation.store(allocation, Ordering::Release);
            return;
        }
    }
    SCANOUT_ALLOC_FULL.fetch_add(1, Ordering::Relaxed);
}

/// Withdraw `resource_id`'s registration. Called before the allocation's Box is
/// dropped, so no lookup can return a dangling pointer afterwards.
fn unregister_scanout_allocation(resource_id: u32) {
    if resource_id == 0 {
        return;
    }
    for slot in SCANOUT_ALLOCS.iter() {
        if slot.resource_id.load(Ordering::Acquire) == resource_id {
            // Pointer first: a reader that still sees the id must not then read
            // a stale pointer.
            slot.allocation.store(0, Ordering::Release);
            slot.resource_id.store(0, Ordering::Release);
            return;
        }
    }
}

/// The global allocation handle for `resource_id`, or `None`.
///
/// PASSIVE-level callers only, and only while the allocation cannot be
/// destroyed concurrently — `DxgkDdiPresent` qualifies: dxgkrnl holds the
/// present's allocations resident for the duration of the call.
pub(crate) fn scanout_allocation_for_resource(resource_id: u32) -> Option<HANDLE> {
    if resource_id == 0 {
        return None;
    }
    for slot in SCANOUT_ALLOCS.iter() {
        if slot.resource_id.load(Ordering::Acquire) == resource_id {
            let allocation = slot.allocation.load(Ordering::Acquire);
            if allocation != 0 {
                return Some(allocation as HANDLE);
            }
        }
    }
    None
}

const PAGE: SIZE_T = 4096;
const D3DDDI_ALLOCATIONPRIORITY_NORMAL: UINT = 0x7800_0000;

/// 32-bpp linear row pitch aligned to the cross-adapter requirement
/// (`D3D12_TEXTURE_DATA_PITCH_ALIGNMENT`, 256 bytes). Re-exported so the existing
/// `crate::ddi::create_allocation::cross_adapter_pitch` call path in
/// `ddi/display.rs` is unchanged. (`ddi/gdi_blit.rs` and `ddi/scanout_diag.rs`
/// were the other two callers; T1b and T6/R901 deleted both files.)
///
/// The body now lives in `helios_kmd_logic`, which has no dependency edge to
/// `wdk-sys` or the generated `dxgk` bindings and is covered by host unit tests —
/// including the `1896 -> 7680` case `ddi/display.rs` names, i.e. the `.117`-era
/// short-row scanout defect.
pub(crate) use helios_kmd_logic::cross_adapter_pitch;

/// `helios_kmd_logic::round_up_page` operates on `u64`; the callers here pass
/// `SIZE_T`. The call below type-checks only while the two are the same type, and
/// this assertion makes a future divergence a compile error rather than a silent
/// widening of an allocation size.
const _: () = assert!(core::mem::size_of::<SIZE_T>() == core::mem::size_of::<u64>());

fn round_up_page(n: SIZE_T) -> SIZE_T {
    helios_kmd_logic::round_up_page(n)
}

/// Cycling 8-slot fixed-name registry ring of allocation create/open events, so a
/// single boot's surface map (venus resid + geometry + ctx, create vs open) is
/// readable live — used to correlate DWM's composition surfaces (1952x1088,
/// res_id 52/54) against the IDD's IddCx swapchain surface (1920x1080): same
/// venus resid ⇒ shared (sync problem), different ⇒ the surfaces never alias and
/// the composed pixels are never copied into what the IDD reads.
static ALLOC_EVENT_SEQ: AtomicU32 = AtomicU32::new(0);
/// Successful exact-primary copy submissions. Fixed registry breadcrumbs are
/// throttled from this counter; writing the registry per frame would itself
/// throttle the display path.
static PRIMARY_COPY_SUBMIT_COUNT: AtomicU32 = AtomicU32::new(0);

/// Ticks the create/open breadcrumb throttle (R317 / k-alloc-05). The ring
/// itself stays 8 slots; what changes is how often it reaches the registry.
static ALLOC_EVENT_TICKS: AtomicU32 = AtomicU32::new(0);

fn record_alloc_event(resid: u32, width: u32, height: u32, ctx_id: u32, is_open: bool) {
    // 3 synchronous registry writes per allocation CREATE and per OPEN. Keeping
    // the first occurrence means a one-shot boot repro still shows it.
    if !crate::diag::sample_tick(&ALLOC_EVENT_TICKS) {
        return;
    }
    let i = (ALLOC_EVENT_SEQ.fetch_add(1, Ordering::Relaxed) % 8) as u8;
    let d = b'0' + i;
    crate::diag::record_named_bytes(&[b'A', b'E', d, b'r'], resid);
    crate::diag::record_named_bytes(&[b'A', b'E', d, b'd'], (width << 16) | (height & 0xFFFF));
    crate::diag::record_named_bytes(
        &[b'A', b'E', d, b'c'],
        (ctx_id & 0x7FFF_FFFF) | ((is_open as u32) << 31),
    );
}

// ⛔ DELETED with the retirement's identity model, and named here so a reader
// who greps for them finds the reason rather than a hole:
//
//   * `read_standard_meta` + `META_LEN_REJECTS` (`MetaLen`) — the per-arm
//     `HeliosWddmAllocMeta` trailer parse. HWA2 is one fixed 168-byte record
//     with no trailer and no optional-length arm, so the trailer-length
//     classification it protected no longer exists. Its discipline survives:
//     the record types own the exact-length check inside `from_private_data`,
//     which is the same rule expressed once instead of per call site.
//   * `ParsedAllocIdentity` + `read_alloc_identity` — the open-time identity
//     parser, whose second arm read the UMD's `adopt_resource_id`. Both halves
//     die with UMD-backing adoption (§18.1:4723-4726).
//   * `write_open_identity` — the `HeliosWddmOpenIdentity` stamp. It had THREE
//     call sites: create (retained, now as the HWA2 write-back below) and the
//     two ORDINARY-OPEN restamps (illegal: §10.3:1033-1034, §18.1:4769,
//     A.2 row 5694). Deleting the helper without noticing the create site, or
//     deleting the create site while chasing the restamps, are the two
//     symmetrical ways to get this wrong; both are closed because the create
//     write is now a different function writing a different record.

/// What VidMm is told about one allocation: where it goes and how it may be
/// touched.
///
/// # Why this is one function
///
/// The segments and the flags are ONE decision and were made in two places
/// twenty lines apart, which is how the illegal combination below stayed
/// invisible. dxgkrnl rejects a CpuVisible allocation in a non-CPU-accessible
/// segment unless its supported set contains an APERTURE segment — the v71/v72
/// "10 x 0x0202 violators" VidPn-commit failure — and `set_CpuVisible(!
/// is_optimal_gdi_texture)` marks every BAR-placed allocation CpuVisible while
/// the aperture bit is gated on `DisplayHalf`, a registry knob defaulting to 0.
///
/// So with `DisplayHalf=0` the driver KNOWINGLY emits the shape that produced
/// that failure. The tolerance is real — the render-only surface never demanded
/// the aperture fallback, because its CpuHostAperture always had space — but
/// nothing recorded how large the violating population is, so it could not be
/// re-verified after any change. `ApMiss` is that measurement, incremented
/// exactly where the illegal combination is constructed.
///
/// BEHAVIOUR IS UNCHANGED. The `DisplayHalf` gate is preserved exactly; only the
/// violation becomes visible.
///
/// # The limit of the encoding, which is the point
///
/// A `SupportedSegments` newtype whose `cpu_visible_in(bar)` constructor could
/// only produce `bar_bit | aperture_bit` would make the illegal shape
/// unconstructible — but preserving today's `DisplayHalf=0` behaviour requires a
/// `legacy_render_only()` constructor that still yields it. So the honest
/// guarantee is "the illegal shape has exactly one, explicitly named
/// constructor", not "the illegal shape is unconstructible". This function IS
/// that one constructor.
struct VidMmPlacement {
    preferred_segment: u32,
    supported_segments: u32,
    cpu_visible: bool,
    cached: bool,
    accessed_physically: bool,
    /// `DXGK_ALLOCATIONINFOFLAGS_WDDM2_0::ExplicitResidencyNotification`.
    /// §10.7:2002-2003 requires it on every HVM1 role; §18.1:4751-4753 gates it.
    explicit_residency_notification: bool,
    /// `DXGK_ALLOCATIONINFOFLAGS2::DisablePartialResidency`
    /// (§10.7:2005-2007, gated §18.1:4752-4753).
    disable_partial_residency: bool,
    /// `DXGK_ALLOCATIONINFOFLAGS2::RestrictedToSingleSegment`. Whole-allocation
    /// residency in ONE segment at a time; §10.7:2010-2011 is explicit that this
    /// is **not** a claim that an offset is pinned, so nothing may assume a
    /// fixed BAR offset for an allocation carrying it.
    restricted_to_single_segment: bool,
}

/// The aperture segment's bit in a `SupportedSegmentSet` word.
///
/// `crate::ddi::gpummu::APERTURE_SEGMENT_ID` and
/// `helios_protocol::HELIOS_SEGMENT_ID_APERTURE` are the same segment named from
/// the two sides of the wire; the assertion below is what keeps them the same
/// number, because the HVM1/HOC1 placements state their segments in the protocol
/// vocabulary while the WDDM word is built from the driver's.
const _: () =
    assert!(crate::ddi::gpummu::APERTURE_SEGMENT_ID == helios_protocol::HELIOS_SEGMENT_ID_APERTURE);

/// A `SupportedSegmentSet` bit for a 1-based WDDM segment id.
///
/// Total and panic-free: segment id 0 is `HELIOS_SEGMENT_ID_SYSTEM`, which is
/// the implicit system-memory segment and is never named in a supported set, and
/// an id above 32 cannot be expressed in the word at all. Both answer 0 rather
/// than shifting out of range — a DDI may not panic on a runtime value.
const fn segment_bit(seg_id: u32) -> u32 {
    if seg_id == 0 || seg_id > 32 {
        0
    } else {
        1u32 << (seg_id - 1)
    }
}

/// Is `seg_id` a segment this driver actually REPORTED to dxgkrnl?
///
/// The reported set is `ddi/segment_table.rs`'s immutable post-StartDevice
/// [`crate::ddi::segment_table::SegmentTable`], rendered by
/// `DXGKQAITYPE_QUERYSEGMENT4` (`query_adapter_info.rs::query_segments`). Its ids
/// are POSITIONAL — index 0 is id 1 — and that type owns the mapping precisely so
/// the id and the slot cannot be maintained separately in two files. Asking it is
/// therefore the only way to answer this question without re-deriving the
/// topology; `bar_seg_id()` is the same question asked about the BAR segment, and
/// [`vidmm_placement`] already routes through it.
///
/// `None` — StartDevice has not published a table — resolves to `APERTURE_ONLY`,
/// byte-for-byte the fallback `query_segments` renders in the same case.
/// Answering a placement question against a different table than the one dxgkrnl
/// was shown is the one answer here that would be worse than either true or
/// false.
///
/// ⚠ This is a snapshot of an immutable-after-StartDevice value, so there is no
/// race to close: the table cannot change under a create. If K2 ever makes the
/// table mutable at runtime, this becomes a TOCTOU and the check must move to
/// wherever the table is locked.
fn segment_is_reported(adapter: &AdapterContext, seg_id: u32) -> bool {
    let table = adapter
        .segment_table()
        .unwrap_or(crate::ddi::segment_table::SegmentTable::APERTURE_ONLY);
    // ⚠ Bound to a local rather than returned as the tail expression.
    // `SegmentTable::iter` borrows the table (`+ '_`), and a temporary in a tail
    // expression is dropped AFTER the block's locals — so returning the `any(..)`
    // directly is an E0597 on `table`. The bool is what leaves this function; the
    // borrow must end first.
    let reported = table.iter().any(|(id, _)| id == seg_id);
    reported
}

/// The per-role counter and registry name for "this role's preferred segment is
/// not reported" (`K4-CONTRACT.md` §4).
///
/// An exhaustive `match` rather than an array index: `Hvm1Role` is a closed
/// vocabulary, and a `[AtomicU32; 4]` indexed by `role.to_u32() - 1` would be a
/// panicking index on a value that ultimately came from guest bytes. A DDI may
/// not panic — a panic in any DDI is a silent graphics deadlock — and this shape
/// cannot, while still forcing a new role to be given a name here before it
/// compiles.
fn role_segment_absent_counter(role: Hvm1Role) -> (&'static AtomicU32, &'static [u8]) {
    match role {
        Hvm1Role::ReplyPool => (&CREATE_ROLE1_SEGMENT_ABSENT, b"AcSegRole1"),
        Hvm1Role::VulkanHostVisible => (&CREATE_ROLE2_SEGMENT_ABSENT, b"AcSegRole2"),
        Hvm1Role::Feedback => (&CREATE_ROLE3_SEGMENT_ABSENT, b"AcSegRole3"),
        Hvm1Role::VulkanDeviceLocal => (&CREATE_ROLE4_SEGMENT_ABSENT, b"AcSegRole4"),
    }
}

/// Allocations marked CpuVisible in the BAR segment WITHOUT an aperture segment
/// in their supported set — the shape dxgkrnl rejects for a VidPn commit.
///
/// Expected NONZERO on the render-only configuration (`DisplayHalf=0`) and ZERO
/// on the production `DisplayHalf=1` one. It documents the current population
/// rather than changing it.
///
/// A per-allocation ATOMIC, flushed from an existing PASSIVE dump site — never a
/// per-allocation registry write (T1b's k-alloc-05).
///
/// ⚠ CROSS-LANE: its flush site is `ddi/cpu_host_aperture.rs:141-144`, which K1
/// deletes. K4 does not re-home it (that would be an edit to a file this unit
/// does not own); the counter must be re-homed by whichever of K1/K4's successor
/// runs second, or it silently stops being readable.
pub(crate) static APERTURE_MISSING_CPU_VISIBLE: AtomicU32 = AtomicU32::new(0);

/// The placement for an ordinary HWA2 allocation.
///
/// ⚠ The `TrackingBudget` parameter and its two arms are GONE with the VidMm
/// tracker (`K4-CONTRACT.md` §6): they existed only to place the one-page
/// identity object of a `HELIOS_WDDM_ALLOC_KIND_TRACKING` allocation, a kind
/// HWA2 does not have. Everything else is byte-identical to the pre-retirement
/// rule, including the `DisplayHalf` gate and the `ApMiss` measurement.
///
/// The three WDDM-3.2 flags are **not** set here: §10.7:2005-2007 states them
/// for HVM1 roles, and asserting whole-allocation single-segment residency for
/// every D3D11 texture would be a new claim with no evidence behind it. See
/// [`hvm1_placement`].
fn vidmm_placement(
    bar_eligible: bool,
    bar_seg_id: Option<u32>,
    is_primary: bool,
    cpu_visible: bool,
    display_half: bool,
) -> VidMmPlacement {
    let aperture_bit = segment_bit(crate::ddi::gpummu::APERTURE_SEGMENT_ID);

    let (preferred_segment, supported_segments) =
        if let (true, Some(seg_id)) = (bar_eligible, bar_seg_id) {
            // Prefer the BAR (the two-memory-split fix keeps CPU raster in the venus
            // blob's bytes). These allocations are CpuVisible (set below) in a
            // NON-CPU-accessible memory segment — the BAR exposes CPU access only via
            // the CpuHostAperture (segment CpuVisible=0) — and WDDM REQUIRES every such
            // allocation to list an aperture segment in its supported set so VidMm can
            // always obtain a CPU virtual address, falling back to system memory if the
            // CpuHostAperture is full (allocation-usage-tracking.md: "all CPU-accessible
            // allocations in non-CPU-accessible memory segments must contain an aperture
            // segment in their supported segment set"). v71 added it to the PRIMARY only;
            // the other CpuVisible surfaces (SHADOW/STAGING/GDI) shipped BAR-only and
            // dxgkrnl rejected them ("CPUVisible allocations must include an aperture
            // segment", ETW-confirmed v71/v72 — 10 `0x0202` violators in the S-ring) →
            // the whole VidPn commit failed. Gated on the display half so the proven
            // render-only surface (DisplayHalf off) stays byte-identical: it never hit
            // the rejection because its CpuHostAperture always had space, so the fallback
            // was never demanded. PreferredSegment stays the BAR — with a 1 GiB BAR vs a
            // ~200 MB CpuVisible working set, content lives in the venus blob in steady
            // state and the aperture (which VidMm upgrades to the implicit system-memory
            // segment without AccessedPhysically, iommu-dma-remapping.md) is an
            // eviction-only fallback not exercised at this scale — negligible runtime cost.
            let needs_aperture = is_primary || display_half;
            let supp = segment_bit(seg_id);
            (
                seg_id,
                if needs_aperture {
                    supp | aperture_bit
                } else {
                    supp
                },
            )
        } else {
            (crate::ddi::gpummu::APERTURE_SEGMENT_ID, aperture_bit)
        };
    // THE MEASUREMENT. Counted here, at the one site that constructs the shape:
    // CpuVisible, preferred to the BAR, with no aperture segment to fall back to.
    if cpu_visible
        && Some(preferred_segment) == bar_seg_id
        && supported_segments & aperture_bit == 0
    {
        APERTURE_MISSING_CPU_VISIBLE.fetch_add(1, Ordering::Relaxed);
    }
    VidMmPlacement {
        preferred_segment,
        supported_segments,
        // §10.3 offset 68/92: CPU visibility is a descriptor field the creator
        // states and the KMD validates, not something inferred from the tiling
        // class. The pre-retirement rule inferred it (`!is_optimal_gdi_texture`)
        // because there was no field to read; there is one now, and
        // `validate` already cross-checks it against `memory_class`.
        cpu_visible,
        // Omit `Cached` on the scan-out primary: dxgkrnl rejects
        // Cached-with-Primary (AzureTriage; 36th-session primary-creation
        // failure -> no VidPn path). The primary is host-scanned-out, not
        // CPU-read-hot, so write-combined is fine. The `AllocCached` kill switch
        // is applied by the caller, which owns the knob snapshot.
        cached: !is_primary && cpu_visible,
        // A D3DDDI primary is selected by the display engine using the physical
        // address delivered in SetVidPnSourceAddress. Tell VidMm that exact
        // access model so it allocates the primary contiguously in a
        // GPU-addressable segment rather than at a non-identifiable implicit
        // system-memory address.
        accessed_physically: is_primary,
        explicit_residency_notification: false,
        disable_partial_residency: false,
        restricted_to_single_segment: false,
    }
}

/// The placement §10.7:1999-2011 fixes for every HVM1 role.
///
/// It is `Hvm1Role::placement()` transcribed into the WDDM vocabulary and
/// nothing else: the protocol states the contract once
/// (`protocol/src/native_render.rs:1755-1780`) precisely so the KMD cannot
/// express it differently per call site, and every field below is copied rather
/// than re-decided. The two things this function adds are the WDDM segment-set
/// word and the `AllocCached` interaction, neither of which the protocol can
/// know.
///
/// ⛔ **`preferred_segment` is `HELIOS_SEGMENT_ID_HLM1`, which the segment table
/// may not report.** `K4-CONTRACT.md` §4 is explicit that K4 records the constant
/// and does not depend on the segment existing: K2 (the two-segment table) is
/// unstarted and its host half is parked (`FINDINGS.md` F5). Substituting the
/// aperture segment here would produce a create that "works" while placing
/// native-Vulkan memory somewhere §10.7 forbids, so this function keeps stating
/// the contract exactly.
///
/// ⭐ What it does NOT do any more is let that placement leave the driver
/// unchecked. This doc used to end "so an HVM1 create is refused by dxgkrnl,
/// loudly" — and a refusal by dxgkrnl is a refusal with NO HELIOS COUNTER, which
/// is the opposite of loud from inside the guest. [`admit_hvm1`] now asks
/// [`segment_is_reported`] whether this exact `preferred_segment` is in the table
/// this driver reported, and refuses per role with a named counter when it is not
/// (§4 as amended 2026-08-10). The placement is unchanged; only who refuses it,
/// and whether anyone can see that, changed.
///
/// The aperture bit is in the supported set because §10.7:2001-2002 names "the
/// package's ordinary aperture as the documented system-residency
/// physical-address domain" — the eviction/system-residency domain, not an
/// alternative preference.
///
/// ⛔ **VidMm read it as an alternative preference anyway** (`FINDINGS.md` F15):
/// it placed the role-1 pool in the aperture, `HlPlSg = 1`, with the GPU page
/// table agreeing — so the Lock2 view was guest RAM and F14's defect stood. It is
/// entitled to: `preferred_segment` is a hint. `Hlm1Only` removes the
/// alternative, and defaults OFF because a hard `MakeResident` failure would
/// block A3 entirely (CLAUDE.md rule 8: the measured value is the default, and
/// the other arm stays reachable).
fn hvm1_placement(role: Hvm1Role, hlm1_only: bool, flags_off: u32) -> VidMmPlacement {
    let contract = role.placement();
    let aperture = if hlm1_only {
        0
    } else {
        segment_bit(crate::ddi::gpummu::APERTURE_SEGMENT_ID)
    };
    // `Hlm1FlagsOff` — see `diag::knobs::HLM1_FLAGS_OFF`. Each cleared bit is a
    // candidate explanation for VidMm ending the allocation in the aperture;
    // 0 (the default) states §10.7 exactly.
    let keep = |bit: u32, contract_value: bool| contract_value && flags_off & bit == 0;
    VidMmPlacement {
        preferred_segment: contract.preferred_segment,
        supported_segments: segment_bit(contract.preferred_segment) | aperture,
        cpu_visible: contract.cpu_visible,
        // §10.7:2003-2005 — `Cached = 0` for every role, and the CPU publication
        // ordering proof depends on it: a `HOST_CACHED` mapping would break the
        // `HOST_VISIBLE|HOST_COHERENT` advertisement (§18.1:4763-4766). NOT
        // subject to the `AllocCached` knob, which is a BAR-readback tuning
        // switch for the D3D11 surfaces and has no authority here.
        cached: contract.cached,
        // §10.7:2007-2010 — truthful, not a contiguity request: the legacy
        // physical Render engine really does dereference the final
        // segment/physical-address capability patched from the allocation list.
        // ⚠ If K6's Render/Patch path ever stops dereferencing it, this bit
        // becomes a lie even though it is set exactly as specified.
        accessed_physically: keep(1, contract.accessed_physically),
        // NOT maskable: this bit is what delivers `NOTIFY_RESIDENCY`, which is
        // the arm the bind runs on (F15).
        explicit_residency_notification: contract.explicit_residency_notification,
        disable_partial_residency: keep(2, contract.disable_partial_residency),
        restricted_to_single_segment: keep(4, contract.restricted_to_single_segment),
    }
}

/// The placement §10.6:1510-1512 and §17.6:4414-4418 fix for the HOC1 pool.
///
/// ⚠ Note the DELIBERATE ASYMMETRY with [`hvm1_placement`], which is the obvious
/// implementation error here: HOC1 must **not** claim `AccessedPhysically`
/// (§17.6:4417-4418) whereas every HVM1 role must (§10.7:2002-2003). HOC1 is
/// executed through C64/HPM1 page-table handling from a GPUVA, not through a
/// patched physical capability, so the claim would be untrue.
///
/// "No HAP flags" (§10.6:1511) is satisfied by construction: this driver's
/// CpuHostAperture participation is a segment property, and the HOC1 allocation
/// names no BAR segment.
///
/// ⛔ Same HLM1 caveat as [`hvm1_placement`], and now the same check —
/// "preferred in HLM1, with ordinary system placement supported" needs K2's
/// segment table, so [`admit_hoc1`] refuses through [`segment_is_reported`] and
/// [`CREATE_HOC1_SEGMENT_ABSENT`] rather than letting dxgkrnl refuse it where no
/// Helios counter can see it. The aperture bit is the "ordinary system placement"
/// half and does exist today.
fn hoc1_placement() -> VidMmPlacement {
    VidMmPlacement {
        preferred_segment: HELIOS_SEGMENT_ID_HLM1,
        supported_segments: segment_bit(HELIOS_SEGMENT_ID_HLM1)
            | segment_bit(crate::ddi::gpummu::APERTURE_SEGMENT_ID),
        // §10.6:1510-1511 — nonprimary, nonshared, CPU-visible/WC.
        cpu_visible: true,
        // Write-combined, and the D3D12 UMD's whole seal protocol depends on it:
        // §18.2:4946-4949 rejects device qualification if the returned mapping's
        // cache policy is not WC, because a cached mapping makes the x64 drain
        // that seals an HOB1 unobservable.
        cached: false,
        accessed_physically: false,
        // §10.7's Flags2/residency-notification set is stated for HVM1 roles.
        // §17.6:4414-4418's HOC1 list does not include them, and asserting them
        // here would be a claim with no line behind it.
        explicit_residency_notification: false,
        disable_partial_residency: false,
        restricted_to_single_segment: false,
    }
}

/// Tear down one blob allocation: unmap (if mapped) → detach → unref → free the
/// KMD context. Best-effort on the virtio ops (teardown must not get stuck).
/// PASSIVE_LEVEL (DxgkDdiDestroyAllocation) — the round-trips ride
/// `virtio::ctrl`'s PASSIVE waits.
unsafe fn destroy_allocation_ctx(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx: Box<AllocationContext>,
) {
    let allocation_handle = (&*ctx as *const AllocationContext) as usize;
    // K2a: an HLM1 allocation that lived and died without ever being bound had
    // a CPU view backed by nothing this driver owns. There is no Lock-time
    // callback to refuse it at, so this post-hoc count is the only signal.
    if ctx.hlm1_eligible {
        if ctx.hlm1_bound.load(Ordering::Acquire) == BAR_UNPLACED {
            crate::ddi::build_paging_buffer::HLM1_ERR_NEVER_BOUND.fetch_add(1, Ordering::Relaxed);
        }
        // ⭐ THE ACCEPTANCE ORACLE, and the only one that does not round-trip
        // through the same pointer it is testing (`hts1_session_probe` H5's
        // defect, `FINDINGS.md` F14). Read the BLOB's bytes at the three offsets
        // that probe writes and report them raw: `0xA55AC3` means the guest's
        // Lock2 view IS the blob; the K2a stamp bytes mean the mapping is being
        // read correctly and the guest wrote somewhere else. The KMD knows
        // neither expectation, which is what makes the reading evidence.
        //
        // Before any teardown below: the blob must still exist to be read.
        // SAFETY: PASSIVE (this DDI), and `resource_id` is still live here.
        unsafe {
            crate::ddi::build_paging_buffer::hlm1_readback(
                passive,
                adapter,
                ctx.resource_id,
                ctx.size as u64,
            )
        };
        // Publish here or not at all: the only other flush site is the paging
        // content tail, and destroy runs after this allocation's last paging op.
        // ⛔ Unconditional on eligibility, not on the never-bound arm it used to
        // sit inside — a SUCCESSFUL bind is exactly the case whose counters no
        // other site publishes — and unthrottled, because a bind moves no failure
        // counter and `flush` would then write nothing here.
        crate::ddi::build_paging_buffer::hlm1_publish_counters();
    }
    // Withdraw the DMA-flip lookup FIRST: after this no Present can resolve
    // this resource id to a handle whose Box is about to be dropped.
    unregister_scanout_allocation(ctx.resource_id);
    // ⛔ `adapter.system_backings.remove(ctx.resource_id)` was here.
    //
    // `adapter/backing.rs` is the private byte mirror §10.7:1940 forbids ("its
    // bytes are never an independent private copy") and its deletion is
    // sequenced to K3, which owns 8 of its 10 call sites
    // (`K4-CONTRACT.md` §3). K4 removes only its own call so K3 can delete the
    // file without editing this one.
    //
    // ⚠ TRANSIENT CONSEQUENCE, recorded rather than absorbed: until K3 lands, a
    // destroyed BAR allocation leaves its entry in a table of 128 and
    // `BAR_SYSTEM_BACKING_ERRORS` will start counting once the table fills. The
    // damage is bounded exactly as the k-paging-04 comment argued — resource ids
    // are monotonic and never recycled within a transport generation, so a stale
    // entry consumes a slot rather than aliasing a new allocation, and the
    // Present-time mirror's `contains(resource_id)` therefore cannot answer true
    // for the wrong surface.
    // Retire the exact Windows/KMD allocation identity before any backing
    // resource, Venus image, or cached copy can be torn down. If QEMU cannot
    // confirm resource_id=0 scanout disable, retain every host object until
    // device teardown rather than leave scanout 0 pointing at an unref'd blob.
    if !adapter.retire_scanout_allocation(passive, allocation_handle, ctx.resource_id) {
        let _ = orphaned_copy_requires_backing_retain(&ctx);
        drop(ctx);
        return;
    }
    // A prepared scanout copy owns a command buffer which may still be queued
    // through the outer async SUBMIT_3D. Drain that GPU-completion fence and
    // tear the prepared objects down BEFORE touching the allocation's resource,
    // image, or memory. On an ambiguous drain failure, leak the allocation's
    // host objects to Venus-context teardown rather than use-after-free them.
    if let Some(copy) = cached_prepared_copy(&ctx) {
        // The drain fence lives on the VenusClient now (R609). This read used to
        // load ctx.scanout_copy_last_fence with Acquire BEFORE acquiring the
        // venus mutex, while the only writer performed its Release store INSIDE
        // it — so a concurrent SetVidPnSourceAddress submit could leave this
        // thread with a stale-or-zero fence and skip the mandatory drain.
        let drained = adapter
            .with_venus_client(passive, |client| {
                client.destroy_prepared_image_copy(adapter, copy)
            })
            .map(|r| r.is_ok())
            .unwrap_or(false);
        if !drained {
            crate::diag::record_named_bytes(b"CpDrn", 0xE);
            let _ = orphaned_copy_requires_backing_retain(&ctx);
            drop(ctx);
            return;
        }
        clear_prepared_copy(&ctx);
        crate::diag::record_named_bytes(b"CpDrn", 1);
    }
    if orphaned_copy_requires_backing_retain(&ctx) {
        drop(ctx);
        return;
    }

    // Present BLT command buffers bake imported aliases of ordinary WDDM
    // resources. Drain the cache before any one backing resource can be
    // detached/unref'd. The cache is intentionally one ownership unit because
    // several swapchain sources may share the same DWM destination.
    let windowed_terminal = adapter.with_scanout_lifecycle(passive, |lock| {
        let present_drained = lock
            .with_venus_client(|client| {
                // Undispatched requests have no host reader and cancel now.
                // A dispatched request stays pinned in the FIFO until its
                // exact ring response; cache release below drains that fence
                // before the backing can be destroyed.
                let _ = adapter
                    .with_virtio(|v| v.cancel_windowed_blt_for_resource(adapter, ctx.resource_id));
                client.release_present_blits_for_resource(adapter, ctx.resource_id)
            })
            .map(|result| result.is_ok())
            .unwrap_or(false);
        if !present_drained {
            return false;
        }

        // Keep the scanout lifecycle lock from cancellation through the
        // exact reader terminal.  Releasing it between these phases would
        // let the HPD worker dispatch a request for this resource after
        // the cache drain but before the backing is retained/destroyed.
        // `with_venus_client` has returned before this virtio step, so the
        // established scanout -> Venus ordering is not extended.
        adapter
            .with_virtio(|v| v.finish_windowed_blt_teardown_for_resource(adapter, ctx.resource_id))
            .unwrap_or(false)
    });
    if !windowed_terminal {
        crate::diag::record_named_bytes(b"PBDrn", 0xE);
        // A matching ring-1 command still owns the source or destination.
        // Retain all backing state until context teardown rather than UAF it.
        drop(ctx);
        return;
    }

    // A DWM import of the adapter-owned LINEAR target can acquire a transient
    // WDDM AllocationContext carrying the same resource id.  That allocation
    // is only an importer: destroying it must not clear, detach, unref, or
    // destroy the adapter-owned scanout image/memory.
    let adapter_owned_scanout = ctx.resource_id != 0
        && adapter.dedicated_scanout_resource.load(Ordering::Acquire) == ctx.resource_id;
    if ctx.resource_id != 0 && !adapter_owned_scanout {
        adapter.forget_primary_scanout(ctx.resource_id);
    }
    // `ctx.owns_resource` used to gate this arm. It is GONE: it was false only
    // for a non-owning `AdoptedUmdResource`, and with adoption deleted the KMD
    // creates every backing it names, so the field was a constant `true` for
    // every reachable arm. `resource_id != 0` is the surviving, honest test —
    // an HOC1 pool and a failed backing both have none.
    if ctx.resource_id != 0 && !adapter_owned_scanout {
        // Drop the owner-0 tracking slot (registered at CreateAllocation, or
        // re-owned to the allocation at adopt), unmapping the GDI executor's
        // host-visible mapping if one is live.
        // `forget_allocation_blob` already OWNS the unmap decision. The
        // fallback that used to sit here was gated on `ctx.mapped`, whose doc
        // claimed "true once RESOURCE_MAP_BLOB has succeeded" -- but its only
        // writer set it `false`, so the branch never ran and the doc described
        // a state the field could not reach. T6/R915.
        let _ = crate::virtio::ctrl::forget_allocation_blob(passive, adapter, ctx.resource_id);
        // One guarded teardown path for created AND adopted resources. The old
        // adopted arm unref'd unconditionally, which double-freed resources
        // another path had already reclaimed — QEMU's "virgl_cmd_resource_unref:
        // resource does not exist ×9" at the 2026-07-03 boot-#3 dwm teardown.
        let first_teardown = adapter
            .with_virtio(|v| v.take_live_resource(ctx.resource_id))
            .unwrap_or(false);
        if first_teardown {
            let _ = crate::virtio::ctrl::ctx_detach_resource(
                passive,
                adapter,
                ctx.ctx_id,
                ctx.resource_id,
            );
            let _ = crate::virtio::ctrl::resource_unref(passive, adapter, ctx.resource_id);
        }
        if ctx.venus_image_id != 0 {
            let _ = adapter
                .with_venus_client(passive, |c| c.destroy_image(adapter, ctx.venus_image_id));
        }
        if ctx.venus_memory_id != 0 {
            // KMD-backed standard allocation: after the RESOURCE teardown above
            // (the host blob holds a reference into the memory object),
            // vkFreeMemory the venus memory. Best-effort: if the venus client
            // is already gone (device teardown), the host context destruction
            // reclaims everything anyway.
            let _ = adapter.with_venus_client(passive, |c| {
                c.free_memory_blob(adapter, ctx.venus_memory_id)
            });
        }
    } else if adapter_owned_scanout {
        crate::diag::record_named_bytes(b"CpKeep", ctx.resource_id);
    }
    drop(ctx);
}

/// Where an allocation's backing size came from.
///
/// ⭐ Doc re-derived against HEAD by round 3 of the Phase-2 review. It used to
/// explain itself in terms of `ap.size` being "ICD-supplied for ADOPTED
/// allocations", and of `bar_eligible` resting on "an incidental property" of
/// `venus_memory_id != 0`. **Both describe the pre-retirement tree.** There are
/// no adopted allocations — UMD-backing adoption is deleted with the VidMm
/// tracker (`K4-CONTRACT.md` §6) and `tools/retirement-gates.sh` §8.6 proves it
/// — and `ap` is not code anywhere in this file. That mattered: this predicate
/// is the whole gate on the KMD overwriting its own `byte_size`
/// (`create_one`'s Tier-1 adoption block), so a reviewer checking that gate was
/// being sent to look for a mechanism that no longer exists.
///
/// What the variants separate at HEAD: every backing `build_backing` creates is
/// `HostAuthoritative` — the host allocated it and reported the size back — and
/// `NonHostAuthoritative` marks the one allocation that has no venus object at
/// all, the HOC1 outer-command pool (§10.6; `admit_hoc1` constructs it with
/// `backing: None`, where the size is valid for VidMm accounting and for nothing
/// else). So the type is not degenerate; it separates "a host backing exists and
/// reported its extent" from "a VidMm-only allocation".
///
/// Why it is a type rather than a bool at the call site: it gives Tier 1's
/// adoption and the aperture path a **typed precondition** instead of an
/// implicit one, so the aperture path can eventually REQUIRE `HostAuthoritative`
/// in its signature. `MapCpuHostAperture`'s whole-allocation refusal computes a
/// page count from `PagingAllocInfo::size` while `map_blob_at` maps whatever the
/// TRACKED BLOB size implies — two sources — and that comparison is only sound
/// where the size came from the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackingSize {
    /// The host allocated it and reported the size back.
    HostAuthoritative(u64),
    /// The size is valid for accounting, but not authoritative for BAR mapping.
    NonHostAuthoritative(u64),
}

impl BackingSize {
    pub(crate) fn bytes(self) -> u64 {
        match self {
            Self::HostAuthoritative(n) | Self::NonHostAuthoritative(n) => n,
        }
    }

    pub(crate) fn is_host_authoritative(self) -> bool {
        matches!(self, Self::HostAuthoritative(_))
    }
}

/// Everything one [`Hwa2Backing`] arm must answer.
///
/// NO `Option` fields and NO `Default`, deliberately: that is what forces every
/// arm of [`build_backing`] to produce a COMPLETE descriptor, and what makes
/// adding a backing class a compile error until it is handled. Arms that do not
/// change a field pass the descriptor's value through explicitly — the
/// pass-through is the point, because the defect class here is an arm that
/// forgets one (the historical `width*4` = 7584 pitch shear against the real
/// 7680, and the exact-size import mismatches).
///
/// Do not add a field-wise mutation of this struct after construction. The
/// guarantee holds only while it is built once and read once.
struct CreatedBacking {
    resource_id: u32,
    venus_memory_id: u64,
    venus_image_id: u64,
    /// The row stride the HOST actually laid the surface out with, which for the
    /// LINEAR scan-out arm is NOT derived from width. 0 for a tiled image and
    /// for a plain buffer.
    pitch: u32,
    plane_offset: u64,
    /// The creator's exact `vkAllocateMemory` size + memory type. §10.3's "no
    /// host resource token … or independently usable identity" rule — stated on
    /// [`helios_protocol::HeliosWddmAllocationDescV2`], and on `memory_class` for
    /// the memory-type half — keeps these OUT of HWA2: they are host-side detail
    /// the KMD allocation object owns and no descriptor may name.
    venus_alloc_size: u64,
    memory_type_index: u32,
    /// The extent VidMm is charged, WITH its provenance. NOT always the created
    /// blob's size: the plain-buffer arm keeps the requested size, as it always
    /// has.
    blob_size: BackingSize,
}

/// Which backing the KMD must create for one validated HWA2 descriptor.
///
/// # Why the KMD classifies at all
///
/// §10.3:1026-1027 and §18.1:4723-4726 — the KMD creates and owns the
/// host/Venus backing *as part of creating the exact WDDM allocation*, and no
/// raw `resid` or host backing token is accepted from a UMD/ICD. So the
/// descriptor is a *request* and this enum is the KMD's total reading of it.
///
/// The retired `helios_protocol::classify` answered the same question over
/// `HeliosWddmAllocPrivate` + `HeliosWddmAllocMeta` and lives in
/// `wddm_legacy.rs`, whose own module header records that there is **no 1:1
/// HWA2 replacement**. This is deliberately a KMD-local decision rather than a
/// protocol one: it maps a wire record onto *this driver's* venus-client
/// constructors, and a second consumer with different constructors would need a
/// different map. It defines no wire struct, so it does not cross the
/// "this lane consumes `helios_protocol`, never defines a wire struct" line.
enum Hwa2Backing {
    /// A KMD-created LINEAR scan-out image: the shared-primary standard surface
    /// the display path binds through `SET_SCANOUT_BLOB`.
    LinearScanoutImage { width: u32, height: u32 },
    /// A KMD-created OPTIMAL cross-context image. Tiled, no linear row layout,
    /// sampled by a compositor and rendered into by D3D.
    OptimalImage {
        width: u32,
        height: u32,
        dxgi_format: u32,
        /// D3D11 **DDI** bind bits, translated out of the HWA2 vocabulary by
        /// [`hwa2_bind_to_ddi`] — see that function for why the two vocabularies
        /// are deliberately non-coincident.
        ddi_bind_flags: u32,
    },
    /// A KMD-created linear venus `VkDeviceMemory` blob: shadow/staging/GDI
    /// staging surfaces and ordinary buffers.
    LinearMemory {
        bytes: u64,
        mappable: bool,
        shareable: bool,
    },
}

/// Translate the HWA2 bind vocabulary into the D3D11 DDI bits the kernel venus
/// image constructor speaks.
///
/// ⛔ These two vocabularies are NON-COINCIDENT ON PURPOSE
/// (`protocol/src/wddm.rs`, the offset-72 comment): `SHADER_RESOURCE` is `0x1`
/// in HWA2 and `0x8` at the D3D11 DDI; `RENDER_TARGET` is `0x2` here and `0x1`
/// in the D3D12 DDI. The retired trailer carried the raw DDI word and a second
/// producer wrote a *D3D12* word into the same field — a different vocabulary at
/// overlapping bit positions, which is not a mismatch any reader can detect.
/// Passing an HWA2 word straight into `create_optimal_present_image_alias`
/// would silently drop SAMPLED and add nothing, so this translation is
/// load-bearing rather than cosmetic.
///
/// Only the three bits `create_optimal_present_image_alias` reads are mapped;
/// the rest have no effect on the created image and are deliberately not
/// invented into DDI bits the venus client would ignore anyway.
const fn hwa2_bind_to_ddi(bind_flags: u32) -> u32 {
    const DDI_BIND_SHADER_RESOURCE: u32 = 0x0000_0008;
    const DDI_BIND_RENDER_TARGET: u32 = 0x0000_0020;
    const DDI_BIND_UNORDERED_ACCESS: u32 = 0x0000_0100;
    let mut ddi = 0;
    if bind_flags & HELIOS_HWA2_BIND_SHADER_RESOURCE != 0 {
        ddi |= DDI_BIND_SHADER_RESOURCE;
    }
    if bind_flags & HELIOS_HWA2_BIND_RENDER_TARGET != 0 {
        ddi |= DDI_BIND_RENDER_TARGET;
    }
    if bind_flags & HELIOS_HWA2_BIND_UNORDERED_ACCESS != 0 {
        ddi |= DDI_BIND_UNORDERED_ACCESS;
    }
    ddi
}

/// Is this descriptor the UMD's DIRECT scan-out primary — the exact allocation
/// `SetVidPnSourceAddress` binds through `SET_SCANOUT_BLOB`, with no copy into
/// the adapter-owned target?
///
/// ⭐ **ONE derivation, two readers**, and that is the point: [`classify_hwa2`]
/// must give this shape a real cross-context OPTIMAL image, and [`admit_hwa2`]
/// must register it in `SCANOUT_ALLOCS`. Two independent spellings is exactly the
/// defect this closes — the derivation used to key on `swizzle_class ==
/// HELIOS_HWA2_SWIZZLE_LINEAR`, which is TRUE precisely when the producer said
/// "copy me": `umd/src/forward/alloc.rs` sets `DISPLAYABLE` **and**
/// `OPAQUE_OPTIMAL` together on the direct arm and plain `LINEAR` on the copy
/// arm. Inverted, the zero-copy primary never entered `SCANOUT_ALLOCS` and the
/// copy-path primary always did, so `set_vidpn_source_address` took the wrong arm
/// for both.
///
/// `HELIOS_HWA2_FLAG_DISPLAYABLE` is the successor of the retired
/// `HELIOS_WDDM_ALLOC_MISC_DIRECT_SCANOUT` bit, which the pre-retirement code
/// read directly (`41d13f8:create_allocation.rs:2584`). It is the producer's
/// claim, and it is the only field that carries it.
///
/// ⛔ `STANDARD` excludes the KMD's OWN shared primary, which sets `PRIMARY |
/// DISPLAYABLE` too (`dxgkddi_get_standard_allocation_driver_data`) and means
/// something different by it: those bytes ARE the adapter's LINEAR scan-out
/// image, which the display path reaches through `venus_image_id` /
/// `production_linear_scanout`, never through the resid→handle bridge.
///
/// The swizzle class is required as well as the flag — not as the discriminator
/// but as the layout `ScanoutTarget::from_direct_primary` and the QEMU fork's
/// native reconstruction expect. A hypothetical `DISPLAYABLE` + `LINEAR` primary
/// therefore takes the COPY path, which is the fail-safe direction: that is the
/// same path the OS standard primary uses, and it is the proven desktop.
fn hwa2_is_direct_scanout_primary(desc: &HeliosWddmAllocationDescV2) -> bool {
    desc.has_flag(HELIOS_HWA2_FLAG_PRIMARY)
        && desc.has_flag(HELIOS_HWA2_FLAG_DISPLAYABLE)
        && !desc.has_flag(HELIOS_HWA2_FLAG_STANDARD)
        && desc.swizzle_class == HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL
}

/// Is this the KMD's own `D3DKMDT_GDISURFACE_TEXTURE`?
///
/// §10.3 gives a GDI surface no kind of its own — it is `KIND_IMAGE` plus the
/// `STANDARD` bit plus the exact OS enum at offset 84 — so the OS enum is what
/// names it. The GDI surface's OTHER variants are the CPU-visible staging
/// surfaces, which `dxgkddi_get_standard_allocation_driver_data` authors with
/// `HELIOS_HWA2_SWIZZLE_LINEAR`; only the `GDI_SURFACE_TYPE_TEXTURE` arm sets
/// `OPAQUE_OPTIMAL`, so the caller's layout test separates the two and this
/// predicate does not have to.
///
/// The validator couples the flag and the enum in both directions
/// (`StandardAllocationTypeWithoutStandardFlag` / `StandardAllocationTypeZero`),
/// so testing both is redundancy rather than a second rule.
fn hwa2_is_standard_gdi_texture(desc: &HeliosWddmAllocationDescV2) -> bool {
    desc.has_flag(HELIOS_HWA2_FLAG_STANDARD)
        && desc.standard_allocation_type == D3DKMDT_STANDARDALLOCATION_GDISURFACE as u32
}

/// The KMD's total reading of a validated HWA2 descriptor.
///
/// `desc` MUST already have passed `validate_create_input`, which is what makes
/// every field read below meaningful: an image kind is known to carry nonzero
/// geometry and a known DXGI format, a non-image kind is known to carry none,
/// and `plane_count >= 1` is guaranteed for every image.
///
/// # Why the swizzle class alone is NOT the discriminator
///
/// It was, and that was wrong in the direction that matters. §10.3 offset 88
/// says whether a linear row layout exists; it does NOT say which of this
/// driver's three venus constructors reproduces the surface. Keying the OPTIMAL
/// arm on it alone sent every `helios_umd12.dll` committed texture into
/// `allocate_optimal_gdi_image_blob` — a 1-mip, 1-layer, BGRA-only
/// cross-context PRESENT alias — because `resource12.rs::hwa2_swizzle_class`
/// maps `TL_UNDEFINED` and `TL_64KB_TILE_UNDEFINED_SWIZZLE` onto
/// `OPAQUE_OPTIMAL`. That constructor then refused every DXGI format outside
/// {87, 88} as `AcBackFail`, i.e. "the host could not build it", when the truth
/// is that the KMD asked for the wrong image.
///
/// So the arms are selected by the fields that identify the PRODUCER — kind,
/// the `STANDARD` flag with its OS enum, and `PRIMARY | DISPLAYABLE` — and the
/// layout class is read only where it genuinely separates two shapes from the
/// same producer (the tiled GDI texture from the CPU-visible GDI staging
/// surface). Nothing reaches a permissive default: the one arm with no
/// constructor is refused by name, and the one downgrade is counted by name.
fn classify_hwa2(desc: &HeliosWddmAllocationDescV2) -> Result<Hwa2Backing, NTSTATUS> {
    // Spelled ONCE and reached from four arms, for [`CreatedBacking`]'s reason:
    // the defect class here is arms that drift. Constructed eagerly because it
    // has no side effects; only one arm can ever move it.
    let linear_memory = Hwa2Backing::LinearMemory {
        bytes: desc.byte_size,
        mappable: desc.has_flag(HELIOS_HWA2_FLAG_CPU_VISIBLE),
        shareable: desc.has_flag(HELIOS_HWA2_FLAG_SHARED),
    };
    match desc.allocation_kind {
        // The scan-out primary. Its bytes must be a real LINEAR VkImage the host
        // can bind to a virtio scan-out, not a plain buffer — that is what
        // `allocate_linear_scanout_image_blob` builds, and the display lane's
        // `venus_image_id != 0` branch in `submit_primary_scanout_copy` selects
        // on it.
        HELIOS_HWA2_KIND_STANDARD_PRIMARY => Ok(Hwa2Backing::LinearScanoutImage {
            width: desc.width,
            height: desc.height,
        }),
        // Everything else with texel geometry.
        kind if helios_hwa2_kind_is_image(kind) => {
            // A linear row layout exists ⇒ the surface IS its bytes, and a plain
            // venus memory blob of exactly `byte_size` reproduces it. Every
            // D3D11 non-primary texture, every D3D12 `TL_ROW_MAJOR` resource and
            // the STANDARD shadow/staging/GDI-staging surfaces land here.
            if desc.swizzle_class != HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL {
                return Ok(linear_memory);
            }
            // Tiled. The KMD has exactly ONE image constructor and exactly two
            // producers it is the right image for: the KMD's own GDI texture (a
            // shared tiled texture DWM samples) and the UMD's direct scan-out
            // primary (the OPTIMAL export the QEMU fork reconstructs natively).
            // Both are BGRA cross-context present aliases, which is what
            // `allocate_optimal_gdi_image_blob` builds.
            if hwa2_is_standard_gdi_texture(desc) || hwa2_is_direct_scanout_primary(desc) {
                return Ok(Hwa2Backing::OptimalImage {
                    width: desc.width,
                    height: desc.height,
                    dxgi_format: desc.dxgi_format,
                    ddi_bind_flags: hwa2_bind_to_ddi(desc.bind_flags),
                });
            }
            // A STANDARD shadow or staging surface in a tiled class. No producer
            // in this package authors one and no constructor builds one, so it is
            // refused by name rather than routed to an arm that would be wrong
            // either way. See [`CREATE_UNSUPPORTED_LAYOUT`] for why this must
            // read 0.
            if kind != HELIOS_HWA2_KIND_IMAGE {
                bump(&CREATE_UNSUPPORTED_LAYOUT, b"AcLayout");
                crate::diag::record(0x0C01_00E8);
                return Err(STATUS_NOT_SUPPORTED);
            }
            // An ordinary tiled UMD image — today, a D3D12 committed texture.
            // Backed by its exact extent in plain device memory, with the
            // downgrade COUNTED rather than refused: see
            // [`CREATE_OPTIMAL_AS_LINEAR`] for the full argument, including why
            // nothing may read it as an image before mesa unit A3.
            CREATE_OPTIMAL_AS_LINEAR.fetch_add(1, Ordering::Relaxed);
            Ok(linear_memory)
        }
        HELIOS_HWA2_KIND_BUFFER => Ok(linear_memory),
        // STUB: the C65 outer-command pool is the only paging object this
        // package defines, and it arrives as an HOC1 record on its own
        // (§10.6:1489-1494), not as an HWA2 with this kind. There is no second
        // paging object and therefore no backing constructor to call. Refused by
        // name so a future one is a visible counter rather than a wrong arm.
        HELIOS_HWA2_KIND_PAGING_OBJECT => {
            bump(&CREATE_UNSUPPORTED_KIND, b"AcKind");
            crate::diag::record(0x0C01_00E6);
            Err(STATUS_NOT_SUPPORTED)
        }
        // Unreachable: `validate_create_input` refuses `KIND_INVALID` and every
        // value above `KIND_MAX`. Refused rather than `unreachable!()` — a DDI
        // may not panic on a runtime value, and the counter proves the premise.
        _ => {
            bump(&CREATE_UNSUPPORTED_KIND, b"AcKind");
            crate::diag::record(0x0C01_00E6);
            Err(STATUS_NOT_SUPPORTED)
        }
    }
}

/// Produce the backing for one classified allocation.
///
/// Every diag code and every returned NTSTATUS here is byte-identical to the
/// pre-retirement arms — including `STATUS_NO_MEMORY` rather than
/// `STATUS_UNSUCCESSFUL`, because `0xC0000001` is not in
/// `DxgkDdiCreateAllocation`'s legal return set and dxgkrnl logged it as "Driver
/// returned an invalid NTSTATUS" (197x) and responded with adapter resets during
/// boot.
fn build_backing(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    backing: Hwa2Backing,
) -> Result<CreatedBacking, NTSTATUS> {
    match backing {
        Hwa2Backing::LinearScanoutImage { width, height } => {
            match adapter.with_venus_client(passive, |c| {
                c.allocate_linear_scanout_image_blob(adapter, width, height)
            }) {
                Ok(Ok(scanout)) => Ok(CreatedBacking {
                    resource_id: scanout.blob.res_id,
                    venus_memory_id: scanout.blob.blob_id,
                    venus_image_id: scanout.image_id.get(),
                    pitch: scanout.row_pitch,
                    plane_offset: scanout.plane_offset as u64,
                    venus_alloc_size: scanout.blob.size,
                    memory_type_index: scanout.memory_type_index,
                    blob_size: BackingSize::HostAuthoritative(scanout.blob.size),
                }),
                Ok(Err(_ve)) => {
                    crate::diag::record(0x0C01_00E5);
                    bump(&CREATE_BACKING_FAILED, b"AcBackFail");
                    Err(STATUS_NO_MEMORY)
                }
                Err(_de) => {
                    crate::diag::record(0x0C01_00E1);
                    bump(&CREATE_BACKING_FAILED, b"AcBackFail");
                    Err(STATUS_DEVICE_NOT_READY)
                }
            }
        }
        Hwa2Backing::OptimalImage {
            width,
            height,
            dxgi_format,
            ddi_bind_flags,
        } => {
            // A shared, non-CPU-visible tiled texture used as both a compositor
            // sample source and a DirectX render target. Preserve that contract
            // with a real cross-context OPTIMAL image: reinterpreting this
            // allocation as pitched host bytes makes DWM sample tiled memory as
            // a different resource shape and produces a black redirected window.
            match adapter.with_venus_client(passive, |client| {
                client.allocate_optimal_gdi_image_blob(
                    adapter,
                    width,
                    height,
                    ddi_bind_flags,
                    dxgi_format,
                )
            }) {
                Ok(Ok(image)) => Ok(CreatedBacking {
                    resource_id: image.blob.res_id,
                    venus_memory_id: image.blob.blob_id,
                    venus_image_id: image.image_id.get(),
                    // No row layout: this is a tiled image, not a byte buffer.
                    pitch: 0,
                    plane_offset: 0,
                    venus_alloc_size: image.blob.size,
                    memory_type_index: image.memory_type_index,
                    blob_size: BackingSize::HostAuthoritative(image.blob.size),
                }),
                Ok(Err(_ve)) => {
                    // The constructor refuses width/height 0 and every DXGI
                    // format outside {87, 88}, so this arm also covers "the KMD
                    // has no OPTIMAL constructor for that format". Both are a
                    // refusal, never a substituted format.
                    crate::diag::record_named_bytes(b"GdiOImg", 0xE1);
                    bump(&CREATE_BACKING_FAILED, b"AcBackFail");
                    Err(STATUS_NO_MEMORY)
                }
                Err(_de) => {
                    crate::diag::record_named_bytes(b"GdiOImg", 0xE2);
                    bump(&CREATE_BACKING_FAILED, b"AcBackFail");
                    Err(STATUS_DEVICE_NOT_READY)
                }
            }
        }
        Hwa2Backing::LinearMemory {
            bytes,
            mappable,
            shareable,
        } => {
            // Back it with a REAL venus `VkDeviceMemory` blob through the kernel
            // venus client: user-mode venus contexts import it by resource id and
            // `vkBindImageMemory2` against it — a raw `blob_id = 0` shmem blob
            // has no memory object behind it, and that bind poisons the
            // importer's venus ring (host: "failed to look up object of type 8"
            // -> fatal decoder state -> context destroyed). `allocate_memory_blob`
            // also registers the blob in the tracking table (owner 0), which the
            // GDI executor's `blob_kernel_range` resolves. PASSIVE flow under the
            // venus mutex (never the DISPATCH spinlock).
            match adapter.with_venus_client(passive, |c| {
                c.allocate_memory_blob(adapter, bytes, mappable, shareable)
                    .map(|b| (b, c.memory_type_index()))
            }) {
                Ok(Ok((blob, kernel_mti))) => Ok(CreatedBacking {
                    resource_id: blob.res_id,
                    venus_memory_id: blob.blob_id,
                    venus_image_id: 0,
                    // A plain memory blob has no row layout of its own; the
                    // descriptor's plane record is the authority and the caller
                    // reads it from there.
                    pitch: 0,
                    plane_offset: 0,
                    // The EXACT venus allocation parameters, so cross-process
                    // openers import with the creator's size + memory type.
                    venus_alloc_size: blob.size,
                    memory_type_index: kernel_mti,
                    // NOT `blob.size`: this arm has always charged VidMm the
                    // REQUESTED size. Still HostAuthoritative — the host
                    // allocated exactly this request and reported back its
                    // page-rounded form, and this arm is part of the
                    // `venus_memory_id != 0` set `bar_eligible` used to test, so
                    // the eligible population is unchanged.
                    blob_size: BackingSize::HostAuthoritative(bytes),
                }),
                Ok(Err(_ve)) => {
                    crate::diag::record(0x0C01_00E3);
                    bump(&CREATE_BACKING_FAILED, b"AcBackFail");
                    Err(STATUS_NO_MEMORY)
                }
                Err(_de) => {
                    crate::diag::record(0x0C01_00E1);
                    bump(&CREATE_BACKING_FAILED, b"AcBackFail");
                    Err(STATUS_DEVICE_NOT_READY)
                }
            }
        }
    }
}

/// The shape of the `pfnAllocateCb` / `D3DKMTCreateAllocation` call itself, as
/// opposed to the record inside it.
///
/// It exists because HVM1 (§10.7:1964-1972) and HOC1 (§10.6:1489-1494) both fix
/// the OUTER call, not just the private bytes, and §18.1:4750-4751 tests those
/// properties. The doc states them as what the ICD/UMD *does*; the KMD enforces
/// them, because a guest-supplied shape checked only by the guest is not checked
/// (CLAUDE.md: validate every runtime-supplied value before acting on it).
#[derive(Clone, Copy)]
struct CreateCallShape {
    /// `DXGK_CREATEALLOCATIONFLAGS::Value` — "outer flags zero" for HVM1/HOC1.
    /// The binding defines exactly one named bit (`Resource`) plus `Reserved`,
    /// so this is the whole word.
    flags: u32,
    /// Whether `DXGKARG_CREATEALLOCATION::hResource` is non-null — i.e. whether
    /// a runtime resource handle exists at all. Both records require none.
    has_resource_handle: bool,
    /// `DXGKARG_CREATEALLOCATION::NumAllocations`.
    num_allocations: u32,
    /// `DXGKARG_CREATEALLOCATION::PrivateDriverDataSize` — the RESOURCE-level
    /// private data. Both records require zero of it.
    resource_private_size: u32,
}

impl CreateCallShape {
    /// The exact shape §10.7:1964-1972 / §10.6:1489-1494 require: no resource
    /// handle, no resource-level private bytes, exactly one allocation, outer
    /// flags zero.
    fn is_bare_single_allocation(self) -> bool {
        self.flags == 0
            && !self.has_resource_handle
            && self.num_allocations == 1
            && self.resource_private_size == 0
    }
}

/// The result of admitting one allocation: what to place, what to publish, and
/// what to remember.
///
/// Built once per record arm and consumed once by [`create_one`]'s tail, which
/// is what stops the three arms from each open-coding the `DXGK_ALLOCATIONINFO`
/// write-out and drifting apart.
struct AdmittedAllocation {
    kind: u32,
    generation: u64,
    /// The exact extent charged to VidMm, page-rounded.
    vidmm_size: SIZE_T,
    placement: VidMmPlacement,
    /// `None` for a record whose allocation has no host backing at all (HOC1).
    backing: Option<CreatedBacking>,
    /// Geometry the KMD keeps for `DxgkDdiDescribeAllocation`, the scan-out
    /// path, and the prepared-copy setup. Zero for a non-image allocation.
    width: u32,
    height: u32,
    d3d_ddi_format: u32,
    dxgi_format: u32,
    ddi_bind_flags: u32,
    /// Byte offset and stride of plane 0, from the descriptor's plane record for
    /// HWA2 and from the created backing for the LINEAR scan-out arm (whose true
    /// stride is Vulkan's, not a function of width).
    plane_offset: u64,
    pitch: u32,
    /// The allocation is a UMD-created primary the display path may bind
    /// directly through `SET_SCANOUT_BLOB` without an intermediate copy.
    /// [`hwa2_is_direct_scanout_primary`] is the derivation — from the
    /// descriptor's flags, never from geometry and never from the layout class
    /// alone.
    direct_scanout: bool,
    bar_eligible: bool,
    size_provenance: BackingSize,
}

/// Admit one HWA2 create-input record, create its backing, and stamp the
/// create-output record back into the `[in/out]` buffer.
///
/// # The write-back is the create's last act, deliberately
///
/// §10.3:1040-1041 makes the KMD the sole writer and only at create. The record
/// is therefore written AFTER the backing exists and the generation is minted,
/// so a failed create leaves the input bytes untouched rather than a
/// half-stamped descriptor a later opener could validate.
///
/// # SAFETY
/// `private` is the runtime's `[in/out]` per-allocation buffer and
/// `private_size` is its authoritative length; both come straight from
/// `DXGK_ALLOCATIONINFO`.
unsafe fn admit_hwa2(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    private: *mut u8,
    private_size: usize,
    shape: CreateCallShape,
) -> Result<AdmittedAllocation, NTSTATUS> {
    // SAFETY: `private_size` is the runtime's authoritative length for this
    // buffer and the slice is bounded by the record size we are about to
    // require; `from_private_data` owns the exact-length check and refuses any
    // other length, so a shorter buffer never reaches a read of 168 bytes.
    if private_size != HELIOS_HWA2_BYTES as usize {
        bump(&CREATE_HWA2_REJECT, b"AcHwa2Rej");
        crate::diag::record(0x0C01_0002);
        return Err(STATUS_INVALID_PARAMETER);
    }
    // ⚠ THE READ SLICE LIVES IN THIS BLOCK AND NOWHERE ELSE. The create-output
    // write at the end of this function forms a `&mut [u8]` over the identical
    // address range, and holding a live `&[u8]` across it is aliasing UB under
    // every model that gives `&mut` uniqueness — even though borrowck cannot see
    // it, because both slices are raw-pointer-derived. `from_private_data`
    // returns the record BY VALUE, so nothing needs the read slice afterwards.
    let parsed = {
        // SAFETY: length checked exactly above; the runtime guarantees the buffer.
        let bytes = unsafe { core::slice::from_raw_parts(private as *const u8, private_size) };
        HeliosWddmAllocationDescV2::from_private_data(bytes)
    };
    let Ok(mut desc) = parsed else {
        bump(&CREATE_HWA2_REJECT, b"AcHwa2Rej");
        crate::diag::record(0x0C01_0003);
        return Err(STATUS_INVALID_PARAMETER);
    };
    if desc
        .validate_create_input(HELIOS_PACKAGE_GENERATION)
        .is_err()
    {
        // §10.3:1079-1080 — "any malformed, unknown, truncated,
        // mismatched-generation, or reserved-nonzero descriptor makes
        // create/open fail; it never selects a legacy parser." There is
        // deliberately no fallback arm below this line.
        bump(&CREATE_HWA2_REJECT, b"AcHwa2Rej");
        crate::diag::record(0x0C01_0003);
        return Err(STATUS_INVALID_PARAMETER);
    }

    // ── cross-checks the descriptor alone cannot express ────────────────────
    //
    // A `STANDARD_PRIMARY` kind without the `PRIMARY` flag is legal to
    // `validate` (the standard coupling only binds the STANDARD flag to the
    // offset-84 type) but is not legal here: every later placement decision
    // reads the FLAG, so a primary-kind allocation without it would be placed as
    // an ordinary texture — CpuVisible-with-Cached, no AccessedPhysically, no
    // aperture fallback — and the VidPn commit would fail with nothing naming
    // why.
    let is_primary = desc.has_flag(HELIOS_HWA2_FLAG_PRIMARY);
    if desc.allocation_kind == HELIOS_HWA2_KIND_STANDARD_PRIMARY && !is_primary {
        bump(&CREATE_HWA2_REJECT, b"AcHwa2Rej");
        crate::diag::record(0x0C01_0003);
        return Err(STATUS_INVALID_PARAMETER);
    }
    // §10.3 offset 68: `RESOURCE_ASSOCIATED` is "preserved verbatim so later
    // diagnostics never have to infer it from dimensions, process, or order".
    //
    // ⚠ COUNTED, NOT REFUSED, and the asymmetry is forced rather than chosen.
    // The pre-retirement code OR-ed the bit in from `DXGK_CREATEALLOCATIONFLAGS
    // ::Resource` at create — i.e. the KMD *set* it. Under the echo rule the KMD
    // may not: `K4-CONTRACT.md` §1.1 puts this field in the "echoed verbatim"
    // row and says the KMD refuses rather than corrects. But for an OS standard
    // allocation the author is `dxgkddi_get_standard_allocation_driver_data`,
    // which is called BEFORE dxgkrnl decides whether the create carries a
    // resource and therefore cannot know the answer — so refusing a mismatch
    // would fail every DWM primary, and correcting one is forbidden. There is no
    // producer that can be right, which is a gap in the field's definition
    // rather than in either implementation. Recorded as a divergence so it is
    // visible and measurable instead of silently absorbed.
    if desc.has_flag(HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED)
        != (shape.flags & DXGK_CREATEALLOCATION_FLAG_RESOURCE != 0)
    {
        bump(&CREATE_RESOURCE_ASSOC_DIVERGENCE, b"AcRcAssoc");
    }

    // ── `byte_size` for a KMD-created LINEAR image (`K4-CONTRACT.md` §1.3) ───
    //
    // §10.3 gives no rule for computing it, so the existing empirical
    // computation stays AUTHORITATIVE and the descriptor is validated against it
    // rather than replacing it. It applies to exactly the surfaces
    // `dxgkddi_get_standard_allocation_driver_data` authors below with
    // `linear_blob_size`: a STANDARD allocation with a linear row layout. An
    // ordinary UMD buffer or image is not covered — the doc gives no rule for
    // those either, and inventing one would refuse legal creates.
    // `HELIOS_HWA2_FLAG_STANDARD` is NOT in `HELIOS_HWA2_FLAG_KMD_OWNED_MASK`, so
    // any producer can set it and authorship is unprovable here — the flag means
    // only "claims to be an OS standard allocation". What makes the claim
    // harmless is that a claimant must then produce the exact `byte_size` this
    // driver would have authored, so BOTH swizzle classes are pinned to
    // `dxgkddi_get_standard_allocation_driver_data`'s own arithmetic.
    // `validate_stage` already refused INVALID and > MAX, so these are exhaustive.
    let plane0 = desc.planes[0];
    if desc.has_flag(HELIOS_HWA2_FLAG_STANDARD) {
        let expected = if desc.swizzle_class == HELIOS_HWA2_SWIZZLE_LINEAR {
            linear_blob_size(plane0.row_pitch as u64, desc.height as u64)
        } else {
            // OPAQUE_OPTIMAL GDI texture: notional pitch, sized from it so the
            // declared plane fits.
            round_up_page(
                (plane0.row_pitch as u64)
                    .saturating_mul(desc.height as u64)
                    .max(PAGE as u64),
            )
        };
        if desc.byte_size != expected {
            bump(&CREATE_SIZE_REJECT, b"AcSize");
            crate::diag::record(0x0C01_00E7);
            return Err(STATUS_INVALID_PARAMETER);
        }
    }

    let backing_class = classify_hwa2(&desc)?;
    let created = build_backing(passive, adapter, backing_class)?;

    // The extent the AUTHOR claimed, captured before the adoption below can move
    // it. Every "estimate versus host answer" measurement must compare against
    // this rather than against `desc.byte_size`, which the KMD-authored LINEAR
    // arm overwrites forty lines down — comparing after the overwrite makes the
    // comparison an identity and the counter a constant.
    let authored_byte_size = desc.byte_size;
    // NOT "the KMD authored this" — a claim, pinned by the equality check above.
    let claims_standard = desc.has_flag(HELIOS_HWA2_FLAG_STANDARD);

    // MEASURE THE GUESS (R719). `authored_byte_size` is what the authoring side
    // computed before any backing existed; `created.venus_alloc_size` is the
    // exact Vulkan requirement the host reported. Counted, never acted on — this
    // measures how far off the KMD's empirical pre-create estimate is.
    //
    // ⛔ SCOPED to `claims_standard`, and round 3 of the Phase-2 review is why.
    // Unscoped, it compared the host's PAGE-ROUNDED blob size against the UMD's
    // resource extent — two quantities `K4-CONTRACT.md` §1.3 requires to differ,
    // since `allocate_memory_blob` does `round_up_page(size.max(4096))`. It
    // therefore fired on essentially every allocation whose extent is not a
    // multiple of 4096, i.e. most D3D11 and D3D12 textures, and a reader
    // following §1.3 would have read a page-alignment census as evidence that
    // `NV_LINEAR_ROW_ALIGN`/`NV_LINEAR_TAIL_SLACK` were catastrophically wrong.
    // The question the counter exists to answer — "how far is OUR estimate from
    // the host's real requirement" — only has a meaning where WE authored the
    // estimate. Both KMD-authored arms are in scope; what differs between them
    // is what the code then DOES, which is the block below.
    if claims_standard && created.venus_alloc_size != 0 && created.venus_alloc_size != authored_byte_size
    {
        LINEAR_BLOB_SIZE_DIVERGENCE.fetch_add(1, Ordering::Relaxed);
    }

    // ── the KMD's OWN estimate is not evidence about the host ────────────────
    //
    // ⛔ A STANDARD descriptor is authored by `dxgkddi_get_standard_allocation_
    // driver_data` BEFORE any backing exists, so its `byte_size` and plane
    // record are an ESTIMATE — `linear_blob_size`, which §1.3 and its own doc
    // call "deliberately LARGER" (128-row round-up plus a 64 KiB tail slack).
    // The `LinearScanoutImage` arm is the one backing whose size the HOST
    // decides, and the host's answer is smaller: measured on the live desktop,
    // `SdgLReq=7910400 SdgLPch=7680` gives `venus_alloc_size = 7_913_472`
    // against `linear_blob_size(7680,1030) = 8_912_896`.
    //
    // Round 2 of the Phase-2 review found that refusing on that comparison
    // rejects EVERY OS shared primary — no primary, no composited desktop —
    // and that the pre-retirement code did the opposite: it OVERWROTE the guess
    // (`41d13f8:create_allocation.rs:2427`, `ap.size = created.blob_size.bytes()`)
    // and only counted the divergence.
    //
    // So the KMD adopts the host's answer for the descriptor it authored itself.
    // This is not "correcting a UMD's field" — §1.1's echo rule protects a claim
    // the UMD made, and on this arm there is no UMD: the KMD wrote the input and
    // is replacing its own pre-create estimate with the measured extent. The
    // published descriptor is then TRUE, which is what §10.3's "exact backing
    // extent" asks for and what an opener bounding planes against it needs.
    //
    // The stride moves with it for the same reason, and the code already knew
    // this one: the returned `Pitch` below prefers `created.pitch` because "a
    // `width*4`-derived stride shears the scan-out (1896*4 = 7584 against the
    // real 7680)". Publishing that same wrong stride inside the descriptor while
    // handing the runtime the right one would make the two disagree.
    //
    // ⛔⛔ THE ADOPTION IS A PAIR — size AND plane record, or NEITHER. Round 3 of
    // the Phase-2 review found this, from three independent lenses, and it is
    // round 2's own repair overreaching by one arm.
    //
    // `HELIOS_HWA2_FLAG_STANDARD` is true for BOTH surfaces this DDI authors:
    // the LINEAR scan-out primary and the OPAQUE_OPTIMAL GDI texture. The
    // OPTIMAL arm returns `pitch: 0` by construction ("No row layout: this is a
    // tiled image, not a byte buffer"), so gating only the plane half on
    // `created.pitch != 0` moved `byte_size` to the host's TILED requirement
    // while leaving `planes[0]` holding the author's 256-byte-aligned LINEAR
    // estimate — two numbers with no defined relationship. Whenever the tiled
    // requirement came in under `cross_adapter_pitch(width) * height`, which is
    // the whole `width < 64` band and much else besides, the KMD published a
    // descriptor that fails its OWN `validate_create_output` at
    // `PlaneRangeExceedsByteSize`, bumped `AcHwa2Out` — documented "**Must read
    // 0** — a nonzero value is a driver bug" — and refused a legal GDI-surface
    // create. That is DWM's redirected-window texture.
    //
    // `K4-CONTRACT.md` §1.3 Tier 1 step 2 says the KMD overwrites `byte_size`
    // "— and the plane record's offset/pitch —" with "the measured extent and
    // Vulkan's real stride". Where there is no real stride to adopt there is no
    // adoption: the pair cannot be completed, so it is not begun.
    //
    // ⇒ What the OPTIMAL arm keeps instead, and why that is correct rather than
    // merely safe. Its descriptor is self-consistent as authored
    // (`dxgkddi_get_standard_allocation_driver_data` sizes `byte_size` FROM the
    // notional stride precisely so the plane fits), and §10.3 forbids a zero
    // `row_pitch` on a declared plane, so "describe the tiling honestly" is not
    // expressible in this record — the author says so at its own plane site: the
    // runtime is told "no byte addressing exists" (`pitch` resolves to 0 below)
    // and the descriptor carries the notional span, with `swizzle_class` telling
    // a reader which interpretation applies. Nothing byte-addresses an
    // `OPAQUE_OPTIMAL` allocation: `pitch` is 0, `bar_eligible` excludes the
    // class outright, and VidMm is charged `max(backing, byte_size)` below. The
    // backing IS the image's own `vkGetImageMemoryRequirements` answer, so it is
    // exactly right for the image — this is not the Xid-31 undersize shape,
    // which is a blob smaller than the requirement for the SAME layout.
    let adopt_host_extent =
        claims_standard && created.blob_size.is_host_authoritative() && created.pitch != 0;
    if adopt_host_extent {
        desc.byte_size = created.blob_size.bytes();
        desc.planes[0].offset = created.plane_offset;
        desc.planes[0].row_pitch = created.pitch;
        desc.planes[0].slice_pitch = created
            .pitch
            .saturating_mul(desc.height)
            .min(u32::try_from(desc.byte_size.saturating_sub(created.plane_offset)).unwrap_or(u32::MAX));
    }

    // ⛔ ACTED ON: an UNDERSIZED backing, for every descriptor whose extent this
    // driver did NOT author. §10.3:1050 makes `byte_size` bound every plane, and
    // a UMD's descriptor is echoed verbatim, so admitting a backing smaller than
    // it would publish a descriptor whose planes run off the end of the real
    // allocation. A blob smaller than the image requirement binds "successfully"
    // and then MMU-faults when the sampler reads the slack region (host Xid 31,
    // FAULT_PTE VIRT_READ — killed the IDD feed live 2026-07-04).
    //
    // ⚠ The `!claims_standard` exclusion is safe ONLY because the equality check
    // above pins `byte_size` on both STANDARD arms. Not `!adopt_host_extent`:
    // applying this to the OPAQUE_OPTIMAL arm compares a notional LINEAR estimate
    // against a TILED requirement and refuses legal GDI creates (round 3).
    // Narrowing that check means widening this guard in the same commit.
    if !claims_standard && created.venus_alloc_size != 0 && created.venus_alloc_size < desc.byte_size
    {
        bump(&CREATE_SIZE_REJECT, b"AcSize");
        crate::diag::record(0x0C01_00E7);
        // The backing is destroyed by the caller's unwind through
        // `destroy_allocation_ctx` only if an `AllocationContext` exists, and it
        // does not yet — so tear it down here rather than leak a host resource.
        release_orphan_backing(passive, adapter, &created);
        return Err(STATUS_INVALID_PARAMETER);
    }

    let Some(generation) = allocation_object::mint() else {
        bump(&CREATE_GENERATION_EXHAUSTED, b"AcGenExh");
        release_orphan_backing(passive, adapter, &created);
        // `STATUS_NO_MEMORY`, not `STATUS_INSUFFICIENT_RESOURCES`: the former is
        // in this DDI's documented return set and the latter is not, and
        // dxgkrnl logs an illegal NTSTATUS as a driver bug ("Driver returned an
        // invalid NTSTATUS", 197x) and answers with adapter resets.
        return Err(STATUS_NO_MEMORY);
    };

    // ── the create-output record ────────────────────────────────────────────
    //
    // Every field is ECHOED; exactly three are stamped. `K4-CONTRACT.md` §1.1:
    // "the KMD refuses the create rather than correcting a field", because a
    // silent correction would make the descriptor disagree with the resource the
    // UMD believes it made.
    desc.allocation_generation = generation;
    // §10.3:1071-1073 / C44 — set ONLY when the D3D12 create record, the runtime
    // PRIMARY flag, and the required `D3DDDI_ID_UNINITIALIZED` sentinel all
    // agree. The KMD sees the last two directly; the first is what produced the
    // sentinel in the first place, since the D3D12 runtime is the only thing
    // that overwrites `VidPnSourceId` with it on a primary. The bit and the
    // sentinel are set together, here, so no opener has to infer either.
    if is_primary && desc.vidpn_source == D3DDDI_ID_UNINITIALIZED {
        desc.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
    }
    // §10.3:1074-1076 — set ONLY for a non-protected, non-cross-adapter managed
    // primary in a swizzle class the selected display backend implements. This
    // backend scans out a LINEAR image through `SET_SCANOUT_BLOB`, which is
    // exactly what `helios_hwa2_swizzle_is_direct_flip_capable` names.
    //
    // ⚠ Necessary, never sufficient (§10.3:1076-1078): the D3D11 UMD must still
    // compare both live wrappers and every C43 field, and KMD `CheckMPO3` must
    // still accept the actual display attributes. STEREO is excluded here even
    // though `validate` does not require it, because §10.8:2740-2748 makes the
    // display path refuse it — promoting a flip the display path will refuse is
    // a wrong claim, not a harmless one.
    //
    // ⭐ CONSEQUENCE, decided rather than stumbled into: a UMD **direct scan-out**
    // primary can NEVER earn this bit, because `helios_hwa2_swizzle_is_direct_
    // flip_capable` is LINEAR-only and that primary is `OPAQUE_OPTIMAL` by
    // construction. That is CORRECT, and the two questions are genuinely
    // different ones:
    //
    //   * `DIRECT_FLIP_COMPATIBLE` is a WIRE claim an opener reads — "dxgkrnl and
    //     DWM may Direct-Flip this allocation" — and `protocol/src/wddm.rs` rules
    //     on the class outright: `OPAQUE_OPTIMAL` is "legal for ordinary
    //     rendering; never Direct-Flip eligible in this generation".
    //   * [`AdmittedAllocation::direct_scanout`] is a KMD-PRIVATE routing
    //     decision — "`SetVidPnSourceAddress` binds this allocation's own host
    //     resource instead of copying into the adapter's LINEAR target" — and it
    //     works on `OPAQUE_OPTIMAL` precisely because the QEMU fork reconstructs
    //     that native layout (`qemu-helios`, native OPTIMAL readback).
    //
    // ⇒ The bit is earned by the OS standard primary, which is LINEAR + PRIMARY +
    // DISPLAYABLE + one plane. Widening the predicate so the tiled primary could
    // claim it would contradict the protocol's own class ruling and promote a
    // flip `CheckMPO3` and the display backend would then refuse.
    if is_primary
        && desc.has_flag(HELIOS_HWA2_FLAG_DISPLAYABLE)
        && !desc.has_flag(HELIOS_HWA2_FLAG_PROTECTED)
        && !desc.has_flag(HELIOS_HWA2_FLAG_CROSS_ADAPTER)
        && !desc.has_flag(HELIOS_HWA2_FLAG_STEREO)
        && desc.plane_count == 1
        && helios_hwa2_swizzle_is_direct_flip_capable(desc.swizzle_class)
    {
        desc.flags |= HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
    }

    // Self-check BEFORE the write: the record this KMD is about to publish must
    // pass the validator every opener will run on it. A failure here is a driver
    // bug, not a guest one, and refusing the create is the only way it can be
    // noticed at all — a published descriptor that fails open-time validation
    // would present as an unexplained `OpenResource` failure much later.
    if desc
        .validate_create_output(HELIOS_PACKAGE_GENERATION)
        .is_err()
    {
        bump(&CREATE_HWA2_OUTPUT_REJECT, b"AcHwa2Out");
        release_orphan_backing(passive, adapter, &created);
        return Err(STATUS_INVALID_PARAMETER);
    }

    // SAFETY: `private_size == HELIOS_HWA2_BYTES` was proven above and the
    // runtime owns a writable buffer of that length for the DDI call's duration.
    // `bytes_of` yields exactly `size_of::<HeliosWddmAllocationDescV2>()` bytes,
    // which the assertion in `protocol` pins at 168.
    let out = unsafe { core::slice::from_raw_parts_mut(private, private_size) };
    out.copy_from_slice(bytes_of(&desc));
    crate::diag::record(0x0C3B_0000 | (created.resource_id & 0xFFFF));

    // The pitch a byte-addressing consumer must use. For the LINEAR scan-out arm
    // it is VULKAN'S stride, not the descriptor's: `allocate_linear_scanout_image_blob`
    // reports what the host actually laid out, and a `width*4`-derived stride
    // shears the scan-out (1896*4 = 7584 against the real 7680). For every other
    // arm the descriptor's plane record is the authority.
    let pitch = if created.pitch != 0 {
        created.pitch
    } else {
        plane0.row_pitch
    };
    let plane_offset = if created.pitch != 0 {
        created.plane_offset
    } else {
        plane0.offset
    };

    let bar_seg_id = adapter.bar_segment().map(|b| b.seg_id);
    // HostAuthoritative is exactly the three KMD-created arms, which are exactly
    // the arms that set `venus_memory_id`. What that buys is that the aperture
    // path's safety rests on a stated fact rather than on an incidental property
    // of an unrelated field.
    //
    // ⛔ THE THIRD TERM IS LOAD-BEARING AND IS NEW: **BAR eligibility requires a
    // MAPPABLE blob**, and since K4 that is no longer true by construction.
    // Round 3 of the Phase-2 review found the disagreement.
    //
    // Before K4, `create_one` passed `mappable = true` unconditionally
    // (`d1c820a:create_allocation.rs:2214`), so every host-authoritative blob
    // could be mapped and this predicate did not need to care. `classify_hwa2`
    // now derives `mappable` from `HELIOS_HWA2_FLAG_CPU_VISIBLE`, and
    // `allocate_memory_blob` omits `VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE` when it
    // is false. Meanwhile this predicate had no CPU-visibility term at all — so
    // a D3D12 `D3D12_HEAP_TYPE_DEFAULT` resource (`CPUPageProperty ==
    // CPU_NOT_AVAILABLE` ⇒ the flag clear ⇒ a non-mappable blob) was published
    // BAR-eligible, `vidmm_placement` PREFERRED it into the BAR, and the paging
    // engine then issued `VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB` against a blob the
    // host was never asked to make mappable — `build_paging_buffer.rs`'s
    // not-eligible arm says in terms that the eligible arm is the one that maps.
    // `BAR_ERR_MAP` would then name the symptom and not the cause.
    //
    // The term also states the thing the placement is FOR: the BAR exposes CPU
    // access through the CpuHostAperture, so an allocation with no CPU view has
    // no reason to prefer it. The excluded population falls back to the aperture
    // segment, which is where every other non-eligible allocation already goes.
    //
    // ⚠ The live D3D11 desktop cannot reach the broken arm today — the D3D11
    // producer sets `HELIOS_HWA2_FLAG_CPU_VISIBLE` unconditionally — and `umd12`
    // is behind the default-OFF `UmdD3D12` knob. This is fixed before its first
    // boot rather than after, which is the point of reviewing before flipping.
    //
    // The `OPAQUE_OPTIMAL` exclusion is the same invariant from the other side:
    // `scanout.rs` records that the OPTIMAL GDI image "is deliberately not
    // mappable". Both terms now say one thing — BAR ⇒ mappable.
    let bar_eligible = created.blob_size.is_host_authoritative()
        && desc.swizzle_class != HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL
        && desc.has_flag(HELIOS_HWA2_FLAG_CPU_VISIBLE)
        && bar_seg_id.is_some();
    let placement = vidmm_placement(
        bar_eligible,
        bar_seg_id,
        is_primary,
        desc.has_flag(HELIOS_HWA2_FLAG_CPU_VISIBLE),
        adapter.display_half(),
    );

    // VidMm is charged the LARGER of the descriptor's extent and the backing the
    // host actually produced. The descriptor is const and may under-state a
    // host-rounded image requirement; charging the smaller number would let
    // `MapCpuHostAperture` compute a page count below the tracked blob's length.
    let charged = created.blob_size.bytes().max(desc.byte_size);
    let vidmm_size = round_up_page(if charged == 0 {
        PAGE
    } else {
        charged as SIZE_T
    });

    Ok(AdmittedAllocation {
        kind: desc.allocation_kind,
        generation,
        vidmm_size,
        placement,
        // ⭐ THE SAME predicate `classify_hwa2` routed the backing with, so the
        // allocation registered in `SCANOUT_ALLOCS` is exactly the allocation
        // that was given a bindable OPTIMAL image. It reads the producer's
        // `DISPLAYABLE` claim; see [`hwa2_is_direct_scanout_primary`] for what
        // the two producers mean by it and for the inversion this replaces.
        direct_scanout: hwa2_is_direct_scanout_primary(&desc),
        width: desc.width,
        height: desc.height,
        d3d_ddi_format: desc.d3d_ddi_format,
        dxgi_format: desc.dxgi_format,
        ddi_bind_flags: hwa2_bind_to_ddi(desc.bind_flags),
        plane_offset,
        pitch,
        bar_eligible,
        size_provenance: created.blob_size,
        backing: Some(created),
    })
}

/// Admit one HVM1 create-input record (§10.7), create its renderer-view backing,
/// and stamp the three write-back fields.
///
/// # ⚠⚠ NO PRODUCER EXISTS — this function has never been called and cannot be
///
/// Measured 2026-08-10 (round 3 of the Phase-2 review, re-verified here):
/// `HELIOS_HVM1_MAGIC` and `HeliosVenusMemoryAllocationV1` appear outside
/// `protocol/` in exactly two places — this file, the consumer, and
/// `kmd_logic`, the model. **No UMD, no ICD and no tool ever builds one.** The
/// ICD's only mentions are prose comments in `vn_renderer_helios.c` saying the
/// KMD *will* own the venus allocation after mesa unit **A3**; that unit is the
/// producer, and it does not exist. `create_one`'s magic dispatch therefore
/// never reaches here.
///
/// ⛔ **What that means for the counters, which is the trap:** `AcHvm1Rej`,
/// `AcHvm1Mem`, `AcHvm1Out`, `AcSegRole1`..`AcSegRole4` and (for the sibling)
/// `AcHoc1Rej`, `AcHoc1Out`, `AcSegHoc1` are all **absent** from the service
/// key, and their absence is evidence of **nothing**. In particular it is not
/// evidence that `HELIOS_SEGMENT_ID_HLM1` is in the reported segment table —
/// the natural reading of "no role was refused for a missing segment". Nothing
/// asked. `K4-CONTRACT.md` §4's amended ruling ("check the segment, do not
/// hardcode the role") is written as though role-1..3 creates arrive and are
/// refused; none can arrive.
///
/// ⛔ STALE AS OF 2026-08-11. `tools/hts1_session_probe.c:292-297` builds an
/// HVM1 role-1 record and `TsPoolBind` moved 2 -> 3 on the target across one run
/// of it (`FINDINGS.md` F14), so this arm IS exercised. What remains true is that
/// no SHIPPING component produces one — the producer is a probe.
///
/// # SAFETY
/// As [`admit_hwa2`].
unsafe fn admit_hvm1(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    private: *mut u8,
    private_size: usize,
    shape: CreateCallShape,
) -> Result<AdmittedAllocation, NTSTATUS> {
    if private_size != HELIOS_HVM1_SIZE as usize {
        bump_with_code(&CREATE_HVM1_REJECT, b"AcHvm1Rej", 0);
        return Err(STATUS_INVALID_PARAMETER);
    }
    // Scoped for the reason `admit_hwa2` states in full: the write-back below
    // forms a `&mut [u8]` over the same bytes.
    let parsed = {
        // SAFETY: length checked exactly above; the runtime owns the buffer.
        let bytes = unsafe { core::slice::from_raw_parts(private as *const u8, private_size) };
        HeliosVenusMemoryAllocationV1::from_private_data(bytes)
    };
    let mut record = match parsed {
        Ok(record) => record,
        Err(reject) => {
            bump_with_code(&CREATE_HVM1_REJECT, b"AcHvm1Rej", reject.code());
            return Err(STATUS_INVALID_PARAMETER);
        }
    };
    let role = match record.validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateInput) {
        Ok(role) => role,
        Err(reject) => {
            bump_with_code(&CREATE_HVM1_REJECT, b"AcHvm1Rej", reject.code());
            return Err(STATUS_INVALID_PARAMETER);
        }
    };

    // §10.7:1964-1972 — the KMT create-call shape is fixed and the KMD enforces
    // it. The doc states it as what the ICD does; §18.1:4750-4751 tests that the
    // properties hold, and a guest-supplied shape checked only by the guest is
    // not checked.
    //
    // ⚠ The `pSystemMem = NULL` / `ExistingSysMem` / `CreateShared` /
    // `NtSecuritySharing` / `ExistingKernelSysMem` / `ExistingSection` /
    // `PermanentSysMem` half of that list is a `D3DDDI_ALLOCATIONINFO` /
    // `D3DKMT_CREATEALLOCATION` property that dxgkrnl consumes and does NOT
    // surface to the miniport: `DXGKARG_CREATEALLOCATION` carries only
    // `Flags{Resource, Reserved}`, `NumAllocations`, `hResource`, and the two
    // private-data pairs (grep `_DXGKARG_CREATEALLOCATION`). So this check
    // covers the four properties the KMD can see and CANNOT cover the other
    // seven — recorded here rather than implied, because §18.1:4750-4751 asks
    // for a proof this DDI cannot supply.
    if !shape.is_bare_single_allocation() {
        bump(&CREATE_CALL_SHAPE, b"AcShape");
        return Err(STATUS_INVALID_PARAMETER);
    }

    // ⛔ THE ROLE'S PREFERRED SEGMENT MUST BE ONE THIS DRIVER ACTUALLY REPORTS
    // (`K4-CONTRACT.md` §4, as AMENDED 2026-08-10 — the amendment overrides the
    // role-4-only refusal that stood here).
    //
    // `Hvm1Role::placement()` prefers `HELIOS_SEGMENT_ID_HLM1` for EVERY role, so
    // the refusal that keyed on role 4 caught the one create it could name and
    // let roles 1-3 through onto the same unreportable segment — where dxgkrnl
    // refuses them outside this driver, with nothing here counting it. The check
    // is the segment, not the role: it is correct whether or not K2 has landed,
    // it needs no knowledge of K2's schedule, and it cannot go stale. See
    // [`CREATE_ROLE1_SEGMENT_ABSENT`] for the full argument and for why no
    // substitute placement is admissible (role 4 in particular has
    // `CpuVisible = 0`, no CPU VA, and may never reach Lock2 — §10.7:2004-2005,
    // :2049-2050).
    //
    // Checked BEFORE `build_backing` so a refusal has no host resource to orphan.
    let knobs = adapter.knobs();
    let placement = hvm1_placement(role, knobs.hlm1_only, knobs.hlm1_flags_off);
    if !segment_is_reported(adapter, placement.preferred_segment) {
        let (counter, name) = role_segment_absent_counter(role);
        bump(counter, name);
        // `STATUS_NOT_SUPPORTED`, unchanged from the role-4 refusal this
        // replaces: the record is well-formed and legal, and it is this adapter's
        // current segment topology that cannot host it. `STATUS_INVALID_PARAMETER`
        // would blame the caller for a KMD/host sequencing state.
        return Err(STATUS_NOT_SUPPORTED);
    }

    // ⛔ THE KMD CANNOT ALLOCATE DEVICE-LOCAL MEMORY, so a role that asks for it
    // is REFUSED rather than quietly given host-visible memory.
    //
    // Found by round 3 of the Phase-2 review as "a validated-then-discarded
    // field". `Hvm1Role::VulkanDeviceLocal` (role 4) validates with
    // `cache_policy = HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE` and its
    // `placement()` publishes `cpu_visible = false, lockable = false`; its own
    // doc says "**Rejects map and has no CPU VA**". But `allocate_memory_blob`
    // allocates from `self.memory_type_index` unconditionally — the ONE
    // host-visible/host-coherent type chosen at bring-up — because a plain
    // memory blob has no `vkGet*MemoryRequirements` query to feed
    // `choose_device_local_memory_type`, which is the only way this client picks
    // a type (the scan-out image path is the one caller that has such a query).
    // So role 4 would have been backed by mappable host-visible memory while
    // dxgkrnl was told the allocation has no CPU view: the record's `role`,
    // `cache_policy` and `access` validated and then contradicted by the only
    // code that acts on them. That is fake success, which CLAUDE.md rule 2
    // forbids ahead of a loud failure.
    //
    // ⚠ This is a CAPABILITY statement, not a schedule claim, and it must not be
    // confused with the segment check above: `K4-CONTRACT.md` §4 forbids
    // hardcoding a role number as a stand-in for "K2 has not landed", and rightly
    // — that check is `segment_is_reported`, evaluated at runtime. This one says
    // something the code can state truthfully today and that no other lane's
    // schedule can change: this venus client has no way to request a device-local
    // Vulkan memory type. Giving it one is mesa unit A3 plus K6 work; when it
    // lands, delete this arm rather than widening it.
    //
    // Costs nothing today — nothing in this package produces an HVM1 record at
    // all (see this function's banner) — which is precisely why it is written
    // before the first producer exists rather than after.
    let placement_rules = role.placement();
    if !placement_rules.cpu_visible {
        // `to_u32`, not `as u32`: the counter's low word must be the WIRE role
        // number a reader can look up in §10.7, not this enum's declaration
        // order, which is one lower.
        bump_with_code(
            &CREATE_HVM1_MEMORY_CLASS_REFUSED,
            b"AcHvm1Mem",
            role.to_u32(),
        );
        return Err(STATUS_NOT_SUPPORTED);
    }

    // §10.7:1936-1940 — one ordinary, nonprimary, unshared WDDM allocation and
    // one KMD-owned renderer resource descriptor of the same page-rounded size.
    // The blob is MAPPABLE exactly when the role's own placement says the
    // allocation has a CPU view — derived, not asserted, so the blob flag and
    // the WDDM `CpuVisible` flag can never disagree. (With the refusal above
    // that is every admitted role, i.e. 1-3; it is written as a derivation so
    // that widening the refusal cannot silently leave a non-CPU-visible role
    // with a `USE_MAPPABLE` blob.) It is never SHAREABLE, because
    // §10.7:1971-1972 requires every sharing flag zero.
    let created = build_backing(
        passive,
        adapter,
        Hwa2Backing::LinearMemory {
            bytes: record.byte_size,
            mappable: placement_rules.cpu_visible,
            shareable: false,
        },
    )?;
    if created.venus_alloc_size != 0 && created.venus_alloc_size < record.byte_size {
        bump(&CREATE_SIZE_REJECT, b"AcSize");
        release_orphan_backing(passive, adapter, &created);
        return Err(STATUS_INVALID_PARAMETER);
    }

    let Some(generation) = allocation_object::mint() else {
        bump(&CREATE_GENERATION_EXHAUSTED, b"AcGenExh");
        release_orphan_backing(passive, adapter, &created);
        // `STATUS_NO_MEMORY`, not `STATUS_INSUFFICIENT_RESOURCES`: the former is
        // in this DDI's documented return set and the latter is not, and
        // dxgkrnl logs an illegal NTSTATUS as a driver bug ("Driver returned an
        // invalid NTSTATUS", 197x) and answers with adapter resets.
        return Err(STATUS_NO_MEMORY);
    };

    // The three write-back fields (§10.7:1955, :1960, :1961), and nothing else.
    // ⚠ MEASURED 2026-08-11: dxgkrnl DISCARDS this write — no caller and no later
    // DDI ever sees it (`tools/hwa2_writeback_probe.c`). [`stamp_open_hvm1`] is
    // what actually publishes the create-output; this stays because the record
    // must still be a valid create-output before the allocation is admitted, and
    // because an OS that did propagate it would then agree with the open.
    record.object_generation = generation;
    record.segment_page_shift = HELIOS_HVM1_SEGMENT_PAGE_SHIFT;
    // §10.7:1961 — "the KMD returns the exact alignment". Every backing this
    // driver creates is page-granular (`allocate_memory_blob` rounds up to
    // `PAGE`), and `validate` requires a nonzero power of two.
    record.allocation_alignment = PAGE as u64;
    if record
        .validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateOutput)
        .is_err()
    {
        bump(&CREATE_HVM1_OUTPUT_REJECT, b"AcHvm1Out");
        release_orphan_backing(passive, adapter, &created);
        return Err(STATUS_INVALID_PARAMETER);
    }
    // SAFETY: length proven exactly `HELIOS_HVM1_SIZE` above; the runtime owns a
    // writable buffer of that length for the call's duration.
    let out = unsafe { core::slice::from_raw_parts_mut(private, private_size) };
    out.copy_from_slice(bytes_of(&record));

    Ok(AdmittedAllocation {
        kind: ALLOC_KIND_HVM1,
        generation,
        vidmm_size: round_up_page(created.blob_size.bytes().max(record.byte_size) as SIZE_T),
        // The same value the segment check above interrogated — computed once so
        // the placement that was validated is the placement that ships.
        placement,
        width: 0,
        height: 0,
        d3d_ddi_format: 0,
        dxgi_format: 0,
        ddi_bind_flags: 0,
        plane_offset: 0,
        pitch: 0,
        direct_scanout: false,
        // ⛔ NOT BAR-eligible by default. An HVM1 object's placement is HLM1 plus
        // the ordinary aperture (§10.7:1999-2002); the CpuHostAperture BAR path is
        // the mechanism §17.6 deletes, and routing native-Vulkan memory through
        // it would re-create exactly what the retirement removes.
        //
        // ⚠ `Hlm1Bar` opens it anyway, because F16 measured that no HLM1
        // configuration produces an aliased CPU view and this is the one path on
        // this driver that demonstrably does. Same three terms as the D3D11
        // predicate: a host-authoritative blob, a CPU view, and a reported BAR
        // segment (the last is implied by the `segment_is_reported` check above,
        // which already returned for a table without HLM1).
        bar_eligible: knobs.hlm1_bar
            && created.blob_size.is_host_authoritative()
            && placement_rules.cpu_visible,
        size_provenance: created.blob_size,
        backing: Some(created),
    })
}

/// Admit the one HOC1 outer-command pool for a D3D12 device (§10.6).
///
/// # ⚠⚠ NO PRODUCER EXISTS — as [`admit_hvm1`], and for the same measurement
///
/// `HELIOS_HOC1_MAGIC` and `HeliosOuterCommandAllocationV1` appear outside
/// `protocol/` only in this file and in `kmd_logic`. `helios_umd12.dll` does not
/// build one: its D3D12 submit path does not use the HOB1/HOS1 GPUVA pool at
/// all — see `FINDINGS.md` F5's "option zero", where vkd3d translates to Vulkan
/// and needs no D3D12 GPUVA semantics *from the KMD*, and the D3D12 lane reached
/// device + queues + command lists + DXIL PSOs on the existing submit path.
/// So `AcHoc1Rej`, `AcHoc1Out` and `AcSegHoc1` are absent and their absence
/// attributes nothing. Read [`admit_hvm1`]'s banner for the full argument.
///
/// # SAFETY
/// As [`admit_hwa2`].
unsafe fn admit_hoc1(
    adapter: &AdapterContext,
    private: *mut u8,
    private_size: usize,
    shape: CreateCallShape,
) -> Result<AdmittedAllocation, NTSTATUS> {
    if private_size != HELIOS_HOC1_BYTES as usize {
        bump(&CREATE_HOC1_REJECT, b"AcHoc1Rej");
        return Err(STATUS_INVALID_PARAMETER);
    }
    // Scoped for the reason `admit_hwa2` states in full: the write-back below
    // forms a `&mut [u8]` over the same bytes.
    let parsed = {
        // SAFETY: length checked exactly above; the runtime owns the buffer.
        let bytes = unsafe { core::slice::from_raw_parts(private as *const u8, private_size) };
        HeliosOuterCommandAllocationV1::from_private_data(bytes)
    };
    let Ok(mut record) = parsed else {
        bump(&CREATE_HOC1_REJECT, b"AcHoc1Rej");
        return Err(STATUS_INVALID_PARAMETER);
    };
    // §17.6:4419-4420 — "every other size/flag/cache/node/role combination fails
    // allocation". `validate_create_input` is that rule: exactly 64 MiB, 64-KiB
    // extent alignment, `CPU_WRITE|DEVICE_READ`, write-combined, node bit 0,
    // reserved zero, and a zero generation awaiting this KMD's write-back.
    if record
        .validate_create_input(HELIOS_PACKAGE_GENERATION)
        .is_err()
    {
        bump(&CREATE_HOC1_REJECT, b"AcHoc1Rej");
        return Err(STATUS_INVALID_PARAMETER);
    }
    // §10.6:1489-1494 / §17.6:4414-4415 — admit ONLY from
    // `pfnAllocateCb{hResource = NULL}` with one zeroed legacy
    // `D3DDDI_ALLOCATIONINFO` and no resource-level private data.
    if !shape.is_bare_single_allocation() {
        bump(&CREATE_CALL_SHAPE, b"AcShape");
        return Err(STATUS_INVALID_PARAMETER);
    }
    // The same segment check `admit_hvm1` performs, for the same reason: this
    // placement also prefers `HELIOS_SEGMENT_ID_HLM1`, so without it an HOC1
    // create is refused by dxgkrnl with nothing in the guest naming why. See
    // [`CREATE_HOC1_SEGMENT_ABSENT`], which records that this goes one step
    // beyond §4's letter and how to revert it.
    let placement = hoc1_placement();
    if !segment_is_reported(adapter, placement.preferred_segment) {
        bump(&CREATE_HOC1_SEGMENT_ABSENT, b"AcSegHoc1");
        return Err(STATUS_NOT_SUPPORTED);
    }

    let Some(generation) = allocation_object::mint() else {
        bump(&CREATE_GENERATION_EXHAUSTED, b"AcGenExh");
        // See the same site in `admit_hwa2` for why this is `STATUS_NO_MEMORY`.
        return Err(STATUS_NO_MEMORY);
    };
    // §17.6:4418-4419 — "KMD writes ONE allocation generation into HOC1 and lets
    // C64/HPM1 map its pages for virtual execution".
    record.allocation_generation = generation;
    if record
        .validate_create_output(HELIOS_PACKAGE_GENERATION)
        .is_err()
    {
        bump(&CREATE_HOC1_OUTPUT_REJECT, b"AcHoc1Out");
        return Err(STATUS_INVALID_PARAMETER);
    }
    // SAFETY: length proven exactly `HELIOS_HOC1_BYTES` above.
    let out = unsafe { core::slice::from_raw_parts_mut(private, private_size) };
    out.copy_from_slice(bytes_of(&record));

    Ok(AdmittedAllocation {
        kind: ALLOC_KIND_HOC1,
        generation,
        vidmm_size: round_up_page(record.byte_size as SIZE_T),
        // Validated by the segment check above; computed once so the placement
        // that was validated is the placement that ships.
        placement,
        // ⛔ NO BACKING, and that is the contract, not an omission
        // (§10.6:1511-1512, §17.6:4416-4417): the pool is "not an HVM1 renderer
        // resource", has "no renderer view", and its bytes are ordinary VidMm
        // memory the UMD Lock2-maps and C64/HPM1 page-tables for execution.
        // Creating a venus object here would give it exactly the renderer view
        // the doc forbids.
        backing: None,
        width: 0,
        height: 0,
        d3d_ddi_format: 0,
        dxgi_format: 0,
        ddi_bind_flags: 0,
        plane_offset: 0,
        pitch: 0,
        direct_scanout: false,
        bar_eligible: false,
        size_provenance: BackingSize::NonHostAuthoritative(record.byte_size),
    })
}

/// Tear down a backing created for an allocation that then failed admission.
///
/// The ordinary teardown path is `destroy_allocation_ctx`, which needs an
/// `AllocationContext`; between `build_backing` succeeding and the `Box` being
/// leaked into `info.hAllocation` there is none, so a refusal in that window
/// would leak a host resource with no handle able to reclaim it. Best-effort on
/// every step, exactly like the teardown path: a refusal must not get stuck.
fn release_orphan_backing(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    created: &CreatedBacking,
) {
    if created.resource_id == 0 {
        return;
    }
    let _ = crate::virtio::ctrl::forget_allocation_blob(passive, adapter, created.resource_id);
    let first_teardown = adapter
        .with_virtio(|v| v.take_live_resource(created.resource_id))
        .unwrap_or(false);
    if first_teardown {
        // Detach before unref, exactly as `destroy_allocation_ctx` does: the
        // blob was ctx-attached by `resource_create_blob` at creation, and
        // unref'ing an attached resource is the shape that produced QEMU's
        // "virgl_cmd_resource_unref: resource does not exist" burst.
        let _ = crate::virtio::ctrl::ctx_detach_resource(
            passive,
            adapter,
            adapter.venus_ctx_id(),
            created.resource_id,
        );
        let _ = crate::virtio::ctrl::resource_unref(passive, adapter, created.resource_id);
    }
    if created.venus_image_id != 0 {
        let _ = adapter.with_venus_client(passive, |c| {
            c.destroy_image(adapter, created.venus_image_id)
        });
    }
    if created.venus_memory_id != 0 {
        let _ = adapter.with_venus_client(passive, |c| {
            c.free_memory_blob(adapter, created.venus_memory_id)
        });
    }
}

/// `DXGK_CREATEALLOCATIONFLAGS::Resource`, as a mask over the flags word.
///
/// The binding exposes the bit only through an accessor; the mask is needed here
/// to state "outer flags zero" and the RESOURCE_ASSOCIATED cross-check over the
/// same word, so it is named once with its provenance rather than spelled `1`.
const DXGK_CREATEALLOCATION_FLAG_RESOURCE: u32 = 1 << 0;

/// Create one allocation: parse the record, create the backing, stamp the
/// create-output descriptor, and fill the `DXGK_ALLOCATIONINFO`.
///
/// On failure nothing is stored and no byte of the private buffer is written
/// (the caller unwinds prior allocations).
///
/// # The three-record dispatch
///
/// One `pfnAllocateCb` per-allocation buffer may hold exactly one of HWA2 (168
/// B), HVM1 (64 B) or HOC1 (64 B). They are told apart by the 4-byte magic at
/// offset 0, read through a BOUNDED copy before any record-sized read, and each
/// arm then requires its own exact length. That is per-arm validation: nothing
/// reads 168 bytes out of a buffer that only claimed 64, and nothing takes the
/// max-union of the three sizes as a bound.
unsafe fn create_one(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    info: &mut DXGK_ALLOCATIONINFO,
    shape: CreateCallShape,
) -> Result<(), NTSTATUS> {
    let private = info.pPrivateDriverData as *mut u8;
    let private_size = info.PrivateDriverDataSize as usize;
    crate::diag::record(0x0C30_0000 | ((info.PrivateDriverDataSize as u32).min(0xFFFF)));
    crate::diag::record(0x0C31_0000 | ((shape.resource_private_size as u32).min(0xFFFF)));

    // ⛔ The RESOURCE-level buffer is NOT a fallback source any more.
    //
    // The pre-retirement read fell back to it when it was the larger of the two,
    // with a breadcrumb recording that the fallback was believed unreachable.
    // Under §10.3 the descriptor is per-allocation by construction — dxgkrnl
    // associates one HWA2 with one allocation and carries it to `OpenResource` —
    // so a resource-level read would be reading another allocation's identity.
    // HVM1 and HOC1 forbid resource-level private data outright.
    if private.is_null() || private_size < size_of::<u32>() {
        bump(&CREATE_UNKNOWN_RECORD, b"AcMagic");
        crate::diag::record(0x0C01_0002);
        return Err(STATUS_INVALID_PARAMETER);
    }
    // Bounded 4-byte read to pick the arm. Copied rather than referenced: the
    // buffer carries no alignment promise, and forming a `&u32` over it would
    // assert one. Nothing wider is read until an arm has required its own exact
    // length.
    let mut magic_bytes = [0u8; 4];
    // SAFETY: `private_size >= 4` was proven above and the runtime guarantees
    // `private_size` readable bytes at `private`.
    unsafe {
        core::ptr::copy_nonoverlapping(private as *const u8, magic_bytes.as_mut_ptr(), 4);
    }
    let magic = u32::from_le_bytes(magic_bytes);
    crate::diag::record(0x0C11_0000 | (magic & 0xFFFF));

    let admitted = match magic {
        HELIOS_HWA2_MAGIC => unsafe { admit_hwa2(passive, adapter, private, private_size, shape) }?,
        HELIOS_HVM1_MAGIC => unsafe { admit_hvm1(passive, adapter, private, private_size, shape) }?,
        HELIOS_HOC1_MAGIC => unsafe { admit_hoc1(adapter, private, private_size, shape) }?,
        _ => {
            // §10.3:1104-1107 — a descriptor that fails makes the create fail;
            // NOTHING selects a legacy parser. The retired
            // `HeliosWddmAllocPrivate` is one of the parsers this must not
            // select, and a UMD still sending it lands here by construction:
            // its `magic` sits at byte 16, so byte 0 is its `size` field and
            // will not match any of the three above.
            bump(&CREATE_UNKNOWN_RECORD, b"AcMagic");
            crate::diag::record(0x0C01_0003);
            return Err(STATUS_INVALID_PARAMETER);
        }
    };

    let backing = admitted.backing.as_ref();
    let resource_id = backing.map_or(0, |b| b.resource_id);
    crate::diag::record(0x0C01_0020);
    crate::diag::record(resource_id);
    record_alloc_event(
        resource_id,
        admitted.width,
        admitted.height,
        adapter.venus_ctx_id(),
        false,
    );

    // Derived from the placement that was admitted, not from the record kind: the
    // predicate that decided this allocation has a CPU view is the one that
    // decides its blob must be mapped where VidMm puts it.
    //
    // The excluded population is DWM's D3D11 textures: `vidmm_placement` gives
    // every BAR-eligible one `cpu_visible` on this same segment, and without a
    // term for it the counters are dominated by them and attribute nothing to an
    // HVM1 allocation.
    //
    // ⛔ That term used to be `!bar_eligible`, which was a PROXY for "not a D3D11
    // surface" and stopped being one the moment `Hlm1Bar` could make an HVM1
    // allocation BAR-eligible — the instrument would have gone dark in exactly the
    // arm it was built to measure. The allocation KIND says the same thing
    // directly and cannot be turned off by a knob.
    let hlm1_eligible = admitted.placement.cpu_visible
        && admitted.placement.preferred_segment == HELIOS_SEGMENT_ID_HLM1
        && matches!(admitted.kind, ALLOC_KIND_HVM1 | ALLOC_KIND_HOC1);

    let ctx = Box::new(AllocationContext {
        magic: ALLOCATION_CTX_MAGIC,
        ctx_id: adapter.venus_ctx_id(),
        resource_id,
        generation: admitted.generation,
        kind: admitted.kind,
        venus_memory_id: backing.map_or(0, |b| b.venus_memory_id),
        venus_image_id: backing.map_or(0, |b| b.venus_image_id),
        scanout_copy_image_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_memory_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_conversion_image_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_conversion_memory_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_conversion_init_pool_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_pool_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_command_buffer_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_target_image_id: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_last_fence: core::sync::atomic::AtomicU64::new(0),
        scanout_copy_owns_source_alias: AtomicU32::new(0),
        scanout_copy_orphaned: AtomicU32::new(0),
        vidpn_primary_address: AtomicU64::new(0),
        vidpn_primary_segment: AtomicU32::new(0),
        vidpn_primary_flags: AtomicU32::new(0),
        vidpn_present_epoch: AtomicU64::new(helios_kmd_logic::scanout_lease::NO_LEASE),
        vidpn_frame_watermark: AtomicU64::new(0),
        vidpn_snap_resid: AtomicU32::new(0),
        vidpn_snap_width: AtomicU32::new(0),
        vidpn_snap_height: AtomicU32::new(0),
        vidpn_snap_pitch: AtomicU32::new(0),
        vidpn_snap_dxgi_format: AtomicU32::new(0),
        vidpn_snap_plane_offset: AtomicU32::new(0),
        vidpn_snap_alloc_size: AtomicU64::new(0),
        size: admitted.vidmm_size,
        width: admitted.width,
        height: admitted.height,
        format: admitted.d3d_ddi_format,
        bind_flags: admitted.ddi_bind_flags,
        pitch: admitted.pitch,
        dxgi_format: admitted.dxgi_format,
        direct_scanout: admitted.direct_scanout,
        plane_offset: admitted.plane_offset,
        venus_alloc_size: backing.map_or(0, |b| b.venus_alloc_size),
        memory_type_index: backing.map_or(0, |b| b.memory_type_index),
        bar_placed: core::sync::atomic::AtomicU64::new(BAR_UNPLACED),
        hlm1_eligible,
        hlm1_bound: core::sync::atomic::AtomicU64::new(BAR_UNPLACED),
        bar_eligible: admitted.bar_eligible,
        size_provenance: admitted.size_provenance,
    });

    if hlm1_eligible {
        crate::ddi::build_paging_buffer::hlm1_note_eligible();
    }

    // ── VidMm metadata: segment placement + CPU visibility ──────────────────
    let is_direct_scanout = ctx.direct_scanout;
    let ctx_resource_id = ctx.resource_id;
    info.hAllocation = Box::into_raw(ctx) as HANDLE;
    // Register AFTER the Box is leaked, so the pointer published here is the
    // one dxgkrnl will hand back.
    if is_direct_scanout {
        register_scanout_allocation(ctx_resource_id, info.hAllocation as usize);
    }
    info.Size = admitted.vidmm_size;
    info.PitchAlignedSize = admitted.vidmm_size;
    let placement = &admitted.placement;
    info.SupportedWriteSegmentSet = placement.supported_segments;
    info.EvictionSegmentSet = 0;
    info.HintedBank.__bindgen_anon_1.Value = 0;
    // §10.7:1999-2001 — normal priority and physical-adapter index 0 for HVM1;
    // unchanged from the pre-retirement value for everything else, which is the
    // same constant.
    info.AllocationPriority = D3DDDI_ALLOCATIONPRIORITY_NORMAL;
    info.pAllocationUsageHint = core::ptr::null_mut();
    unsafe {
        info.__bindgen_anon_1.Alignment = PAGE as UINT;
        info.PreferredSegment
            .__bindgen_anon_1
            .__bindgen_anon_1
            .set_SegmentId0(placement.preferred_segment);
        info.__bindgen_anon_2.SupportedReadSegmentSet = placement.supported_segments;
        info.__bindgen_anon_3.MaximumRenamingListLength = 0;
        info.__bindgen_anon_3.PhysicalAdapterIndex = 0;
        info.__bindgen_anon_4
            .FlagsWddm2
            .__bindgen_anon_1
            .__bindgen_anon_1
            .set_CpuVisible(u32::from(placement.cpu_visible));
        // A D3DDDI primary is selected by the display engine using the physical
        // address delivered in SetVidPnSourceAddress. Tell VidMm that exact
        // access model so it allocates the primary contiguously in a
        // GPU-addressable segment rather than at a non-identifiable implicit
        // system-memory address.
        if placement.accessed_physically {
            info.__bindgen_anon_4
                .FlagsWddm2
                .__bindgen_anon_1
                .__bindgen_anon_1
                .set_AccessedPhysically(1);
        }
        // ⚠ FIRST-TIME FIELD WRITE, and reported as such: `grep -rn
        // ExplicitResidencyNotification kmd_render/src/` was empty before this
        // change. §10.7:2002-2003 requires it on every HVM1 role and
        // §18.1:4751-4753 gates it. It is set only where the doc requires it —
        // `vidmm_placement` leaves it false — so the proven D3D11 surface is
        // byte-identical and the new bit rides only on the HVM1 path.
        //
        // ⛔ CORRECTED after round 3: this comment used to attribute the bit's
        // unreachability to `admit_hvm1`'s segment check, "for as long as the
        // reported table lacks the role's preferred segment" — a condition that
        // would LIFT, and which implies HVM1 creates arrive and are refused.
        // They do not arrive at all: **nothing in this package produces an HVM1
        // record**, so `admit_hvm1` has never been called (its banner has the
        // measurement). Two consequences a reader needs: the bit is unreachable
        // for a reason no other lane's schedule changes, and it stays unreachable
        // after K2 lands — the producer is mesa unit A3, not K2. And the segment
        // check is *additionally* wrong to lean on here: measured at HEAD,
        // `HELIOS_SEGMENT_ID_HLM1` is the constant 2 and `SegmentTable::iter`
        // numbers positionally from 1, so the production `[Aperture, Bar]` table
        // **already reports id 2** and `segment_is_reported` returns true.
        //
        // The A/B disable is `hvm1_placement`'s
        // `explicit_residency_notification` field (CLAUDE.md rule 8: the
        // opposite value stays reachable).
        if placement.explicit_residency_notification {
            info.__bindgen_anon_4
                .FlagsWddm2
                .__bindgen_anon_1
                .__bindgen_anon_1
                .set_ExplicitResidencyNotification(1);
        }
        // WB-cacheable CPU views: without `Cached`, dxgkrnl maps user views of
        // these allocations write-combined; WC READS of the BAR window measured
        // ~200 MB/s (36 ms per 7.8 MiB IDD readback frame, 2026-07-06). The BAR
        // is RAM-backed host shmem — cache-coherent on x86 for every agent on
        // the same physical pages, and the host reports the venus blobs CACHED
        // (blob_map honors the same hint for kernel maps). Service-key
        // `AllocCached=0` is the kill switch (read at StartDevice).
        if adapter.alloc_cached() && placement.cached {
            info.__bindgen_anon_4
                .FlagsWddm2
                .__bindgen_anon_1
                .__bindgen_anon_1
                .set_Cached(1);
        }
        // ⚠ TWO MORE FIRST-TIME FIELD WRITES, into a union member this driver
        // has never touched: `grep -rn Flags2 kmd_render/src/` was empty.
        // §10.7:2005-2007 requires both on every HVM1 role; §18.1:4752-4753
        // gates them. `Flags2` is a WDDM-3.2 field and
        // `ddi/wddm_surface.rs` still declares `Wddm2_1GpuMmu`, so these bits
        // are INERT until the single activation commit that flips the surface
        // (`docs/retirement/OWNERSHIP.md` §3 makes that the last edit of the
        // whole retirement). That is correct, not a bug to "fix" — do not
        // conclude from an unchanged `PgUn`/residency trace that the writes are
        // not happening.
        //
        // ⛔ AND THERE IS A SECOND, STRONGER REASON, added after round 3, because
        // the paragraph above names only a condition that will lift and so reads
        // as "these go live at the SURFACE flip". They do not. Both bits ride on
        // `placement`, which on this path is `hvm1_placement`'s — and **nothing
        // in this package produces an HVM1 record**, so `admit_hvm1` has never
        // run (see its banner). Flipping `SURFACE` alone will not exercise these
        // writes; the producer is mesa unit A3. `K4-CONTRACT.md` §8 obligation 8
        // says "treat as unexercised until measured on the target" — it is
        // stronger than that: it cannot be measured on the target at all until
        // A3 lands, and that is what the acceptance table must say.
        if placement.disable_partial_residency {
            info.Flags2
                .__bindgen_anon_1
                .__bindgen_anon_1
                .set_DisablePartialResidency(1);
        }
        if placement.restricted_to_single_segment {
            info.Flags2
                .__bindgen_anon_1
                .__bindgen_anon_1
                .set_RestrictedToSingleSegment(1);
        }
    }
    // Allocation creation is not a scanout-selection event. Modern DWM creates
    // and rotates multiple ManagedPrimary allocations; only
    // SetVidPnSourceAddress identifies the one Windows selected for this flip.
    crate::diag::record(0x0C12_0000 | ((admitted.vidmm_size >> 12).min(0xFFFF) as u32));
    crate::diag::record(
        0x0C13_0000
            | (((unsafe { info.__bindgen_anon_1.Alignment } as u32 >> 12) & 0xFF) << 8)
            | (info.PitchAlignedSize.min(0xFF) as u32),
    );
    crate::diag::record(
        0x0C14_0000
            | (((unsafe { info.__bindgen_anon_2.SupportedReadSegmentSet } as u32) & 0xFF) << 8)
            | (info.SupportedWriteSegmentSet & 0xFF),
    );
    crate::diag::record(
        0x0C15_0000
            | ((info.EvictionSegmentSet & 0xFF) << 8)
            | (unsafe { info.__bindgen_anon_4.FlagsWddm2.__bindgen_anon_1.Value } & 0xFF),
    );
    crate::diag::record(0x0C1A_0000 | (unsafe { info.Flags2.__bindgen_anon_1.Value } & 0xFFFF));
    crate::diag::record(0x0C19_0000 | ((info.AllocationPriority >> 16) & 0xFFFF));
    crate::diag::record(0x0C16_0000 | (admitted.width.min(0xFFFF)));
    crate::diag::record(0x0C17_0000 | (admitted.height.min(0xFFFF)));
    crate::diag::record(0x0C18_0000 | (admitted.d3d_ddi_format & 0xFFFF));
    bump(&CREATE_ADMITTED, b"AcOk");
    Ok(())
}

pub unsafe extern "C" fn dxgkddi_create_allocation(
    h_adapter: *mut c_void,
    create_allocation: *mut DXGKARG_CREATEALLOCATION,
) -> NTSTATUS {
    if h_adapter.is_null() || create_allocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    crate::diag::record(0x0C01_0001);
    // SAFETY: Dxgkrnl passes our adapter context and a valid args struct.
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    // SAFETY: `DxgkDdiCreateAllocation` is documented "IRQL: PASSIVE_LEVEL" (WDK
    // DXGKDDI_CREATEALLOCATION). It creates the host resources every arm below
    // round-trips the control queue for, so it cannot be anything else.
    let passive = unsafe { crate::irql::PassiveLevel::assume() };
    let args = unsafe { &mut *create_allocation };
    let create_flags = unsafe { args.Flags.__bindgen_anon_1.Value };
    let input_resource = args.hResource as usize as u64;
    // Per-create identity breadcrumbs, SAMPLED (R317): 5 here plus CAROutLo/Hi
    // and one CARAPSz per allocation — 8 synchronous registry writes per
    // CreateAllocation, for values that only change when the surface set does.
    let sample_create = crate::diag::sample_tick(&CREATE_BREADCRUMB_TICKS);
    if sample_create {
        crate::diag::record_named_bytes(b"CARFlg", create_flags);
        crate::diag::record_named_bytes(b"CARNum", args.NumAllocations);
        crate::diag::record_named_bytes(b"CARRSz", args.PrivateDriverDataSize);
        crate::diag::record_named_bytes(b"CARInLo", input_resource as u32);
        crate::diag::record_named_bytes(b"CARInHi", (input_resource >> 32) as u32);
    }
    crate::diag::record(0x0C10_0000 | ((args.NumAllocations as u32).min(0xFFFF)));
    crate::diag::record(0x0C33_0000 | ((args.PrivateDriverDataSize as u32).min(0xFFFF)));
    crate::diag::record(0x0C34_0000 | (unsafe { args.Flags.__bindgen_anon_1.Value } & 0xFFFF));
    if args.NumAllocations == 0 || args.pAllocationInfo.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    let wants_resource = unsafe { args.Flags.__bindgen_anon_1.__bindgen_anon_1.Resource() } != 0;
    // Mint a ResourceContext ONLY over a null input handle. A non-null
    // args.hResource is an add-allocation-to-existing-resource call: overwriting
    // it minted a second box for one resource and orphaned the first, since the
    // only free path is keyed on Flags.DestroyResource and sees just the last
    // handle. Evidence for the population: CARInLo/CARInHi read 0/0 on the live
    // box, so no in-tree caller takes the new arm today — RcIn is the counter
    // that proves it stays that way (k-alloc-V01).
    let minted_resource = wants_resource && args.hResource.is_null();
    if wants_resource {
        crate::diag::record(0x0C3C_0000 | ((args.hResource as usize as u32) & 0xFFFF));
        if minted_resource {
            let resource = Box::new(ResourceContext {
                _marker: RESOURCE_CTX_MARKER,
            });
            args.hResource = Box::into_raw(resource) as HANDLE;
            crate::diag::record(0x0C01_0030);
        } else {
            // Keep the runtime's handle. Validate it is ours; a foreign value is
            // counted and left untouched — never freed, never overwritten.
            let ours = unsafe { (*(args.hResource as *const ResourceContext))._marker }
                == RESOURCE_CTX_MARKER;
            let n = RESOURCE_INPUT_HANDLES.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 || n % 64 == 0 {
                crate::diag::record_named_bytes(b"RcIn", n);
            }
            if !ours {
                let m = RESOURCE_FOREIGN_HANDLES.fetch_add(1, Ordering::Relaxed) + 1;
                if m == 1 || m % 64 == 0 {
                    crate::diag::record_named_bytes(b"RcBad", m);
                }
            }
        }
        crate::diag::record(0x0C3D_0000 | ((args.hResource as usize as u32) & 0xFFFF));
    }
    let output_resource = args.hResource as usize as u64;
    if sample_create {
        crate::diag::record_named_bytes(b"CAROutLo", output_resource as u32);
        crate::diag::record_named_bytes(b"CAROutHi", (output_resource >> 32) as u32);
    }

    // The OUTER call's shape, captured once and passed to every allocation.
    // `hResource` is read BEFORE the mint above could have replaced it — hence
    // `input_resource`, not `args.hResource`: HVM1/HOC1 require "no runtime
    // resource handle", and a handle this DDI minted itself is not one the
    // caller supplied. Reading the post-mint value would make the check answer
    // the wrong question.
    let shape = CreateCallShape {
        flags: create_flags,
        has_resource_handle: input_resource != 0,
        num_allocations: args.NumAllocations,
        resource_private_size: args.PrivateDriverDataSize,
    };

    for i in 0..args.NumAllocations as usize {
        // SAFETY: pAllocationInfo points to NumAllocations elements.
        let info = unsafe { &mut *args.pAllocationInfo.add(i) };
        if sample_create {
            crate::diag::record_named_bytes(b"CARAPSz", info.PrivateDriverDataSize);
        }
        if let Err(status) = unsafe { create_one(passive, adapter, info, shape) } {
            // Unwind the allocations already created in this call, then the
            // ResourceContext this call minted — leaving it published over a
            // failed create handed dxgkrnl a handle to pool nothing would ever
            // free (the only free path runs on DestroyAllocation with
            // Flags.DestroyResource, which never arrives for a failed create).
            for j in 0..i {
                let prev = unsafe { &mut *args.pAllocationInfo.add(j) };
                if let Some(ctx) = unsafe { take_alloc_ctx(prev.hAllocation) } {
                    unsafe { destroy_allocation_ctx(passive, adapter, ctx) };
                }
                prev.hAllocation = core::ptr::null_mut();
            }
            if minted_resource {
                drop(unsafe { Box::from_raw(args.hResource as *mut ResourceContext) });
                args.hResource = input_resource as usize as HANDLE;
            }
            return status;
        }
    }

    // The PASSIVE dump site for the counters that must not mirror from their own
    // (success-path) sites — today just `OaNoRid`. `DxgkDdiCreateAllocation` is
    // documented PASSIVE_LEVEL, which `passive` above already asserts, and the
    // block's own throttle bounds this to one mirror per 64 creates. Placed on
    // the success tail only: a failed create returns early above, and the values
    // are cumulative atomics, so skipping it there costs mirror latency and never
    // a wrong number.
    dump_alloc_counters();
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_destroy_allocation(
    h_adapter: *mut c_void,
    destroy_allocation: *const DXGKARG_DESTROYALLOCATION,
) -> NTSTATUS {
    if h_adapter.is_null() || destroy_allocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    // SAFETY: `DxgkDdiDestroyAllocation` is documented "IRQL: PASSIVE_LEVEL" (WDK
    // DXGKDDI_DESTROYALLOCATION). Teardown unmaps/detaches/unrefs host
    // resources, all control round-trips.
    let passive = unsafe { crate::irql::PassiveLevel::assume() };
    let args = unsafe { &*destroy_allocation };
    if args.NumAllocations != 0 && args.pAllocationList.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    for i in 0..args.NumAllocations as usize {
        let handle = unsafe { *args.pAllocationList.add(i) };
        if let Some(ctx) = unsafe { take_alloc_ctx(handle) } {
            unsafe { destroy_allocation_ctx(passive, adapter, ctx) };
        }
    }

    let destroy_resource = unsafe {
        args.Flags
            .__bindgen_anon_1
            .__bindgen_anon_1
            .DestroyResource()
    } != 0;
    if destroy_resource && !args.hResource.is_null() {
        crate::diag::record(0x0C01_0031);
        // Magic-checked like the allocation handles: a foreign value is counted
        // and leaked rather than freed as if it were our pool.
        let ours =
            unsafe { (*(args.hResource as *const ResourceContext))._marker } == RESOURCE_CTX_MARKER;
        if ours {
            let _resource = unsafe { Box::from_raw(args.hResource as *mut ResourceContext) };
        } else {
            RECLAIM_BAD_HANDLE.fetch_add(1, Ordering::Relaxed);
        }
    }

    STATUS_SUCCESS
}

// ── Allocation lifetime DDIs. ───────────────────────────────────────────────

// ⛔ `unwind_opens` was here, and it is DELETED rather than kept for a future
// caller.
//
// It freed and nulled every `hDeviceSpecificAllocation` published by entries
// `0..i` when an open refused partway through the loop (k-alloc-09). There is no
// longer a refusal to unwind: the only two were C1's `resource_is_live` liveness
// gate and its transport-error sibling, and both died with the UMD-supplied
// resid they were gating on (see the DDI's own comment below). Every remaining
// step of the loop is infallible, so `DxgkDdiOpenAllocation` cannot return
// anything but `STATUS_SUCCESS` once it starts publishing handles.
//
// Keeping it as dead code would be assurance that is not real — an unwind path
// no failure can reach is untested by construction. If K6 reintroduces a refusal
// here (the allocation-generation check §18.1:4723-4726 puts in Render/Patch is
// the candidate), it must reintroduce the unwind WITH it, in the same commit, so
// the two are written and reviewed together.

/// `DxgkDdiOpenAllocation` — bind a device to allocations. dxgkrnl calls this for
/// EVERY allocation (including ones the same device just created via
/// `CreateAllocation`, not only cross-process opens), so it must succeed or
/// `D3DKMTCreateAllocation` fails with the open status. For each open-info entry
/// return a miniport-owned, device-specific tracking handle as required by the
/// DDI contract.
///
/// # ⛔ THIS DDI WRITES NO BYTE OF AN HWA2 OR HOC1 BUFFER — and exactly one of an HVM1
///
/// The no-restamp rule is the retirement's identity model (§10.3:1033-1034,
/// §18.1:4769, A.2 row 5694): two `write_open_identity` restamps used to live
/// here, one per entry and one call-level, and their existence is exactly why
/// two openers of one allocation could disagree about what they had. That rule
/// stands for HWA2 and HOC1, whose descriptors are `const` from the instant
/// `DxgkDdiCreateAllocation` returns.
///
/// ⛔ It could NOT stand for the HVM1 create-output, and the premise underneath
/// it — that a create-time write reaches the caller — was FALSIFIED on the
/// target on 2026-08-11: dxgkrnl discards a KMD write into
/// `DXGK_ALLOCATIONINFO::pPrivateDriverData` for any user-supplied buffer, so
/// there was no channel left. [`stamp_open_hvm1`] is that one licensed write and
/// carries the measurement; nothing else here may take a `*mut` to
/// `pPrivateDriverData`.
///
/// The thing the restamps were FOR — giving a UMD opener of a KMD-created
/// standard allocation something to alias the venus resource with — is not
/// solved by writing a different record here. It is solved structurally: HWA2 is
/// written once at create and dxgkrnl carries the identical bytes to
/// `OpenResource`.
///
/// # ⚠ And the identity it cannot rebuild: the A3 gap
///
/// See the module doc. HWA2 carries no host resource id, and
/// `DXGK_OPENALLOCATIONINFO::hAllocation` is dxgkrnl's runtime `D3DKMT_HANDLE`,
/// not this driver's `AllocationContext*`, so there is nothing here to resolve
/// one *from* — and §3:373-379 forbids adding an adapter-global table to bridge
/// it. So this DDI publishes a device-specific handle whose
/// [`PresentAllocInfo`] is `None`, counts `OaNoRid`, and lets the Present path
/// refuse at its own gates. It does NOT fabricate a zero resid into a
/// present-looking record; a `PresentAllocInfo { resource_id: 0, .. }` would be
/// acted on by `ddi/display.rs` and is the one outcome worse than refusing.
pub unsafe extern "C" fn dxgkddi_open_allocation(
    h_device: IN_CONST_HANDLE,
    open_allocation: IN_CONST_PDXGKARG_OPENALLOCATION,
) -> NTSTATUS {
    crate::diag::record(0x0C02_0003);
    if h_device.is_null() || open_allocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: hDevice is the DeviceContext we returned from DxgkDdiCreateDevice;
    // its adapter back-pointer is valid for the device's lifetime.
    //
    // This site used to dereference the back-pointer in ONE expression with no
    // null check at all, unlike its two siblings in scheduler.rs and
    // submit_command.rs. The checked traversal is now the only route.
    let Some(_adapter) =
        (unsafe { crate::device::DeviceHandleRef::from_raw(h_device) }).and_then(|d| d.adapter())
    else {
        return STATUS_INVALID_PARAMETER;
    };
    // SAFETY: valid per the DDI contract; `pOpenAllocation` is a `*mut` array of
    // `NumAllocations` entries whose `hDeviceSpecificAllocation` we fill.
    // The struct has output fields (`Pitch`, `SubresourceOffset`) despite the WDK
    // typedef being exposed through a const pointer in our bindings.
    //
    // ⚠ The `*mut` is for `hDeviceSpecificAllocation`, `SubresourceOffset` and
    // `Pitch` — the OUT fields the DDI defines — and for NOTHING else. In
    // particular it is not a licence to write `pPrivateDriverData`.
    let args = unsafe { &mut *(open_allocation as *mut DXGKARG_OPENALLOCATION) };
    if args.NumAllocations != 0 && args.pOpenAllocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // Which entry the call-level OUT fields describe. SubresourceIndex exists in
    // the binding and was never read; entry 0 is the fallback, which reproduces
    // today's value exactly for the single-entry opens this tree produces
    // (the UMD always sets NumAllocations = 1).
    let subresource_entry =
        (args.SubresourceIndex as usize).min((args.NumAllocations as usize).saturating_sub(1));
    if args.NumAllocations > 1 {
        let n = MULTI_ENTRY_OPENS.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 || n % 64 == 0 {
            crate::diag::record_named_bytes(b"OaMulti", n);
        }
    }
    let mut subresource_desc: Option<HeliosWddmAllocationDescV2> = None;
    for i in 0..args.NumAllocations as usize {
        let info = unsafe { &mut *args.pOpenAllocation.add(i) };
        crate::diag::record(0x0C21_0000 | ((info.PrivateDriverDataSize as u32).min(0xFFFF)));
        crate::diag::record(0x0C35_0000 | ((info.hAllocation as usize as u32) & 0xFFFF));

        // Read-only, and only from the PER-ALLOCATION buffer. The resource-level
        // fallback the pre-retirement path had is gone for the same reason the
        // create-time one is: §10.3 associates one descriptor with one
        // allocation, so reading the resource-level copy would be reading a
        // different allocation's identity.
        let desc =
            unsafe { read_open_descriptor(info.pPrivateDriverData, info.PrivateDriverDataSize) };

        // K5: stamp the HVM1 create-output and bind the role-1 reply pool to the
        // raw device's provisional HTS1 session. This DDI is the ONLY allocation
        // DDI that carries `hDevice` — `DXGKARG_CREATEALLOCATION` has none — so it
        // is the only place the binding §10.4:1205-1206 requires can happen, and
        // (measured, see `OPEN_HVM1_STAMPED`) the only place a create-output can
        // be published at all. A refusal never fails the open (see
        // `bind_reply_pool`).
        // ⛔ Role 1 ONLY for the BIND. A raw device also opens role-2/role-4
        // allocations in bulk once A3 lands, and offering those to the binder
        // would make `TsPoolRej` — a counter documented as an anomaly — climb on
        // the normal path. Every role is stamped; only role 1 is bound.
        // ⛔ The STAMP happens only on a create-flagged open. §17.6:4384 —
        // "`DxgkDdiOpenAllocation` only reads private data on an ORDINARY open" —
        // so the write is licensed exactly here, and
        // `DXGK_OPENALLOCATIONFLAGS::Create` ("if not set then allocation is
        // being opened", `d3dkmddi.h`) is the bit that says which open this is.
        // An ordinary open reads the record the creating open already published.
        let open_flags = unsafe { args.Flags.__bindgen_anon_1.Value };
        let hvm1 = if open_flags & DXGK_OPENALLOCATION_FLAG_CREATE != 0 {
            unsafe { stamp_open_hvm1(info.pPrivateDriverData, info.PrivateDriverDataSize) }
        } else {
            unsafe { read_open_hvm1(info.pPrivateDriverData, info.PrivateDriverDataSize) }
        };
        // What the guest was TOLD, for K6 to check its use records against.
        // HWA2's generation comes from the descriptor for the same reason: it is
        // the value the opener reads out of the identical bytes.
        let open_identity = match (hvm1, desc) {
            (Some((_, byte_size, generation)), _) => Some(OpenIdentity {
                generation,
                kind: ALLOC_KIND_HVM1,
                byte_size,
            }),
            (None, Some(d)) => Some(OpenIdentity {
                generation: d.allocation_generation,
                kind: d.allocation_kind,
                byte_size: d.byte_size,
            }),
            (None, None) => None,
        };
        if let Some((Hvm1Role::ReplyPool, byte_size, generation)) = hvm1 {
            if let Some(device) = unsafe { crate::device::DeviceHandleRef::from_raw(h_device) } {
                crate::ddi::translation_session::bind_reply_pool(
                    device.session_cell(),
                    Hvm1Role::ReplyPool,
                    byte_size,
                    generation,
                );
            }
        }

        // ⛔ C1's liveness gate is GONE with the resid it gated on.
        //
        // It refused an open whose venus resource was no longer alive, and it
        // was a trust boundary because the resid came from the UMD. Under HWA2
        // the KMD owns every resid it hands out and no descriptor names one, so
        // the check has nothing to check: `resource_is_live(0)` is not a weaker
        // form of the gate, it is a different question. K6 re-establishes the
        // property where it now belongs — Render/Patch resolving the exact
        // patched WDDM capability against the live allocation generation
        // (§18.1:4723-4726).

        // ⚠ A3: no `PresentAllocInfo` is constructible here. See the DDI doc.
        // The trace-only companion IS built, because every field it carries
        // comes from the descriptor and none of them is an identity.
        //
        // A PLAIN `fetch_add`: this is the SUCCESS path of every open, so `bump`
        // would put a synchronous `RtlWriteRegistryValue` on DWM's open path at a
        // period the workload chooses. The atomic is mirrored from
        // [`ALLOC_COUNTERS`] on the create DDI's PASSIVE cadence instead.
        if desc.is_some() {
            OPEN_NO_RESOURCE_ID.fetch_add(1, Ordering::Relaxed);
        }
        let present_diag = desc.map(|desc| PresentAllocDiag {
            runtime_allocation: info.hAllocation,
            // §10.3 offset 84 carries the exact OS enum when `STANDARD` is set
            // and zero otherwise, which is precisely this field's contract.
            standard_allocation_type: desc.standard_allocation_type,
            // ⛔ NOT CARRIED BY HWA2. The retired trailer packed
            // `D3DKMDT_GDISURFACETYPE` into its misc word; §10.3's misc
            // vocabulary has four bits and none of them is it. The honest
            // successor is the swizzle class — a GDI texture is the OPAQUE
            // OPTIMAL one — but that is a different value with a different
            // meaning, so this trace field reports 0 rather than a lookalike.
            standard_gdi_surface_type: 0,
            open_flags,
            resource_associated: desc.has_flag(HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED),
            allocation_private_size: info.PrivateDriverDataSize,
            resource_private_size: args.PrivateDriverSize,
        });
        let open = Box::new(OpenAllocationContext {
            magic: OPEN_ALLOCATION_CTX_MAGIC,
            present: None,
            present_diag,
            identity: open_identity,
        });
        record_alloc_event(
            0,
            desc.map_or(0, |d| d.width),
            desc.map_or(0, |d| d.height),
            0,
            true,
        );
        info.hDeviceSpecificAllocation = Box::into_raw(open) as HANDLE;
        crate::diag::record(
            0x0C36_0000 | ((info.hDeviceSpecificAllocation as usize as u32) & 0xFFFF),
        );

        // `Pitch` and `SubresourceOffset` are CALL-level OUT fields, not
        // per-entry ones; writing them inside the loop meant the last entry won
        // silently. They are computed once after the loop, from the entry
        // args.SubresourceIndex designates (M4/k-alloc-12).
        if i == subresource_entry {
            subresource_desc = desc;
        }
    }

    if let Some(desc) = subresource_desc {
        args.SubresourceOffset = 0;
        // R1007's one resolver, restated over the descriptor that now owns the
        // layout. A tiled surface has no linear row layout and reports 0, which
        // is what OpenAllocation has always done and is the documented "no
        // pitch" answer for a GDI texture. `plane_count >= 1` is guaranteed for
        // an image kind by `validate`, and a non-image kind has `plane_count ==
        // 0` and therefore an all-zero plane record — so both arms are total
        // without an index that could be out of range.
        args.Pitch = RowPitch::resolve(
            desc.swizzle_class != HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL,
            desc.planes[0].row_pitch,
            desc.width,
        )
        .map_or(0, RowPitch::get);
        crate::diag::record(0x0C38_0000 | (args.Pitch.min(0xFFFF) as u32));
    }
    STATUS_SUCCESS
}

/// Parse and validate an allocation's create-time HWA2 descriptor at open time.
///
/// `None` for a buffer that is absent, the wrong length, or fails
/// `validate_create_output` — §10.3:1079-1080 makes a malformed descriptor fail
/// the OPEN as well as the create, and the open is not a place to be lenient
/// because the receiving UMD reads the identical bytes.
///
/// ⚠ It answers `None` rather than an error because `DxgkDdiOpenAllocation` is
/// called for every allocation including HVM1 and HOC1 ones, whose private data
/// is a different 64-byte record and legitimately is not an HWA2. Refusing the
/// whole open there would fail every native-Vulkan and D3D12 allocation.
/// `OaHwa2Rej` counts only the buffers that were HWA2-shaped and still failed.
///
/// # Safety
/// `private` is dxgkrnl's per-allocation private buffer and `private_size` its
/// authoritative length. NOTHING here writes through the pointer.
unsafe fn read_open_descriptor(
    private: *const c_void,
    private_size: UINT,
) -> Option<HeliosWddmAllocationDescV2> {
    if private.is_null() || private_size as usize != HELIOS_HWA2_BYTES as usize {
        return None;
    }
    // SAFETY: non-null and the length is exactly the record size, checked above.
    let bytes =
        unsafe { core::slice::from_raw_parts(private as *const u8, HELIOS_HWA2_BYTES as usize) };
    let desc = HeliosWddmAllocationDescV2::from_private_data(bytes).ok()?;
    if desc.magic != HELIOS_HWA2_MAGIC {
        // Not an HWA2 record that merely happens to be 168 bytes long. Not
        // counted: it is not a rejected HWA2, it is a different record.
        return None;
    }
    if desc
        .validate_create_output(HELIOS_PACKAGE_GENERATION)
        .is_err()
    {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        crate::diag::record(0x0C02_00E6);
        return None;
    }
    Some(desc)
}

/// Read an already-published HVM1 create-output. The ORDINARY-open half of
/// [`stamp_open_hvm1`]: same answer, no write.
///
/// `None` for anything that is not a complete HVM1 create-output — including a
/// still-unstamped create-input, which on an ordinary open means the creating
/// open never ran or never published, and is not something this DDI may repair.
///
/// # Safety
/// `private` is dxgkrnl's per-allocation private buffer and `private_size` its
/// authoritative length. NOTHING here writes through the pointer.
unsafe fn read_open_hvm1(
    private: *const c_void,
    private_size: UINT,
) -> Option<(Hvm1Role, u64, u64)> {
    if private.is_null() || private_size as usize != HELIOS_HVM1_SIZE as usize {
        return None;
    }
    // SAFETY: non-null and the length is exactly the record size, checked above.
    let bytes =
        unsafe { core::slice::from_raw_parts(private as *const u8, HELIOS_HVM1_SIZE as usize) };
    let record = HeliosVenusMemoryAllocationV1::from_private_data(bytes).ok()?;
    if record.magic != HELIOS_HVM1_MAGIC {
        return None;
    }
    let role = record
        .validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateOutput)
        .ok()?;
    Some((role, record.byte_size, record.object_generation))
}

/// The identity this open PUBLISHED, resolved from the device-specific handle
/// dxgkrnl puts in `DXGK_ALLOCATIONLIST::hDeviceSpecificAllocation`.
///
/// ⛔ **This, not [`allocation_identity`], is K6's reader.** Render and Patch
/// receive the open handle and never the create handle, and the value here is
/// the one the guest was told — so a use record's
/// `expected_allocation_generation` is compared against exactly the number its
/// producer read back, with no second derivation that could disagree.
///
/// # Safety
/// `h` is either null or an `hDeviceSpecificAllocation` this driver returned
/// from `DxgkDdiOpenAllocation`, round-tripped unmodified.
pub(crate) unsafe fn open_allocation_identity(h: HANDLE) -> Option<OpenIdentity> {
    // SAFETY: validated by `open_allocation_context`, which checks the magic.
    let open = unsafe { open_allocation_context(h)? };
    open.identity
}

/// Complete an HVM1 record's create-output at OPEN and publish it through the
/// `in/out` private-data pointer. Returns `(role, byte_size, object_generation)`.
///
/// `None` for a buffer that is absent, the wrong length, or not an HVM1 —
/// `DxgkDdiOpenAllocation` is called for every allocation, and an HWA2 or HOC1
/// legitimately is not one. Not counted in that case: it is a different record,
/// not a rejected HVM1.
///
/// ⛔ THIS IS THE ONE WRITE THIS DDI PERFORMS, and it is here because the create
/// path's identical write is discarded — see [`OPEN_HVM1_STAMPED`] for the
/// measurement.
///
/// The "two openers disagree" defect the no-restamp rule exists for cannot reach
/// an HVM1: §10.7:1971-1972 requires every sharing flag zero, so an HVM1 has
/// exactly one opener by construction. Belt and braces anyway — an already
/// stamped buffer is republished as-is rather than re-minted.
///
/// # Safety
/// `private` is dxgkrnl's per-allocation private buffer and `private_size` its
/// authoritative length. The write is bounded by the exact-length check below.
unsafe fn stamp_open_hvm1(
    private: *mut c_void,
    private_size: UINT,
) -> Option<(Hvm1Role, u64, u64)> {
    if private.is_null() || private_size as usize != HELIOS_HVM1_SIZE as usize {
        return None;
    }
    // Scoped exactly as `admit_hwa2`'s read is: the write below forms a
    // `&mut [u8]` over the same address range, and holding a live `&[u8]` across
    // it is aliasing UB that borrowck cannot see through raw pointers.
    let parsed = {
        // SAFETY: non-null and the length is exactly the record size.
        let bytes =
            unsafe { core::slice::from_raw_parts(private as *const u8, HELIOS_HVM1_SIZE as usize) };
        HeliosVenusMemoryAllocationV1::from_private_data(bytes)
    };
    let mut record = parsed.ok()?;
    if record.magic != HELIOS_HVM1_MAGIC {
        return None;
    }
    // Already complete: a second open, or a buffer dxgkrnl kept from a first one.
    if let Ok(role) = record.validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateOutput) {
        return Some((role, record.byte_size, record.object_generation));
    }
    let Ok(role) = record.validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateInput) else {
        bump(&OPEN_HVM1_REJECT, b"OaHvm1Rej");
        return None;
    };
    let Some(generation) = allocation_object::mint() else {
        bump(&OPEN_HVM1_REJECT, b"OaHvm1Rej");
        return None;
    };
    // The same three fields, and the same values, `admit_hvm1` computes
    // (§10.7:1955, :1960, :1961). Every backing this driver creates is
    // page-granular, so `PAGE` is the exact alignment.
    record.object_generation = generation;
    record.segment_page_shift = HELIOS_HVM1_SEGMENT_PAGE_SHIFT;
    record.allocation_alignment = PAGE as u64;
    if record
        .validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateOutput)
        .is_err()
    {
        bump(&OPEN_HVM1_REJECT, b"OaHvm1Rej");
        return None;
    }
    // SAFETY: length proven exactly `HELIOS_HVM1_SIZE` above; dxgkrnl owns a
    // writable buffer of that length for the call's duration, and the WDK
    // annotates this pointer `in/out` for exactly this purpose.
    let out =
        unsafe { core::slice::from_raw_parts_mut(private as *mut u8, HELIOS_HVM1_SIZE as usize) };
    out.copy_from_slice(bytes_of(&record));
    bump(&OPEN_HVM1_STAMPED, b"OaHvm1Stamp");
    Some((role, record.byte_size, generation))
}

/// `DxgkDdiCloseAllocation` — release device-local allocation references.
pub unsafe extern "C" fn dxgkddi_close_allocation(
    _h_device: IN_CONST_HANDLE,
    close_allocation: IN_CONST_PDXGKARG_CLOSEALLOCATION,
) -> NTSTATUS {
    if close_allocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let args = unsafe { &*close_allocation };
    if args.NumAllocations != 0 && args.pOpenHandleList.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    for i in 0..args.NumAllocations as usize {
        let handle = unsafe { *args.pOpenHandleList.add(i) };
        if !handle.is_null() {
            crate::diag::record(0x0C37_0000 | ((handle as usize as u32) & 0xFFFF));
            // Taking the context frees it; nothing in it was ever read.
            let _ = unsafe { take_open_ctx(handle) };
        }
    }
    STATUS_SUCCESS
}

/// `DxgkDdiDescribeAllocation` — report an allocation's dimensions/format.
///
/// dxgkrnl calls this for shared / cross-process surfaces (and DWM's composition
/// surfaces) to learn their geometry. We echo the geometry recorded at
/// CreateAllocation time (from the standard-allocation trailer). UMD blob
/// allocations carry no dimensions (0×0); report them as-is.
pub unsafe extern "C" fn dxgkddi_describe_allocation(
    h_adapter: IN_CONST_HANDLE,
    describe_allocation: INOUT_PDXGKARG_DESCRIBEALLOCATION,
) -> NTSTATUS {
    crate::diag::record(0x0C02_0001);
    if h_adapter.is_null() || describe_allocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: dxgkrnl passes a writable DXGKARG_DESCRIBEALLOCATION.
    let args = unsafe { &mut *describe_allocation };
    // This DDI used to dereference hAllocation with no magic check at all —
    // the only handle accessor in the file that trusted the pointer outright.
    // SAFETY: hAllocation is the AllocationContext pointer we returned from
    // CreateAllocation; dxgkrnl round-trips it back unmodified.
    let Some(ctx) = (unsafe { describe_alloc_info(args.hAllocation) }) else {
        let n = DESCRIBE_BAD_HANDLE.fetch_add(1, Ordering::Relaxed) + 1;
        // First occurrence plus every 64th — never a per-call registry write on
        // a path a caller could repeat.
        if n == 1 || n % 64 == 0 {
            crate::diag::record_named_bytes(b"DsBad", n);
        }
        return STATUS_INVALID_PARAMETER;
    };
    crate::diag::record(0x0C20_0000 | (ctx.width.min(0xFFFF) as u32));
    crate::diag::record(0x0C22_0000 | (ctx.height.min(0xFFFF) as u32));
    crate::diag::record(0x0C23_0000 | (ctx.format & 0xFFFF));

    args.Width = ctx.width;
    args.Height = ctx.height;
    // Default a plausible BGRA format for dimensionless UMD blobs so dxgkrnl never
    // sees D3DDDIFMT_UNKNOWN(0) for a describable allocation.
    args.Format = if ctx.format != 0 {
        ctx.format as D3DDDIFORMAT
    } else {
        D3DDDIFMT_A8R8G8B8
    };
    args.MultisampleMethod.NumSamples = 1;
    args.MultisampleMethod.NumQualityLevels = 1;
    args.RefreshRate.Numerator = 60;
    args.RefreshRate.Denominator = 1;
    args.PrivateDriverFormatAttribute = 0;
    STATUS_SUCCESS
}

/// `DxgkDdiGetStandardAllocationDriverData` — describe a runtime "standard"
/// allocation (shared primary, shadow, staging, GDI surface). DWM and IddCx use
/// these for the desktop composition surfaces.
///
/// Two-call contract (viogpu3d `viogpu_allocation.cpp:135` is the template):
///   1. **Size query** — `pAllocationPrivateDriverData == NULL`: report the byte
///      sizes the runtime must allocate for the per-allocation / per-resource
///      private data.
///   2. **Fill** — buffers provided: write the private data the runtime then hands
///      to `DxgkDdiCreateAllocation`, and fill the surface `Pitch` out-fields.
///
/// # This DDI is the create-INPUT author for every OS standard allocation
///
/// It writes ONE [`HeliosWddmAllocationDescV2`] — 168 bytes, not the retired
/// 48+48 pair — with `allocation_generation == 0`, and
/// `DxgkDdiCreateAllocation` hands the identical buffer back with the generation
/// and the KMD-owned flags stamped in. So the two halves of this file are the
/// two stages of one record, and the self-check before the write below is what
/// keeps them from drifting: if this DDI ever authors something
/// `validate_create_input` refuses, `StdSelf` moves and the surface fails HERE
/// rather than at an unexplained create.
///
/// ⚠ H16 in the obligation checklist is INFERRED, not stated: §10.3 defines the
/// `STANDARD` flag (`:1060`), the standard kinds (`:1059`) and the offset-84
/// rule (`:1064`) but never names this DDI. The reading below is the only one
/// consistent with all three.
pub unsafe extern "C" fn dxgkddi_get_standard_allocation_driver_data(
    h_adapter: IN_CONST_HANDLE,
    standard_allocation: INOUT_PDXGKARG_GETSTANDARDALLOCATIONDRIVERDATA,
) -> NTSTATUS {
    if h_adapter.is_null() || standard_allocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: dxgkrnl hands back our AdapterContext and a writable args struct.
    let args = unsafe { &mut *standard_allocation };
    let standard_allocation_type = args.StandardAllocationType as u32;
    crate::diag::record_named_bytes(b"StdType", standard_allocation_type);
    crate::diag::record(0x0C02_0002 | ((args.StandardAllocationType as u32 & 0xFF) << 4));

    // §10.3:1039-1040 — ONE record, exactly `HELIOS_HWA2_BYTES`. The retired
    // shape was `size_of::<HeliosWddmAllocPrivate>() +
    // size_of::<HeliosWddmAllocMeta>()` = 96; the runtime allocates whatever is
    // reported here and `from_private_data` requires exactly it, so this
    // constant and that check are one contract expressed twice.
    const PRIV_SIZE: u32 = HELIOS_HWA2_BYTES as u32;

    // ── Phase 1: size query (runtime passes a null allocation buffer) ────────
    if args.pAllocationPrivateDriverData.is_null() {
        args.AllocationPrivateDriverDataSize = PRIV_SIZE;
        args.ResourcePrivateDriverDataSize = PRIV_SIZE;
        crate::diag::record_named_bytes(b"StdPhase", 1);
        crate::diag::record_named_bytes(b"StdAPSz", PRIV_SIZE);
        crate::diag::record_named_bytes(b"StdRPSz", PRIV_SIZE);
        return STATUS_SUCCESS;
    }
    crate::diag::record_named_bytes(b"StdPhase", 2);
    crate::diag::record_named_bytes(b"StdAPSz", args.AllocationPrivateDriverDataSize);
    crate::diag::record_named_bytes(b"StdRPSz", args.ResourcePrivateDriverDataSize);
    if (args.AllocationPrivateDriverDataSize as usize) < PRIV_SIZE as usize {
        return STATUS_INVALID_PARAMETER;
    }
    if !args.pResourcePrivateDriverData.is_null()
        && (args.ResourcePrivateDriverDataSize as usize) < PRIV_SIZE as usize
    {
        return STATUS_INVALID_PARAMETER;
    }

    // ── Phase 2: extract geometry from the per-type union; set out Pitch ─────
    // SAFETY: the union arm is selected by StandardAllocationType; dxgkrnl
    // guarantees the matching surface-data pointer is valid for the fill call.
    let is_primary = args.StandardAllocationType == D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE;
    let mut is_optimal_gdi_texture = false;
    // The exact create-time `VidPnSourceId` (§10.3 offset 80). Only the
    // shared-primary arm has one; every other standard allocation is a
    // non-primary and carries the sentinel, which is what `validate` requires of
    // one. ⭐ `D3DKMDT_SHAREDPRIMARYSURFACEDATA` really does carry the field
    // (`tmp/dxgk_bindings.rs`, the struct's fifth member) — without it a
    // KMD-authored primary could not satisfy §10.3:1063's "concrete source for a
    // conventional D3D11 primary" at all.
    let mut vidpn_source = D3DDDI_ID_UNINITIALIZED;
    let (width, height, format): (u32, u32, u32) = match args.StandardAllocationType {
        D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE => {
            let sd = unsafe { &*args.__bindgen_anon_1.pCreateSharedPrimarySurfaceData };
            vidpn_source = sd.VidPnSourceId;
            (sd.Width, sd.Height, sd.Format as u32)
        }
        D3DKMDT_STANDARDALLOCATION_SHADOWSURFACE => {
            let sd = unsafe { &mut *args.__bindgen_anon_1.pCreateShadowSurfaceData };
            sd.Pitch = RowPitch::linear(0, sd.Width).get();
            (sd.Width, sd.Height, sd.Format as u32)
        }
        D3DKMDT_STANDARDALLOCATION_STAGINGSURFACE => {
            let sd = unsafe { &mut *args.__bindgen_anon_1.pCreateStagingSurfaceData };
            sd.Pitch = RowPitch::linear(0, sd.Width).get();
            (sd.Width, sd.Height, D3DDDIFMT_A8R8G8B8 as u32)
        }
        D3DKMDT_STANDARDALLOCATION_GDISURFACE => {
            let sd = unsafe { &mut *args.__bindgen_anon_1.pCreateGdiSurfaceData };
            let gdi_surface_type = sd.Type as u32;
            crate::diag::record_named_bytes(b"GdiType", gdi_surface_type);
            // D3DKMDT_GDISURFACE_TEXTURE (enum value 1) is explicitly not
            // CPU-visible and has no linear-pitch contract. The CPU-visible
            // staging variants are the only GDI types for which Windows
            // requires the miniport to return a pitch.
            is_optimal_gdi_texture = gdi_surface_type == GDI_SURFACE_TYPE_TEXTURE;
            sd.Pitch =
                RowPitch::resolve(!is_optimal_gdi_texture, 0, sd.Width).map_or(0, RowPitch::get);
            (sd.Width, sd.Height, sd.Format as u32)
        }
        _ => {
            crate::diag::record(0x0C02_00E2);
            return STATUS_NOT_SUPPORTED;
        }
    };

    // THE PRODUCER of the descriptor's plane pitch, and the reason a consumer's
    // fallback arm is `cross_adapter_pitch(width)` at all: KMD standard
    // allocations are AUTHORED with exactly that value, so the consumers agree
    // with the producer by construction rather than by coincidence.
    //
    // ⚠ The one arm that breaks the correspondence is not here: the KMD-created
    // LINEAR scan-out image's real stride is Vulkan's `scanout.row_pitch`, which
    // is NOT derived from width. `admit_hwa2` therefore prefers the created
    // backing's pitch over the descriptor's plane record for that arm alone, and
    // that is the ONLY place the two can differ.
    //
    // An OPTIMAL GDI texture has no linear row layout, so `RowPitch::resolve`
    // answers `None` and the runtime is told 0 — but §10.3 still requires an
    // image kind to declare one plane with nonzero pitches, so the descriptor's
    // plane record carries the notional 256-aligned stride instead. The two
    // answers are deliberately different: the runtime is told "no byte
    // addressing exists", the descriptor is told "this is how much memory one
    // slice spans", and the swizzle class is what tells a reader which
    // interpretation applies.
    let plane_pitch = RowPitch::linear(0, width).get();
    let size = if is_optimal_gdi_texture {
        // An ESTIMATE. Deliberately computed from the notional 256-aligned
        // stride rather than `width * height * 4`, because §10.3:1050 makes
        // `byte_size` bound every plane and the plane below spans
        // `plane_pitch * height`; the older `width*height*4` estimate is smaller
        // than that whenever the 256 alignment inflates the stride, which would
        // make the descriptor fail its own plane-range check.
        //
        // ⚠ CONSEQUENCE OF THE ECHO RULE, recorded because it is a real change:
        // `DxgkDdiCreateAllocation` used to REPLACE this estimate with Vulkan's
        // exact memory requirement before reporting Size to VidMm. It may not
        // any more — the descriptor is echoed verbatim, and correcting a field
        // would make it disagree with the resource the creator believes it made
        // (`K4-CONTRACT.md` §1.1).
        //
        // ⭐ Re-derived after round 3 of the Phase-2 review, because two of the
        // three clauses that used to follow this sentence had gone false and
        // the third named the wrong instrument. What is true at HEAD, for THIS
        // arm — the `OPAQUE_OPTIMAL` GDI texture — is:
        //
        //   * the estimate DOES survive into the published descriptor. §1.3
        //     Tier 1's "adopt the host's measured extent" is a PAIR with the
        //     plane record and is applied only where a real Vulkan stride
        //     exists to adopt; this arm reports `pitch: 0`, so nothing moves.
        //     Round 2 briefly moved `byte_size` here without the plane record
        //     and made the descriptor fail its own plane-range check;
        //     `create_one`'s adoption block carries that argument in full.
        //   * `AcSize` does NOT validate it. The undersize guard is Tier 2's,
        //     scoped to descriptors this driver did not author — comparing this
        //     LINEAR notional span against a TILED requirement would refuse
        //     legal creates, which is exactly the defect just described.
        //   * VidMm IS charged the larger of the two, which is the direction
        //     that matters for the aperture page count.
        //   * the exact host size stays on the KMD allocation object, where
        //     `protocol/src/wddm.rs`'s `HeliosWddmAllocationDescV2` doc (the
        //     "No host resource token, `resid`, PID, …" paragraph — cite the
        //     symbol, the line has moved twice) says host detail belongs.
        //
        // The divergence between this estimate and the host's answer is counted
        // by [`LINEAR_BLOB_SIZE_DIVERGENCE`] and acted on by nothing.
        round_up_page(
            (plane_pitch as u64)
                .saturating_mul(height as u64)
                .max(PAGE as u64),
        )
    } else {
        linear_blob_size(plane_pitch as u64, height as u64)
    };
    // The plane's extent. `slice_pitch` is a `u32` by §10.3's plane record, so a
    // surface whose slice does not fit one must be refused rather than
    // truncated: a truncated slice pitch would under-state the plane and the
    // descriptor's own bound check would then pass on a lie.
    let slice_pitch = (plane_pitch as u64).saturating_mul(height as u64);
    if slice_pitch == 0 || slice_pitch > u32::MAX as u64 || slice_pitch > size {
        bump(&STANDARD_SELF_REJECT, b"StdSelf");
        return STATUS_NOT_SUPPORTED;
    }

    // §10.3:1055 — an image kind must carry an EXACT `DXGI_FORMAT`, and
    // `validate` refuses `UNKNOWN` outright. The retired trailer wrote 0 here
    // for a format with no DXGI peer and let the opener fall back to BGRA; that
    // is no longer expressible, so refuse rather than guess. See
    // `STANDARD_FORMAT_REFUSED` for why this is a deliberate behaviour change.
    let dxgi_format = if is_primary {
        // The display primary must scan out as XR24/XRGB (see
        // `scanout_dxgi_for_primary`): the Linux virtio primary plane advertises
        // XRGB only, and the matching CachyOS dma-buf probe reached egl-headless
        // only with XR24.
        scanout_dxgi_for_primary().as_u32()
    } else {
        match d3dddi_to_dxgi(format) {
            Some(fmt) => fmt.as_u32(),
            None => {
                bump(&STANDARD_FORMAT_REFUSED, b"StdFmt");
                return STATUS_NOT_SUPPORTED;
            }
        }
    };

    let allocation_kind = match args.StandardAllocationType {
        D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE => HELIOS_HWA2_KIND_STANDARD_PRIMARY,
        D3DKMDT_STANDARDALLOCATION_SHADOWSURFACE => HELIOS_HWA2_KIND_STANDARD_SHADOW,
        D3DKMDT_STANDARDALLOCATION_STAGINGSURFACE => HELIOS_HWA2_KIND_STANDARD_STAGING,
        // §10.3 has no GDI-surface kind: "a `D3DKMDT_STANDARDALLOCATION_GDISURFACE`
        // is `HELIOS_HWA2_KIND_IMAGE` plus the `STANDARD` bit and the exact OS
        // enum in `standard_allocation_type`" (`protocol/src/wddm.rs`, the
        // `helios_hwa2_kind_is_image` doc).
        _ => HELIOS_HWA2_KIND_IMAGE,
    };
    // An OPTIMAL GDI texture is a shared, non-CPU-visible tiled texture; every
    // other standard allocation is a CPU-rasterizable linear surface.
    let cpu_visible = !is_optimal_gdi_texture;
    // ⛔ `SHARED` is deliberately NOT set, even though DWM does open these.
    // It is not a decoration: `classify_hwa2` maps it onto
    // `allocate_memory_blob`'s `shareable` argument, which adds
    // `VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE` and a `MemoryPNext::Export{DMA_BUF}`
    // to the venus allocation. The pre-retirement classifier reached the
    // standard-buffer arm only with `primary == false` (a primary classified as
    // the LINEAR scan-out image first), so NO standard allocation has ever been
    // created shareable, and the 38th session's two dma-buf regressions —
    // global modifier-ext enable inflating the IMPORT memory requirement, and
    // dma_buf advertisement breaking enumerate through the WSI proxy — are
    // exactly what an unmeasured flip of this bit risks. Flipping it needs a
    // measurement, not a reading of the flag's name (CLAUDE.md rule 8).
    let mut flags = HELIOS_HWA2_FLAG_STANDARD;
    if cpu_visible {
        flags |= HELIOS_HWA2_FLAG_CPU_VISIBLE;
    }
    if is_primary {
        flags |= HELIOS_HWA2_FLAG_PRIMARY | HELIOS_HWA2_FLAG_DISPLAYABLE;
    }
    // ⛔ `DIRECT_FLIP_COMPATIBLE` and `D3D12_RUNTIME_PRIMARY` are NOT set here.
    // §10.3:1071-1076 makes both KMD-owned *create-time* decisions and
    // `K4-CONTRACT.md` §1.1 requires them zero on the input side;
    // `admit_hwa2` stamps them. Setting one here would be this DDI answering a
    // question the create path is the authority for.

    let mut desc = HeliosWddmAllocationDescV2::header(HELIOS_PACKAGE_GENERATION, 0);
    desc.byte_size = size;
    desc.width = width;
    desc.height = height;
    // One 2D surface, one mip, one sample: the exact shape every standard
    // allocation has, stated rather than defaulted because `validate` requires
    // each of them nonzero for an image kind.
    desc.depth_or_array_size = 1;
    desc.mip_levels = 1;
    desc.sample_count = 1;
    desc.sample_quality = 0;
    desc.dxgi_format = dxgi_format;
    desc.d3d_ddi_format = format;
    desc.allocation_kind = allocation_kind;
    desc.flags = flags;
    // D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, in the SHARED
    // PROTOCOL VOCABULARY — never the raw DDI word. Keeps standard cross-adapter
    // surfaces usable by the UMD when another process opens them.
    desc.bind_flags = HELIOS_HWA2_BIND_SHADER_RESOURCE | HELIOS_HWA2_BIND_RENDER_TARGET;
    desc.misc_flags = if args.StandardAllocationType == D3DKMDT_STANDARDALLOCATION_GDISURFACE {
        HELIOS_HWA2_MISC_GDI_COMPATIBLE
    } else {
        0
    };
    desc.vidpn_source = vidpn_source;
    desc.standard_allocation_type = standard_allocation_type;
    desc.swizzle_class = if is_optimal_gdi_texture {
        HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL
    } else {
        HELIOS_HWA2_SWIZZLE_LINEAR
    };
    desc.memory_class = if cpu_visible {
        HELIOS_HWA2_MEMORY_CPU_VISIBLE
    } else {
        HELIOS_HWA2_MEMORY_DEVICE_LOCAL
    };
    desc.plane_count = 1;
    desc.planes[0] = HeliosWddmPlaneRecordV2 {
        offset: 0,
        row_pitch: plane_pitch,
        // Bounded against `byte_size` above, which is why this cast cannot
        // truncate.
        slice_pitch: slice_pitch as u32,
    };

    // The self-check that keeps the two halves of this file honest. A failure is
    // a driver bug: this DDI would be authoring a record `create_one` will
    // refuse, and the surface would fail with no line naming why.
    if desc
        .validate_create_input(HELIOS_PACKAGE_GENERATION)
        .is_err()
    {
        bump(&STANDARD_SELF_REJECT, b"StdSelf");
        return STATUS_NOT_SUPPORTED;
    }

    // SAFETY: AllocationPrivateDriverDataSize bytes (>= PRIV_SIZE) are writable,
    // checked above, and `bytes_of` yields exactly `PRIV_SIZE` of them.
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytes_of(&desc).as_ptr(),
            args.pAllocationPrivateDriverData as *mut u8,
            PRIV_SIZE as usize,
        );
    }
    if !args.pResourcePrivateDriverData.is_null() {
        // The resource-level copy is the same bytes. It is NOT a second identity
        // and nothing reads it as one: `DxgkDdiCreateAllocation` reads only the
        // per-allocation buffer (§10.3 associates one descriptor with one
        // allocation), and `DxgkDdiOpenAllocation` writes neither.
        //
        // SAFETY: ResourcePrivateDriverDataSize >= PRIV_SIZE, checked above.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes_of(&desc).as_ptr(),
                args.pResourcePrivateDriverData as *mut u8,
                PRIV_SIZE as usize,
            );
        }
        args.ResourcePrivateDriverDataSize = PRIV_SIZE;
    }
    args.AllocationPrivateDriverDataSize = PRIV_SIZE;
    crate::diag::record(0x0C02_0005);
    STATUS_SUCCESS
}
