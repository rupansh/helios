//! Allocation management DDIs for exact runtime allocation ownership.
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
//! `HeliosWddmPlaneRecordV2`). `DXGK_OPENALLOCATIONINFO::hAllocation` is
//! dxgkrnl's runtime token, but WDDM 2.0 supplies the paired
//! `DxgkCbAcquireHandleData` / `DxgkCbReleaseHandleData` callbacks at PASSIVE
//! OpenAllocation time. D4 uses that documented, scoped reference to associate
//! the resulting device-specific open object with this driver's exact
//! `AllocationContext`, then releases it before OpenAllocation returns. The
//! Open/Close contract keeps the allocation live until every binding closes;
//! no adapter-global table or reverse lookup is involved.
//! The guest still receives no host resource id: K6 patches host operands from
//! the canonical allocation owner.
//!
//! ⇒ [`dxgkddi_open_allocation`] stores the canonical allocation association
//! directly. Present and D4 consume that exact owner; no host resource id,
//! reverse lookup, or diagnostic snapshot is published through the open object.
//!
//! TRUST BOUNDARY: `pPrivateDriverData` is guest-supplied and
//! `PrivateDriverDataSize` is the only authoritative length. Every record is
//! entered through its `from_private_data`, which owns the exact-length check —
//! **never** a pointer cast to the record type. The one raw read below the
//! record types is the 4-byte magic used to pick the arm, and it is bounds
//! checked per-arm, not against a max-union.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::size_of;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use bytemuck::bytes_of;
use helios_protocol::{
    helios_hwa2_kind_is_image, helios_hwa2_swizzle_is_direct_flip_capable,
    HeliosOuterCommandAllocationV1, HeliosVenusMemoryAllocationV1, HeliosWddmAllocationDescV2,
    HeliosWddmPlaneRecordV2, Hvm1Role, Hvm1Stage, VirtioGpuMemEntry, D3DDDI_ID_UNINITIALIZED,
    HELIOS_HOC1_BYTES, HELIOS_HOC1_MAGIC, HELIOS_HOC1_POOL_BYTES, HELIOS_HVM1_MAGIC,
    HELIOS_HVM1_SEGMENT_PAGE_SHIFT, HELIOS_HVM1_SIZE, HELIOS_HWA2_BIND_RENDER_TARGET,
    HELIOS_HWA2_BIND_SHADER_RESOURCE, HELIOS_HWA2_BIND_UNORDERED_ACCESS, HELIOS_HWA2_BYTES,
    HELIOS_HWA2_FLAG_CPU_VISIBLE, HELIOS_HWA2_FLAG_CROSS_ADAPTER,
    HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY, HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE,
    HELIOS_HWA2_FLAG_DISPLAYABLE, HELIOS_HWA2_FLAG_PRIMARY, HELIOS_HWA2_FLAG_PROTECTED,
    HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED, HELIOS_HWA2_FLAG_SHARED, HELIOS_HWA2_FLAG_STANDARD,
    HELIOS_HWA2_FLAG_STEREO, HELIOS_HWA2_KIND_BUFFER, HELIOS_HWA2_KIND_IMAGE,
    HELIOS_HWA2_KIND_PAGING_OBJECT, HELIOS_HWA2_KIND_STANDARD_PRIMARY,
    HELIOS_HWA2_KIND_STANDARD_SHADOW, HELIOS_HWA2_KIND_STANDARD_STAGING, HELIOS_HWA2_MAGIC,
    HELIOS_HWA2_MEMORY_CPU_VISIBLE, HELIOS_HWA2_MEMORY_DEVICE_LOCAL,
    HELIOS_HWA2_MISC_GDI_COMPATIBLE, HELIOS_HWA2_SWIZZLE_LINEAR,
    HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL, HELIOS_PACKAGE_GENERATION,
    VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE, VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE,
};
use wdk_sys::ntddk::{
    IoAllocateMdl, IoFreeMdl, KeClearEvent, KeGetCurrentIrql, KeInitializeEvent, KeSetEvent,
    KeWaitForSingleObject, MmMapLockedPagesSpecifyCache,
};
use wdk_sys::{KEVENT, PMDL, PVOID};

use crate::adapter::allocation_object;
use crate::adapter::AdapterContext;
use crate::dxgk::_D3DDDIFORMAT::{D3DDDIFMT_A8B8G8R8, D3DDDIFMT_A8R8G8B8, D3DDDIFMT_X8R8G8B8};
use crate::dxgk::_D3DKMDT_STANDARDALLOCATION_TYPE::{
    D3DKMDT_STANDARDALLOCATION_GDISURFACE, D3DKMDT_STANDARDALLOCATION_SHADOWSURFACE,
    D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE, D3DKMDT_STANDARDALLOCATION_STAGINGSURFACE,
};
use crate::dxgk::*;
use crate::irql::PassiveLevel;
use crate::sync::SpinLock;
use crate::virtio::hal::DmaBuffer;

/// `AllocationContext::magic` — validates `hAllocation` casts in paging DDIs
/// (a garbage dereference in BuildPagingBuffer is a bugcheck).
const ALLOCATION_CTX_MAGIC: u32 = 0x4841_4C43; // "HALC"

const BACKING_STORE_UNBOUND: u32 = 0;
const BACKING_STORE_BINDING: u32 = 1;
const BACKING_STORE_BOUND: u32 = 2;

extern "C" {
    fn helios_mm_probe_and_lock_pages_seh(mdl: PMDL) -> i32;
    fn helios_mm_get_mdl_pfn_array(mdl: PMDL) -> *const u64;
}

const OUTER_GPUVA_MAX_RANGES: usize = 256;

#[derive(Clone, Copy)]
struct OuterGpuVaMapping {
    process: usize,
    gpuva: u64,
    allocation_offset: u64,
    bytes: u64,
}

struct OuterGpuVaState {
    mappings: SpinLock<crate::sync::FixedVec<OuterGpuVaMapping>>,
}

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
    /// The exact host-resource observation used to address this allocation's
    /// canonical row. With KMD D2 enabled, `OwnerTable` is the sole owner and
    /// this immutable value grants no release authority. It is never written
    /// into private data or recovered from geometry.
    resource_id: AtomicU32,
    /// One-shot SetAllocationBackingStore state. The OS-owned address is used
    /// only while locking its pages and is never retained as identity.
    backing_store_state: AtomicU32,
    /// Stable kernel mapping of the locked K2a MDL.  Internal byte access only:
    /// never an ABI field, lookup key, user mapping, or allocation identity.
    /// The canonical resource finalizer keeps the MDL locked until host UNREF
    /// or verified physical reset, which is exactly this mapping's lifetime.
    backing_store_va: AtomicUsize,
    /// MDL retained only for the non-renderer HOC1 shared backing.  HVM1 MDLs
    /// continue to live in their canonical virtio resource finalizer.
    backing_store_mdl: AtomicUsize,
    /// Lazily allocated per-allocation GPUVA mappings.  The pointer is created
    /// only when an exact device open publishes this allocation into an HQA1
    /// device graph; there is no adapter/process lookup table.
    outer_gpuva: AtomicPtr<OuterGpuVaState>,
    /// Exact live HTS1 session that owns this role-1 reply pool, or zero.
    /// Internal direct-object edge only: never serialized, searched, logged, or
    /// accepted from a caller. The owning OpenAllocation holds the strong ref.
    k11_session_binding: AtomicUsize,
    /// Exact virtio transport generation that created `resource_id`. Resource
    /// numbers restart in a replacement transport, so D2 must reject an old
    /// allocation object even when a new row happens to reuse the same scalar.
    transport_instance: u64,
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
    /// Exact HVM1 wire role, or zero for every other allocation kind.
    hvm1_role: u32,
    /// The final create-output HWA2 record for this exact allocation object.
    /// `None` for HVM1/HOC1.  Display admission reaches this only through the
    /// OS-supplied `hAllocation`; it is never reconstructed from the resource
    /// id, dimensions, a scanout cache, or list position.
    final_hwa2: Option<HeliosWddmAllocationDescV2>,
    /// Nonzero observation for KMD-backed standard allocations: the kernel
    /// Venus `VkDeviceMemory` behind the blob. The compiled KMD D2 arm transfers
    /// its only destruction authority into `ResourceBackingFinalizer` before
    /// CREATE; this copy remains usable for non-destructive renderer work.
    venus_memory_id: u64,
    /// Nonzero observation when the standard allocation's memory is bound to a
    /// kernel-created Venus `VkImage`. As above, enabled teardown authority is
    /// held only by the canonical resource row.
    venus_image_id: u64,
    /// How many times this allocation's `resource_id` was substituted into a
    /// generated `VkImportMemoryResourceInfoMESA` operand — i.e. how many times
    /// a host `VkDeviceMemory` was bound to THIS blob. Zero on an allocation
    /// the renderer never imported, which is exactly what an empty scanned-out
    /// primary looks like (22.22.350.0: the primary blob is zero at both ends).
    import_operand_substitutions: AtomicU32,
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
    /// This allocation was reported to VidMm as BAR-segment-only (KMD-backed
    /// standard allocation with a mappable venus blob, BAR segment active).
    bar_eligible: bool,
    /// Provenance of `size`. See [`BackingSize`].
    size_provenance: BackingSize,
}

impl AllocationContext {
    fn resource_id(&self) -> u32 {
        self.resource_id.load(Ordering::Acquire)
    }
}

impl Drop for AllocationContext {
    fn drop(&mut self) {
        let gpuva = self
            .outer_gpuva
            .swap(core::ptr::null_mut(), Ordering::AcqRel);
        if !gpuva.is_null() {
            drop(unsafe { Box::from_raw(gpuva) });
        }
        let mdl = self.backing_store_mdl.swap(0, Ordering::AcqRel) as PMDL;
        if !mdl.is_null() {
            self.backing_store_va.store(0, Ordering::Relaxed);
            self.backing_store_state
                .store(BACKING_STORE_UNBOUND, Ordering::Release);
            unsafe {
                wdk_sys::ntddk::MmUnlockPages(mdl);
                IoFreeMdl(mdl);
            }
        }
    }
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

/// K2a backing-store aliasing probe (2026-08-29). The guest's `D3DKMTLock2`
/// view of a host-visible allocation shows none of the GPU's writes, while the
/// host demonstrably imports these very pages. These read the pages the host
/// was given, so "the host wrote nothing" and "the guest is looking at a
/// different buffer" stop being the same observation.
/// Role-1 backing stores given a system mapping (`Nr2BsMap`).
pub static BS_SAMPLE_MAPPED: AtomicU32 = AtomicU32::new(0);
/// Samples that found a nonzero dword in one (`Nr2BsNz`).
pub static BS_SAMPLE_NONZERO: AtomicU32 = AtomicU32::new(0);
/// The last such dword (`Nr2BsVal`). The probe writes 0xCDCDCDCD through the
/// Lock2 view, so that value here means the two views ARE the same memory.
pub static BS_SAMPLE_VALUE: AtomicU32 = AtomicU32::new(0);
/// Low 32 bits of the mapped VA of the last allocation scanned (`Nr2BsVa`).
///
/// Validity check, not a finding: 26 independently allocated 4 MiB backing
/// stores all reporting the same first value is either a deterministic writer
/// or one shared page read 26 times, and the second would invalidate every
/// conclusion drawn from `Nr2BsCd`.
pub static BS_SAMPLE_VA: AtomicU32 = AtomicU32::new(0);
/// Size in KiB of the last allocation scanned (`Nr2BsSz`). A 4096 here is a
/// DXVK staging pool at the CPU-visible cap; anything small is a venus shmem,
/// and the two answer different questions.
pub static BS_SAMPLE_SIZE_KIB: AtomicU32 = AtomicU32::new(0);
/// Scans performed (`Nr2BsScan`), so a zero `Nr2BsNz` cannot be confused with
/// an instrument that never ran.
pub static BS_SAMPLE_SCANS: AtomicU32 = AtomicU32::new(0);
/// Role-1 allocations the sampler was offered (`Nr2BsSeen`), whether or not one
/// had a mapping to read.
pub static BS_SAMPLE_SEEN: AtomicU32 = AtomicU32::new(0);
/// Sampled backing stores containing [`BS_PROBE_POISON`] (`Nr2BsCd`). Only the
/// guest CPU writes that value, and only through the D3DKMTLock2 view, so a
/// nonzero count here is direct proof the two views are the same memory.
pub static BS_SAMPLE_POISON: AtomicU32 = AtomicU32::new(0);
/// What tools/d3d11_poison_copy_probe.cpp fills its staging textures with.
const BS_PROBE_POISON: u32 = 0xCDCD_CDCD;
/// System PTEs are a global lease; a per-allocation map is otherwise unbounded.
const BS_SAMPLE_MAX: u32 = 64;
/// Cap the all-zero case: this runs per use per submit.
const BS_SAMPLE_MAX_SCANS: u32 = 20_000;
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
/// K2a places every role in the ordinary aperture and the current segment table
/// always reports it. These counters retain the named fail-closed check so a
/// future table/placement drift cannot escape as an unowned dxgkrnl refusal.
static CREATE_ROLE1_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
static CREATE_ROLE2_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
static CREATE_ROLE3_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
static CREATE_ROLE4_SEGMENT_ABSENT: AtomicU32 = AtomicU32::new(0);
/// HOC1's aperture placement refused by the same table cross-check
/// (`AcSegHoc1`).
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
/// HWA2 records published through a create-flagged open (`OaHwa2Stamp`) and
/// opens whose private data could not be reconciled with the exact canonical
/// allocation (`OaHwa2Rej`).
///
/// The create DDI still mints and stores the one allocation generation. Windows
/// discards a KMD write into the user-supplied create buffer, so the
/// create-flagged OpenAllocation is the observable in/out leg of that same
/// transaction. Ordinary opens remain read-only and must carry the identical
/// canonical output.
static OPEN_HWA2_STAMPED: AtomicU32 = AtomicU32::new(0);
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

/// Every registry name the counters above publish, so the compile-time
/// no-truncation proof below has one list to check.
///
/// `diag::record_named_bytes` clamps silently at `diag::MAX_CONFIG_NAME`, so two
/// names sharing a 14-byte prefix would MERGE into one registry value — a
/// refusal counter reading someone else's number. Same guard
/// `diag::FaultCounter` and `native_fence.rs` use.
const RETIREMENT_COUNTER_NAMES: [&[u8]; 28] = [
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
    // The [`ALLOC_COUNTERS`] names, published by that block rather than by
    // [`bump`] — see its own doc for why. Listed here anyway because this array
    // is the file's ONE truncation proof, and a name that skips it is a name
    // nothing checks. `AcGenEpoch` is 10 bytes; `diag::CounterBlock` has no
    // truncation assert of its own, so this list is the only thing standing
    // between it and a silent merge with another value.
    b"OaBadH",
    b"AcGenEpoch",
    b"AcOptLin",
    b"OaHwa2Stamp",
    b"OaHwa2Rej",
    b"OaHvm1Stamp",
    b"OaHvm1Rej",
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

/// Publish "this DDI reached step `n` on call `seq`" as `(seq << 8) | n`.
///
/// PASSIVE only. The value that survives a hang is the step that never
/// returned: the hung thread holds dxgkrnl's adapter DDI lock, so no later call
/// can overwrite it. ⚠ The name must be PRE-CREATED in the service key — see
/// `kmd-registry-counters-are-append-only-fossils`; the key silently refuses to
/// create new values once it is full.
fn step(name: &[u8], seq: u32, n: u32) {
    if crate::diag::diag_step_on() {
        crate::diag::record_named_bytes(name, (seq << 8) | (n & 0xff));
    }
}

static CLOSE_ALLOC_SEQ: AtomicU32 = AtomicU32::new(0);
static DESTROY_ALLOC_SEQ: AtomicU32 = AtomicU32::new(0);

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

/// Counters this file publishes on a bounded PASSIVE create-path cadence.
///
/// [`bump`] is the right emitter for a refusal: refusals are rare, so a mirror on
/// the 1st and every 64th is bounded by construction. It is the WRONG emitter for
/// anything that fires on success — `diag::record_named_bytes` is a synchronous
/// `RtlWriteRegistryValue`, and a success-path counter's period is set by the
/// workload, not by the driver. This block keeps those mirrors off the measured
/// hot paths through the shared `diag::CounterBlock` shape
/// x-dup-dead-27: three modules had each hand-rolled the same dump with
/// different throttles).
///
/// The flush site is the CREATE DDI: one block flush per 64 creates at
/// `PASSIVE_LEVEL`, and `DiagLevel >= 1` flushes every call.
static ALLOC_COUNTERS: crate::diag::CounterBlock = crate::diag::CounterBlock {
    entries: &[
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
            // construction, so the forced flush is bounded.
            failure: true,
        },
        crate::diag::CounterEntry {
            name: b"AcOptLin",
            value: crate::diag::CounterRef::U32(&CREATE_OPTIMAL_AS_LINEAR),
            // A VALUE entry: it can fire on a successful committed-texture
            // create, so marking it a failure would force a registry write per
            // create on that lane's hot path.
            failure: false,
        },
        crate::diag::CounterEntry {
            name: b"OaBadH",
            value: crate::diag::CounterRef::U32(&OPEN_ALLOC_BAD_HANDLE),
            failure: true,
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
    /// Exact KMD allocation object resolved from the per-device runtime handle
    /// by a scoped `DxgkCbAcquireHandleData` call while OpenAllocation is at
    /// PASSIVE_LEVEL.
    ///
    /// This is the documented open-object association, not a resource-id reverse
    /// lookup. The acquire reference has already been released; the value stays
    /// live because dxgkrnl closes every device-specific binding before calling
    /// DestroyAllocation. It is read only while that open handle remains live.
    allocation: usize,
    /// Exactly what this open PUBLISHED to the guest, for K6's Render/Patch
    /// staleness check and its capability records ([`open_allocation_identity`]).
    ///
    /// ⛔ It lives here and not on [`AllocationContext`] because
    /// `DXGK_ALLOCATIONLIST::hDeviceSpecificAllocation` — the handle Render and
    /// Patch resolve — is THIS object, and the create-time handle never appears
    /// in an allocation list at all. Storing what the guest was told, rather
    /// than re-deriving it, is what makes the two sides unable to disagree.
    identity: Option<OpenIdentity>,
    /// Strong HTS1 reference owned by this exact role-1 device-specific open.
    /// CloseAllocation revokes/drains/destroys K11 before dropping it, while
    /// `allocation` and the K2a MDL are still canonically live.
    reply_pool_session: Option<core::ptr::NonNull<crate::ddi::translation_session::SessionObject>>,
    /// Direct executor edge retained from this exact raw-device open.  It is
    /// present only for HVM1 roles 1-4 and exact non-standard,
    /// resource-associated HWA2 buffers/images. `SHARED` is deliberately not
    /// part of this decision: that bit describes external D3D sharing, while
    /// `RESOURCE_ASSOCIATED` is the HRA1 ownership edge. It is never serialized
    /// or searched: Render reaches it only from the
    /// `hDeviceSpecificAllocation` in its own allocation list.
    execution: Option<OpenExecutionBinding>,
    /// Generic open-object rundown used by the D3D12 GPUVA resolver, including
    /// HOC1 which intentionally has no renderer resource attachment.
    outer: OpenOuterBinding,
}

#[derive(Clone, Copy)]
struct OpenOuterRundown {
    open: bool,
    active: u32,
}

/// GPUVA patch resolution / command-pool use.
const OUTER_TAG_GPUVA: u32 = 1;
/// Physical allocation-list use.
const OUTER_TAG_PHYSICAL: u32 = 2;
/// The device open-table bind guard.
const OUTER_TAG_BIND: u32 = 3;
const OUTER_TAG_COUNT: usize = 3;

struct OpenOuterBinding {
    rundown: SpinLock<OpenOuterRundown>,
    drained: UnsafeCell<KEVENT>,
    /// Outstanding guards per acquire site, indexed by tag - 1. Maintained
    /// under `rundown`, so the three always sum to `active` and a sum that
    /// does not is itself the news.
    by_tag: [AtomicU32; OUTER_TAG_COUNT],
}

unsafe impl Send for OpenOuterBinding {}
unsafe impl Sync for OpenOuterBinding {}

impl OpenOuterBinding {
    fn new() -> Self {
        Self {
            rundown: SpinLock::new(OpenOuterRundown {
                open: true,
                active: 0,
            }),
            drained: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            by_tag: [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
        }
    }

    unsafe fn init_event(&self) {
        unsafe { KeInitializeEvent(self.drained.get(), 0, 1) };
    }

    /// `tag` names the acquire site, and every guard carries the tag it was
    /// taken with so `release_tagged` can return it to the same counter. The
    /// earlier design stored only the LAST tag, which cannot answer the
    /// question it was added for: the most recent acquire is not the one that
    /// never came back.
    fn acquire_tagged(&self, tag: u32) -> bool {
        let mut state = self.rundown.lock();
        if !state.open || state.active == u32::MAX {
            return false;
        }
        if state.active == 0 {
            unsafe { KeClearEvent(self.drained.get()) };
        }
        state.active += 1;
        // Under `rundown` on purpose: outside it the per-tag sum could lag
        // `active` by the number of threads mid-acquire, and a disagreement
        // then would not mean anything.
        if let Some(counter) = self.by_tag.get(tag.wrapping_sub(1) as usize) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    fn release_tagged(&self, tag: u32) {
        let signal = {
            let mut state = self.rundown.lock();
            debug_assert!(state.active != 0);
            state.active = state.active.saturating_sub(1);
            // WRAPPING, not saturating: a release whose tag does not match its
            // acquire would otherwise clamp the counter to 0 and read exactly
            // like "nothing outstanding". Underflow shows as 0xF in the nibble.
            if let Some(counter) = self.by_tag.get(tag.wrapping_sub(1) as usize) {
                let previous = counter.load(Ordering::Relaxed);
                counter.store(previous.wrapping_sub(1), Ordering::Relaxed);
            }
            state.active == 0
        };
        if signal {
            unsafe { KeSetEvent(self.drained.get(), 0, 0) };
        }
    }

    /// `census` names the driver's own structures that still hold a guard; it
    /// is evaluated only on the blocking path.
    fn close(&self, census: impl FnOnce() -> u32) {
        let active = {
            let mut state = self.rundown.lock();
            state.open = false;
            state.active
        };
        if active != 0 {
            // PUBLISHED BEFORE THE WAIT ON PURPOSE: this wait has no timeout,
            // and when it does not return, CloseAllocation holds dxgkrnl's
            // adapter-exclusive DDI lock and the whole win32k session deadlocks
            // behind it, so nothing recorded after the wait is ever written.
            //
            // ⛔ ONE VALUE, AND EVERY FIELD COMPUTED BEFORE THE FIRST WRITE.
            // Twice now a `record_named_bytes` sitting BETWEEN two that landed
            // has failed to appear in the registry — `OaOutTag` on .375 and
            // `OaOutWho` on .376 — and both times the vanishing write was the
            // one whose value argument was a CALL. The name is not the cause:
            // `OaOutWho` can be created in that key by hand. So nothing is
            // called between the writes any more, and the answer rides on
            // `OaOutAct`, the write that has always landed. Nibbles, saturating,
            // so it reads straight out of hex as 0xIQPp_321A:
            //   0  active     1 tag1 GPUVA   2 tag2 physical  3 tag3 bind
            //   4  parked no-ticket          5 parked ticketed
            //   6  worker-queued             7 InFlight
            // A tag nibble of 0xF is an underflowed counter, not a count.
            let mut packed = active.min(0xf);
            let mut index = 0;
            while index < self.by_tag.len() {
                packed |= self.by_tag[index].load(Ordering::Relaxed).min(0xf) << (4 * (index + 1));
                index += 1;
            }
            packed |= census() << 16;
            crate::diag::record_named_bytes(b"OaOutAct", packed);
            // Control for the vanishing-write anomaly: same value, second name,
            // nothing called in between. If this one lands, the trigger was the
            // call in the argument; if it does not, it is the name after all.
            crate::diag::record_named_bytes(b"OaOutWho", packed);
            crate::diag::record_named_bytes(
                b"Nr2WkPend",
                crate::ddi::native_render::NR2_WORKER_PENDING_LIVE
                    .load(core::sync::atomic::Ordering::Relaxed),
            );
            crate::diag::wait(crate::diag::waits::OPEN_OUTER, true);
            let _ = unsafe {
                KeWaitForSingleObject(self.drained.get() as PVOID, 0, 0, 0, core::ptr::null_mut())
            };
            crate::diag::wait(crate::diag::waits::OPEN_OUTER, false);
            crate::diag::record_named_bytes(b"OaOutDrn", active);
        }
    }
}

/// Small lifetime token taken under the owning device's open-table lock before
/// a potentially blocking PASSIVE resource attachment.
pub(crate) struct OuterBindGuard {
    open: core::ptr::NonNull<OpenAllocationContext>,
}

unsafe impl Send for OuterBindGuard {}

impl OuterBindGuard {
    pub(crate) fn bind(
        &self,
        passive: PassiveLevel,
        session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
        session_generation: u64,
    ) -> bool {
        let open = unsafe { self.open.as_ref() };
        open.execution
            .as_ref()
            .is_none_or(|binding| binding.bind_outer(passive, session, session_generation))
    }
}

impl Drop for OuterBindGuard {
    fn drop(&mut self) {
        unsafe { self.open.as_ref() }
            .outer
            .release_tagged(OUTER_TAG_BIND);
    }
}

/// What an open published, as K6's Render and Patch read it back.
#[derive(Clone, Copy)]
pub(crate) struct OpenIdentity {
    /// The allocation generation in the bytes this open handed the guest.
    pub generation: u64,
    /// [`ALLOC_KIND_HVM1`] / [`ALLOC_KIND_HOC1`], or the `HELIOS_HWA2_KIND_*`
    /// the descriptor carried.
    pub kind: u32,
    /// Exact HVM1 role, or zero for a non-HVM1 allocation.  This is the role
    /// published by the same open record as `generation`; executor admission
    /// must not reclassify an allocation from placement or visibility.
    pub hvm1_role: u32,
    /// The allocation's own size, from the same bytes.
    ///
    /// ⛔ It has to come from here. `DXGK_ALLOCATIONLIST` carries a handle, a
    /// `WriteOperation` bit, a `SegmentId` and an address — no length — so a
    /// capability record's `byte_length` (§10.7's 48-byte
    /// `Hnr2PhysicalCapability`) has no other source that is not a guess.
    pub byte_size: u64,
}

const OPEN_EXECUTION_UNATTACHED: u32 = 0;
const OPEN_EXECUTION_ATTACHING: u32 = 1;
const OPEN_EXECUTION_ATTACHED: u32 = 2;

#[derive(Clone, Copy)]
struct OpenExecutionRundown {
    open: bool,
    active: u32,
    session: usize,
    owns_session_reference: bool,
    attachment: u32,
    resource_id: u32,
}

/// Per-open direct executor ownership for HVM1 roles 2-4.
///
/// The WDDM open object is the lifetime anchor because it is the object named
/// in Render's allocation list.  CloseAllocation closes admission, waits for
/// every host-custody guard, detaches the exact resource, and only then drops
/// the strong session reference.  No generation or resource scalar can find
/// this object; they are validation facts after the direct edge is reached.
struct OpenExecutionBinding {
    allocation: usize,
    /// Roles 2-4 and HWA2 outer resources retain a second session reference
    /// specifically for executor custody. Role 1 is opened while the session is
    /// provisional and borrows the adjacent `reply_pool_session` reference
    /// instead; close still revokes this binding before releasing that owner.
    rundown: SpinLock<OpenExecutionRundown>,
    drained: UnsafeCell<KEVENT>,
    /// Signaled whenever the one-time CTX_ATTACH transition is not in flight.
    /// Concurrent first users wait at PASSIVE rather than treating another
    /// queue's valid attach as a malformed allocation use.
    attachment_ready: UnsafeCell<KEVENT>,
}

// SAFETY: mutable state is serialized by `rundown`; the event is initialized
// once after the enclosing OpenAllocationContext reaches its final heap
// address, and CloseAllocation joins every guard before freeing that address.
unsafe impl Send for OpenExecutionBinding {}
unsafe impl Sync for OpenExecutionBinding {}

impl OpenExecutionBinding {
    fn new(
        session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
        allocation: usize,
        owns_session_reference: bool,
    ) -> Self {
        Self {
            allocation,
            rundown: SpinLock::new(OpenExecutionRundown {
                open: true,
                active: 0,
                session: session.as_ptr() as usize,
                owns_session_reference,
                attachment: OPEN_EXECUTION_UNATTACHED,
                resource_id: 0,
            }),
            drained: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            attachment_ready: UnsafeCell::new(unsafe { core::mem::zeroed() }),
        }
    }

    fn new_unbound(allocation: usize) -> Self {
        Self {
            allocation,
            rundown: SpinLock::new(OpenExecutionRundown {
                open: true,
                active: 0,
                session: 0,
                owns_session_reference: false,
                attachment: OPEN_EXECUTION_UNATTACHED,
                resource_id: 0,
            }),
            drained: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            attachment_ready: UnsafeCell::new(unsafe { core::mem::zeroed() }),
        }
    }

    /// # Safety
    /// The enclosing Box must be at its final address and unpublished.
    unsafe fn init_event(&self) {
        unsafe { KeInitializeEvent(self.drained.get(), 0, 1) };
        unsafe { KeInitializeEvent(self.attachment_ready.get(), 0, 1) };
    }

    fn acquire(
        &self,
        passive: PassiveLevel,
        expected_session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
        expected_generation: u64,
    ) -> Option<OpenExecutionUse> {
        {
            let mut state = self.rundown.lock();
            if !state.open
                || state.active == u32::MAX
                || state.session != expected_session.as_ptr() as usize
            {
                return None;
            }
            if state.active == 0 {
                unsafe { KeClearEvent(self.drained.get()) };
            }
            state.active += 1;
        }
        let mut guard = OpenExecutionUse {
            owner: core::ptr::NonNull::from(self),
            allocation_generation: 0,
            resource_id: 0,
            transport_instance: 0,
            byte_size: 0,
            memory_type_index: 0,
            hvm1_role: 0,
            kernel_va: None,
        };
        // SAFETY: this binding's ordinary open keeps the canonical allocation
        // live until this rundown guard is returned and CloseAllocation joins.
        let facts = unsafe { hnr2_execution_allocation_facts(self.allocation) }?;
        if facts.allocation_generation != expected_generation {
            return None;
        }

        // Role 1 is the CPU-visible final HVR1 carrier, not a renderer reply
        // target. K11's context-local private resource receives host writes and
        // copies the validated finite range here only after the real terminal;
        // attempting a secondary CTX_ATTACH would cross renderer namespaces
        // without creating usable resource identity in that context.
        if facts.hvm1_role == Hvm1Role::ReplyPool.to_u32() {
            guard.allocation_generation = facts.allocation_generation;
            guard.resource_id = facts.resource_id;
            guard.transport_instance = facts.transport_instance;
            guard.byte_size = facts.byte_size;
            guard.memory_type_index = facts.memory_type_index;
            guard.hvm1_role = facts.hvm1_role;
            guard.kernel_va = facts.kernel_va;
            return Some(guard);
        }

        loop {
            let attach = {
                let mut state = self.rundown.lock();
                if !state.open {
                    return None;
                }
                match state.attachment {
                    OPEN_EXECUTION_ATTACHED => {
                        if state.resource_id != facts.resource_id {
                            return None;
                        }
                        break;
                    }
                    OPEN_EXECUTION_UNATTACHED => {
                        unsafe { KeClearEvent(self.attachment_ready.get()) };
                        state.attachment = OPEN_EXECUTION_ATTACHING;
                        true
                    }
                    OPEN_EXECUTION_ATTACHING => false,
                    _ => return None,
                }
            };
            if !attach {
                // Render is PASSIVE_LEVEL. The attaching user owns an active
                // guard too, so CloseAllocation cannot free either event while
                // this wait is outstanding.
                let _ = unsafe {
                    KeWaitForSingleObject(
                        self.attachment_ready.get() as PVOID,
                        0,
                        0,
                        0,
                        core::ptr::null_mut(),
                    )
                };
                continue;
            }
            let attached = crate::ddi::translation_session::attach_execution_resource(
                expected_session,
                passive,
                facts.resource_id,
                facts.transport_instance,
            );
            let published = {
                let mut state = self.rundown.lock();
                let publish =
                    attached.is_ok() && state.open && state.attachment == OPEN_EXECUTION_ATTACHING;
                if publish {
                    state.resource_id = facts.resource_id;
                    state.attachment = OPEN_EXECUTION_ATTACHED;
                } else {
                    state.resource_id = 0;
                    state.attachment = OPEN_EXECUTION_UNATTACHED;
                }
                unsafe { KeSetEvent(self.attachment_ready.get(), 0, 0) };
                publish
            };
            if !published {
                // The host attach itself is ownership. If CloseAllocation won
                // after that operation but before publication, undo it here;
                // close is waiting on this guard and cannot race the session
                // reference away.
                if attached.is_ok() {
                    crate::ddi::translation_session::detach_execution_resource(
                        expected_session,
                        passive,
                        facts.resource_id,
                    );
                }
                return None;
            }
            break;
        }

        guard.allocation_generation = facts.allocation_generation;
        guard.resource_id = facts.resource_id;
        guard.transport_instance = facts.transport_instance;
        guard.byte_size = facts.byte_size;
        guard.memory_type_index = facts.memory_type_index;
        guard.hvm1_role = facts.hvm1_role;
        guard.kernel_va = facts.kernel_va;
        Some(guard)
    }

    /// Bind an ordinary D3D-device open to the exact HQA1 session before any
    /// scheduler-level submit can name it.  The host CTX_ATTACH round trip runs
    /// at PASSIVE and is one-shot; a second device/session is refused.
    fn bind_outer(
        &self,
        passive: PassiveLevel,
        session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
        session_generation: u64,
    ) -> bool {
        if crate::ddi::translation_session::execution_session_generation(session)
            != Some(session_generation)
        {
            return false;
        }
        let session_word = session.as_ptr() as usize;
        {
            let state = self.rundown.lock();
            if !state.open {
                return false;
            }
            if state.session != 0 {
                if state.session != session_word {
                    return false;
                }
                if state.attachment == OPEN_EXECUTION_ATTACHED {
                    return true;
                }
            }
        }
        if self.rundown.lock().session == 0 {
            let Some(retained) =
                crate::ddi::translation_session::retain_direct_execution_session(session)
            else {
                return false;
            };
            let published = {
                let mut state = self.rundown.lock();
                if state.open && state.session == 0 {
                    state.session = retained.as_ptr() as usize;
                    state.owns_session_reference = true;
                    true
                } else {
                    false
                }
            };
            if !published {
                unsafe { crate::ddi::translation_session::release_execution_session(retained) };
            }
        }
        let Some(facts) = (unsafe { hnr2_execution_allocation_facts(self.allocation) }) else {
            return false;
        };
        self.acquire(passive, session, facts.allocation_generation)
            .is_some()
    }

    /// DISPATCH-safe acquisition after [`Self::bind_outer`] completed the only
    /// host attach.  No wait, allocation, lookup, or control-queue operation is
    /// reachable from this method.
    fn acquire_attached(
        &self,
        expected_session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
        expected_generation: u64,
    ) -> Option<OpenExecutionUse> {
        let facts = unsafe { hnr2_execution_allocation_facts(self.allocation) }?;
        if facts.allocation_generation != expected_generation {
            return None;
        }
        {
            let mut state = self.rundown.lock();
            if !state.open
                || state.active == u32::MAX
                || state.session != expected_session.as_ptr() as usize
                || state.attachment != OPEN_EXECUTION_ATTACHED
                || state.resource_id != facts.resource_id
            {
                return None;
            }
            if state.active == 0 {
                unsafe { KeClearEvent(self.drained.get()) };
            }
            state.active += 1;
        }
        Some(OpenExecutionUse {
            owner: core::ptr::NonNull::from(self),
            allocation_generation: facts.allocation_generation,
            resource_id: facts.resource_id,
            transport_instance: facts.transport_instance,
            byte_size: facts.byte_size,
            memory_type_index: facts.memory_type_index,
            hvm1_role: facts.hvm1_role,
            kernel_va: facts.kernel_va,
        })
    }

    fn close(self, passive: PassiveLevel) {
        let active = {
            let mut state = self.rundown.lock();
            state.open = false;
            // Wake a first-use waiter so it can observe revocation and return
            // its active guard. The actual attacher also signals on completion.
            unsafe { KeSetEvent(self.attachment_ready.get(), 0, 0) };
            state.active
        };
        if active != 0 {
            // Same before/after pair as `OpenOuterBinding::close`, and for the
            // same reason: the wait is untimed and a missing `OaExeDrn` beside a
            // written `OaExeAct` is the only way to tell which of the two joins
            // never returned.
            crate::diag::record_named_bytes(b"OaExeAct", active);
            crate::diag::wait(crate::diag::waits::OPEN_EXECUTION, true);
            let _ = unsafe {
                KeWaitForSingleObject(self.drained.get() as PVOID, 0, 0, 0, core::ptr::null_mut())
            };
            crate::diag::wait(crate::diag::waits::OPEN_EXECUTION, false);
            crate::diag::record_named_bytes(b"OaExeDrn", active);
        }
        let (session, owns_session_reference, attachment, resource_id) = {
            let mut state = self.rundown.lock();
            let tuple = (
                state.session,
                state.owns_session_reference,
                state.attachment,
                state.resource_id,
            );
            state.session = 0;
            state.owns_session_reference = false;
            state.attachment = OPEN_EXECUTION_UNATTACHED;
            state.resource_id = 0;
            tuple
        };
        let session =
            core::ptr::NonNull::new(session as *mut crate::ddi::translation_session::SessionObject);
        if attachment == OPEN_EXECUTION_ATTACHED && resource_id != 0 {
            if let Some(session) = session {
                crate::ddi::translation_session::detach_execution_resource(
                    session,
                    passive,
                    resource_id,
                );
            }
        }
        if owns_session_reference {
            // SAFETY: `new` consumed the exact strong reference and close runs
            // once. Role 1 instead borrows the still-live adjacent reply-pool
            // reference, which its caller releases only after this returns.
            if let Some(session) = session {
                unsafe { crate::ddi::translation_session::release_execution_session(session) };
            }
        }
    }
}

/// Move-only custody for one exact allocation use.  It is retained by the
/// staged immutable HNR2 batch until the real host terminal response.
pub(crate) struct OpenExecutionUse {
    owner: core::ptr::NonNull<OpenExecutionBinding>,
    pub(crate) allocation_generation: u64,
    pub(crate) resource_id: u32,
    pub(crate) transport_instance: u64,
    pub(crate) byte_size: u64,
    pub(crate) memory_type_index: u32,
    pub(crate) hvm1_role: u32,
    /// Present only for the exact role-1 K2a backing.  The pointer is captured
    /// from the canonical allocation while this open-allocation rundown guard
    /// is live; role 4 and every HWA2 object remain unmapped.
    pub(crate) kernel_va: Option<core::ptr::NonNull<u8>>,
}

// SAFETY: Drop touches only the binding's nonpaged spinlock/event.  The
// binding cannot be freed until CloseAllocation observes this guard returned.
unsafe impl Send for OpenExecutionUse {}

impl Drop for OpenExecutionUse {
    fn drop(&mut self) {
        let owner = unsafe { self.owner.as_ref() };
        let signal = {
            let mut state = owner.rundown.lock();
            debug_assert!(state.active != 0);
            state.active = state.active.saturating_sub(1);
            state.active == 0
        };
        if signal {
            unsafe { KeSetEvent(owner.drained.get(), 0, 0) };
        }
    }
}

/// Move-only custody obtained from the owning DeviceContext's bounded open set.
/// The optional execution guard proves the exact resource remains attached to
/// this HTS1 session; HOC1 deliberately has none and is retained by `outer`.
pub(crate) struct OpenOuterUse {
    open: core::ptr::NonNull<OpenAllocationContext>,
    execution: Option<OpenExecutionUse>,
    identity: OpenIdentity,
    allocation_offset: u64,
    /// The acquire site this guard came from, so `Drop` returns it to the same
    /// per-tag counter the join publishes.
    tag: u32,
}

unsafe impl Send for OpenOuterUse {}

impl OpenOuterUse {
    /// Does this guard pin the open allocation object at `open`? Pointer
    /// identity only — the same object `CloseAllocation` is tearing down.
    pub(crate) fn references_open(&self, open: usize) -> bool {
        self.open.as_ptr() as usize == open
    }

    pub(crate) fn generation(&self) -> u64 {
        self.identity.generation
    }

    pub(crate) fn resource_id(&self) -> Option<u32> {
        self.execution.as_ref().map(|guard| guard.resource_id)
    }

    /// Record that this use's blob was just named by a generated resource
    /// operand — the KMD half of the venus memory import.
    pub(crate) fn note_import_operand_substitution(&self) {
        // SAFETY: the open object outlives this use guard (dxgkrnl closes every
        // device-specific binding before DestroyAllocation), and `allocation`
        // is the canonical KMD allocation it was opened against.
        let allocation = unsafe { self.open.as_ref() }.allocation;
        if let Some(ctx) = (unsafe { resolve_alloc(allocation as HANDLE) }) {
            ctx.import_operand_substitutions
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Read the host's own copy of this allocation's pages.
    ///
    /// Only role-1 allocations that got one of the bounded system mappings are
    /// readable; everything else returns without recording, so a zero
    /// `Nr2BsNz` must be read together with `Nr2BsMap`.
    pub(crate) fn sample_backing_store(&self) {
        // SAFETY: as note_import_operand_substitution — the open object
        // outlives this use guard and `allocation` is its canonical allocation.
        let allocation = unsafe { self.open.as_ref() }.allocation;
        if let Some(ctx) = (unsafe { resolve_alloc(allocation as HANDLE) }) {
            sample_hvm1_backing(ctx);
        }
    }

}

/// Read the host's own copy of a host-visible allocation's pages.
///
/// `Nr2BsSeen` counts every role-1 allocation this was offered, so a zero
/// `Nr2BsNz` cannot be read as "the instrument never met one".
pub(crate) fn sample_hvm1_backing(ctx: &AllocationContext) {
    {
        if BS_SAMPLE_SCANS.load(Ordering::Relaxed) >= BS_SAMPLE_MAX_SCANS {
            return;
        }
        if ctx.kind != ALLOC_KIND_HVM1
            || ctx.hvm1_role != Hvm1Role::VulkanHostVisible.to_u32()
        {
            return;
        }
        BS_SAMPLE_SEEN.fetch_add(1, Ordering::Relaxed);
        if ctx.backing_store_state.load(Ordering::Acquire) != BACKING_STORE_BOUND {
            return;
        }
        let va = ctx.backing_store_va.load(Ordering::Relaxed);
        if va == 0 {
            return;
        }
        BS_SAMPLE_SCANS.fetch_add(1, Ordering::Relaxed);
        BS_SAMPLE_SIZE_KIB.store((ctx.size as u64 / 1024) as u32, Ordering::Relaxed);
        BS_SAMPLE_VA.store(va as u32, Ordering::Relaxed);
        // Stride across the WHOLE allocation rather than reading only its first
        // page: DXVK suballocates, so any one offset may simply be unused.
        let total_words = (ctx.size as usize) / 4;
        let stride = core::cmp::max(1, total_words / 1024);
        let mut found = 0u32;
        let mut poison = false;
        for index in (0..total_words).step_by(stride) {
            // SAFETY: `va` is this allocation's MDL system mapping, kept alive
            // by the guest blob resource that owns the MDL, and `index` stays
            // below `total_words`, the mapped size in dwords.
            let value = unsafe { core::ptr::read_volatile((va as *const u32).add(index)) };
            if value == BS_PROBE_POISON {
                poison = true;
                found = value;
                break;
            }
            if value != 0 && found == 0 {
                found = value;
            }
        }
        if found != 0 {
            BS_SAMPLE_NONZERO.fetch_add(1, Ordering::Relaxed);
            BS_SAMPLE_VALUE.store(found, Ordering::Relaxed);
        }
        if poison {
            BS_SAMPLE_POISON.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl OpenOuterUse {
    pub(crate) fn transport_instance(&self) -> Option<u64> {
        self.execution
            .as_ref()
            .map(|guard| guard.transport_instance)
    }

    /// Snapshot one submitted HOC1 extent into a caller-owned KMD-private DMA
    /// buffer.  The caller runs at PASSIVE_LEVEL (the exact context work item),
    /// while `self` retains both the device-specific open and its permanent
    /// kernel mapping.  No arbitrary GPUVA is dereferenced here: GPUVA
    /// resolution already selected this exact HOC1 open and produced the
    /// allocation-relative offset below.
    pub(crate) fn copy_hoc1(&self, total_bytes: u64, target: &mut DmaBuffer) -> bool {
        let Ok(total_bytes) = usize::try_from(total_bytes) else {
            return false;
        };
        if self.identity.kind != ALLOC_KIND_HOC1
            || self.execution.is_some()
            || total_bytes == 0
            || total_bytes != target.as_slice().len()
        {
            return false;
        }
        let open = unsafe { self.open.as_ref() };
        let Some(allocation) = (unsafe { resolve_alloc(open.allocation as HANDLE) }) else {
            return false;
        };
        if allocation.kind != ALLOC_KIND_HOC1
            || allocation.generation != self.identity.generation
            || allocation.backing_store_state.load(Ordering::Acquire) != BACKING_STORE_BOUND
        {
            return false;
        }
        let Some(source) =
            core::ptr::NonNull::new(allocation.backing_store_va.load(Ordering::Relaxed) as *mut u8)
        else {
            return false;
        };
        let Some(end) = self.allocation_offset.checked_add(total_bytes as u64) else {
            return false;
        };
        if end > self.identity.byte_size {
            return false;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                source.as_ptr().add(self.allocation_offset as usize),
                target.as_mut_slice().as_mut_ptr(),
                total_bytes,
            );
        }
        // The UMD seals the WC extent before pfnSubmitCommandCb.  Acquire keeps
        // all subsequent validation reads after the completed snapshot.
        core::sync::atomic::fence(Ordering::Acquire);
        true
    }
}

impl Drop for OpenOuterUse {
    fn drop(&mut self) {
        unsafe { self.open.as_ref() }.outer.release_tagged(self.tag);
    }
}

/// Try one exact open while the owning DeviceContext table lock prevents its
/// removal. `None` is both non-match and refusal; the caller requires exactly
/// one successful match and therefore rejects duplicates as well as absence.
pub(crate) unsafe fn acquire_outer_gpuva_use(
    open: usize,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    process: usize,
    gpuva: u64,
    bytes: u64,
    expected_generation: Option<u64>,
    require_hoc1: bool,
) -> Option<OpenOuterUse> {
    if process == 0 || gpuva == 0 || bytes == 0 {
        return None;
    }
    let end = gpuva.checked_add(bytes)?;
    let open = unsafe { open_allocation_context(open as HANDLE)? };
    let identity = open.identity?;
    if require_hoc1 != (identity.kind == ALLOC_KIND_HOC1)
        || expected_generation.is_some_and(|generation| generation != identity.generation)
        || !allocation_object::is_current(identity.generation)
    {
        return None;
    }
    let allocation = unsafe { resolve_alloc(open.allocation as HANDLE) }?;
    if allocation.generation != identity.generation || allocation.kind != identity.kind {
        return None;
    }
    let mappings = unsafe { allocation.outer_gpuva.load(Ordering::Acquire).as_ref() }?;
    let mappings = mappings.mappings.lock();
    let mut found = None;
    for mapping in mappings.as_slice() {
        let mapping_end = mapping.gpuva.checked_add(mapping.bytes)?;
        if mapping.process == process && gpuva >= mapping.gpuva && end <= mapping_end {
            let offset = mapping
                .allocation_offset
                .checked_add(gpuva.checked_sub(mapping.gpuva)?)?;
            if offset.checked_add(bytes)? > identity.byte_size || found.is_some() {
                return None;
            }
            found = Some(offset);
        }
    }
    let allocation_offset = found?;
    if !open.outer.acquire_tagged(OUTER_TAG_GPUVA) {
        return None;
    }
    let execution = if require_hoc1 {
        None
    } else {
        match open
            .execution
            .as_ref()
            .and_then(|binding| binding.acquire_attached(session, identity.generation))
        {
            Some(execution) => Some(execution),
            None => {
                open.outer.release_tagged(OUTER_TAG_GPUVA);
                return None;
            }
        }
    };
    Some(OpenOuterUse {
        open: core::ptr::NonNull::from(open),
        execution,
        identity,
        allocation_offset,
        tag: OUTER_TAG_GPUVA,
    })
}

/// Acquire the exact D3D11 allocation-list open after DeviceContext proved it
/// is a member of that device's bounded open set. No numeric allocation token,
/// resource id, name, PID, or pointer value is used to find another owner.
/// Name the arm [`acquire_outer_physical_use`] would refuse on, WITHOUT
/// acquiring. Diagnostic only, called on the already-failed path: eleven
/// silent `None` arms made the 2026-08-24 `UseMissingOrForeign` batch
/// refusals unattributable.
pub(crate) unsafe fn diagnose_outer_physical_use(
    open: HANDLE,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    expected_generation: u64,
    bytes: u64,
) -> u32 {
    if open.is_null() || expected_generation == 0 || bytes == 0 {
        return 2;
    }
    let Some(open) = (unsafe { open_allocation_context(open) }) else {
        return 3;
    };
    let Some(identity) = open.identity else {
        return 4;
    };
    if identity.kind == ALLOC_KIND_HOC1 {
        return 5;
    }
    if identity.generation != expected_generation {
        return 6;
    }
    if bytes > identity.byte_size {
        return 7;
    }
    if !allocation_object::is_current(identity.generation) {
        return 8;
    }
    let Some(allocation) = (unsafe { resolve_alloc(open.allocation as HANDLE) }) else {
        return 9;
    };
    if allocation.generation != identity.generation || allocation.kind != identity.kind {
        return 10;
    }
    let Some(binding) = open.execution.as_ref() else {
        return 11;
    };
    match binding.acquire_attached(session, identity.generation) {
        Some(execution) => {
            drop(execution);
            // Every arm passes in isolation: the refusal was the outer rundown
            // or a race; 12 marks "no arm reproduces".
            12
        }
        None => 13,
    }
}

pub(crate) unsafe fn acquire_outer_physical_use(
    open: HANDLE,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    expected_generation: u64,
    bytes: u64,
) -> Option<OpenOuterUse> {
    if open.is_null() || expected_generation == 0 || bytes == 0 {
        return None;
    }
    let open = unsafe { open_allocation_context(open)? };
    let identity = open.identity?;
    if identity.kind == ALLOC_KIND_HOC1
        || identity.generation != expected_generation
        || bytes > identity.byte_size
        || !allocation_object::is_current(identity.generation)
    {
        return None;
    }
    let allocation = unsafe { resolve_alloc(open.allocation as HANDLE) }?;
    if allocation.generation != identity.generation || allocation.kind != identity.kind {
        return None;
    }
    if !open.outer.acquire_tagged(OUTER_TAG_PHYSICAL) {
        return None;
    }
    let Some(execution) = open
        .execution
        .as_ref()
        .and_then(|binding| binding.acquire_attached(session, identity.generation))
    else {
        open.outer.release_tagged(OUTER_TAG_PHYSICAL);
        return None;
    };
    Some(OpenOuterUse {
        open: core::ptr::NonNull::from(open),
        execution: Some(execution),
        identity,
        allocation_offset: 0,
        tag: OUTER_TAG_PHYSICAL,
    })
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

/// Acquire generic open-object rundown from a pointer already selected under
/// the owning DeviceContext's bounded open-table lock.
pub(crate) unsafe fn acquire_outer_bind_guard(open: usize) -> Option<OuterBindGuard> {
    let open = unsafe { open_allocation_context(open as HANDLE)? };
    if !open.outer.acquire_tagged(OUTER_TAG_BIND) {
        return None;
    }
    Some(OuterBindGuard {
        open: core::ptr::NonNull::from(open),
    })
}

/// Non-null open-allocation handles that failed alignment or the magic check.
///
/// Must read 0: every handle reaching here came from our own
/// `DxgkDdiOpenAllocation`. Movement means dxgkrnl routed something else through
/// the present allocation list, or a handle outlived its CloseAllocation.
static OPEN_ALLOC_BAD_HANDLE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Count a refused handle without doing any diagnostics I/O.
///
/// D4 also resolves this object from the DMA-flip arm at DISPATCH_LEVEL, so the
/// resolver's complete transitive body must remain atomics-only.  The existing
/// PASSIVE allocation-counter dump mirrors the value instead.
fn refuse_open_allocation_handle() {
    OPEN_ALLOC_BAD_HANDLE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

/// Snapshot of the [`AllocationContext`] fields `BuildPagingBuffer` needs to
/// service content/placement ops against the CPU-visible BAR segment.
#[derive(Clone, Copy)]
pub(crate) struct PagingAllocInfo {
    pub resource_id: u32,
    pub size: u64,
    pub bar_eligible: bool,
    /// `HELIOS_HWA2_*` allocation kind. With `hvm1_role` it separates an
    /// ordinary D3D11 resource from the HVM1 pool, which the aperture census
    /// otherwise cannot tell apart — every `>= 1024`-page map it caught on
    /// 22.22.399.0 turned out to be HVM1.
    pub kind: u32,
    pub hvm1_role: u32,
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

/// Exact immutable facts the D2 display plane may read from one live
/// Windows allocation object.
///
/// The descriptor is the final create-output record, including the generation
/// this KMD stamped after constructing the backing.  `resource_id` is merely
/// the address of that same allocation's canonical OwnerTable row; callers get
/// no lookup or release authority from the scalar itself.
#[derive(Clone, Copy)]
pub(crate) struct DirectScanoutAllocationFacts {
    pub final_hwa2: HeliosWddmAllocationDescV2,
    pub resource_id: u32,
    pub allocation_generation: u64,
    pub transport_instance: u64,
    /// Exact host backing extent captured by the allocation object at create.
    /// Keeping it in this immutable projection lets DIRQL admission prove the
    /// HWA2 byte range without taking the canonical-owner DISPATCH spinlock.
    pub backing_size: u64,
    /// [`AllocationContext::import_operand_substitutions`] at projection time.
    pub import_substitutions: u32,
    /// Nonzero when the KMD built a real Venus `VkImage` for this allocation
    /// rather than a plain memory blob.
    pub venus_image: bool,
}

/// Exact K2a role-1 backing used by one HTS1 session transport.
///
/// The canonical allocation pointer comes from the device-specific ordinary
/// WDDM open retained by that session.  This projection never searches by
/// resource id, generation, name, process, geometry, or pool offset.
#[derive(Clone, Copy)]
pub(crate) struct K11ReplyPoolFacts {
    pub allocation_generation: u64,
    pub resource_id: u32,
    pub transport_instance: u64,
    pub kernel_va: core::ptr::NonNull<u8>,
    pub byte_size: u64,
}

#[derive(Clone, Copy)]
struct Hnr2ExecutionAllocationFacts {
    allocation_generation: u64,
    resource_id: u32,
    transport_instance: u64,
    byte_size: u64,
    memory_type_index: u32,
    hvm1_role: u32,
    kernel_va: Option<core::ptr::NonNull<u8>>,
}

/// Project the exact canonical allocation backing used for one HNR2 use.
///
/// HVM1 roles 1-3 are admitted only after K2a has release-published its
/// OS-owned backing and stable kernel view. Role 4 is the inverse contract: its
/// KMD-created pure-device-local HOST3D backing must exist while every K2a/CPU
/// mapping field remains absent. HWA2 admission is an exact non-standard,
/// resource-associated buffer/image opened directly on the submitting KMT
/// device. External D3D sharing is orthogonal: ordinary associated allocations
/// and C57 imports both require the same executor edge. No present/global
/// classification participates.
unsafe fn hnr2_execution_allocation_facts(
    allocation: usize,
) -> Option<Hnr2ExecutionAllocationFacts> {
    let ctx = unsafe { resolve_alloc(allocation as HANDLE) }?;
    if !allocation_object::is_current(ctx.generation)
        || ctx.resource_id.load(Ordering::Acquire) == 0
        || ctx.transport_instance == 0
    {
        return None;
    }
    let (byte_size, hvm1_role, kernel_va) = if ctx.kind == ALLOC_KIND_HVM1 {
        let byte_size = ctx.size as u64;
        let role = Hvm1Role::from_u32(ctx.hvm1_role)?;
        let valid_backing = match role {
            Hvm1Role::ReplyPool | Hvm1Role::VulkanHostVisible | Hvm1Role::Feedback => {
                ctx.backing_store_state.load(Ordering::Acquire) == BACKING_STORE_BOUND
                    && ctx.backing_store_va.load(Ordering::Relaxed) != 0
                    && matches!(ctx.size_provenance, BackingSize::SharedBackingStore(n) if n == byte_size)
            }
            Hvm1Role::VulkanDeviceLocal => {
                ctx.backing_store_state.load(Ordering::Acquire) == BACKING_STORE_UNBOUND
                    && ctx.backing_store_va.load(Ordering::Relaxed) == 0
                    && matches!(ctx.size_provenance, BackingSize::HostAuthoritative(n) if n == byte_size)
                    && ctx.venus_memory_id != 0
            }
        };
        if !valid_backing {
            return None;
        }
        let kernel_va = if role == Hvm1Role::ReplyPool {
            core::ptr::NonNull::new(ctx.backing_store_va.load(Ordering::Relaxed) as *mut u8)
        } else {
            None
        };
        (byte_size, ctx.hvm1_role, kernel_va)
    } else {
        let desc = ctx.final_hwa2?;
        let byte_size = desc.byte_size;
        let exact_outer = desc.allocation_generation == ctx.generation
            && helios_kmd_logic::outer_execution::hwa2_is_outer_execution_resource(&desc)
            && ctx.venus_alloc_size >= byte_size
            && ctx.venus_alloc_size != 0;
        if !exact_outer {
            return None;
        }
        (byte_size, 0, None)
    };
    Some(Hnr2ExecutionAllocationFacts {
        allocation_generation: ctx.generation,
        resource_id: ctx.resource_id.load(Ordering::Relaxed),
        transport_instance: ctx.transport_instance,
        byte_size,
        memory_type_index: ctx.memory_type_index,
        hvm1_role,
        kernel_va,
    })
}

/// Resolve K11's exact retained role-1 allocation to its completed K2a view.
///
/// # Safety
/// `allocation` must be the canonical allocation pointer captured by a live
/// `OpenAllocationContext`; CloseAllocation keeps it live for this call.
pub(crate) unsafe fn k11_reply_pool_facts(allocation: usize) -> Option<K11ReplyPoolFacts> {
    let ctx = unsafe { resolve_alloc(allocation as HANDLE) }?;
    // BOUND is the one release-published alias state.  Acquire it before
    // reading either member so a successful projection cannot combine a new
    // state word with stale resource/mapping fields.
    if ctx.backing_store_state.load(Ordering::Acquire) != BACKING_STORE_BOUND {
        return None;
    }
    let resource_id = ctx.resource_id.load(Ordering::Relaxed);
    let kernel_va =
        core::ptr::NonNull::new(ctx.backing_store_va.load(Ordering::Relaxed) as *mut u8)?;
    let byte_size = ctx.size as u64;
    if ctx.kind != ALLOC_KIND_HVM1
        || ctx.hvm1_role != Hvm1Role::ReplyPool.to_u32()
        || resource_id == 0
        || ctx.transport_instance == 0
        || !allocation_object::is_current(ctx.generation)
        || byte_size != helios_protocol::native_render::HELIOS_HVM1_REPLY_POOL_BYTES
        || !matches!(ctx.size_provenance, BackingSize::SharedBackingStore(n) if n == byte_size)
    {
        return None;
    }
    Some(K11ReplyPoolFacts {
        allocation_generation: ctx.generation,
        resource_id,
        transport_instance: ctx.transport_instance,
        kernel_va,
        byte_size,
    })
}

/// Claim one canonical role-1 allocation for one exact live SessionObject.
/// This prevents two sessions from attaching distinct host contexts to a shared
/// reply carrier even if a caller deliberately re-opens the shared WDDM
/// resource on another raw device.
pub(crate) unsafe fn claim_k11_reply_pool_session(
    allocation: usize,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
) -> bool {
    let Some(ctx) = (unsafe { resolve_alloc(allocation as HANDLE) }) else {
        return false;
    };
    if ctx.kind != ALLOC_KIND_HVM1
        || ctx.hvm1_role != Hvm1Role::ReplyPool.to_u32()
        || !allocation_object::is_current(ctx.generation)
    {
        return false;
    }
    ctx.k11_session_binding
        .compare_exchange(
            0,
            session.as_ptr() as usize,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

/// Release the exact direct-object claim after host work has been revoked and
/// the session context destroyed. A mismatch is an ownership failure; no other
/// session is guessed or recovered.
pub(crate) unsafe fn release_k11_reply_pool_session(
    allocation: usize,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
) -> bool {
    let Some(ctx) = (unsafe { resolve_alloc(allocation as HANDLE) }) else {
        return false;
    };
    ctx.k11_session_binding
        .compare_exchange(
            session.as_ptr() as usize,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

/// Resolve the exact OS-supplied `hAllocation` to its final HWA2 facts.
///
/// Returns `None` for a null, foreign, stale, HVM1, HOC1, or backing-less
/// allocation.  There is intentionally no resource-id-to-allocation reverse
/// lookup: the Windows handle is the identity and the allocation object is the
/// sole source of the descriptor/backing association.
///
/// # Safety
/// `h` must be an allocation handle supplied by dxgkrnl for the duration of a
/// DDI that keeps the allocation live.
pub(crate) unsafe fn direct_scanout_allocation_facts(
    h: HANDLE,
) -> Option<DirectScanoutAllocationFacts> {
    let ctx = unsafe { resolve_alloc(h) }?;
    let final_hwa2 = ctx.final_hwa2?;
    let resource_id = ctx.resource_id.load(Ordering::Acquire);
    if resource_id == 0
        || ctx.transport_instance == 0
        || final_hwa2.allocation_generation != ctx.generation
    {
        return None;
    }
    Some(DirectScanoutAllocationFacts {
        final_hwa2,
        resource_id,
        allocation_generation: ctx.generation,
        transport_instance: ctx.transport_instance,
        backing_size: ctx.venus_alloc_size,
        import_substitutions: ctx.import_operand_substitutions.load(Ordering::Relaxed),
        venus_image: ctx.venus_image_id != 0,
    })
}

/// Resolve a live device-specific open handle to the exact KMD allocation that
/// dxgkrnl associated with it at OpenAllocation.
///
/// There is deliberately no resource-id, dimension, list-order, or current-
/// scanout fallback.  A missing association is a typed refusal.
///
/// # Safety
/// `h` must be an `hDeviceSpecificAllocation` supplied by dxgkrnl for the
/// duration of a DDI that keeps the open object live.
pub(crate) unsafe fn open_direct_scanout_allocation_facts(
    h: HANDLE,
) -> Option<(HANDLE, DirectScanoutAllocationFacts)> {
    let open = unsafe { open_allocation_context(h) }?;
    if open.allocation == 0 {
        return None;
    }
    let allocation = open.allocation as HANDLE;
    let facts = unsafe { direct_scanout_allocation_facts(allocation) }?;
    Some((allocation, facts))
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
        resource_id: ctx.resource_id(),
        size: ctx.size as u64,
        bar_eligible: ctx.bar_eligible,
        kind: ctx.kind,
        hvm1_role: ctx.hvm1_role,
    })
}

/// Apply one authoritative leaf GPUVA update to the exact allocation object it
/// names.  The state exists only for allocations opened on a D3D device; no
/// process/global address-space table is created.
pub(crate) unsafe fn update_outer_gpuva_mapping(
    update: &DXGK_BUILDPAGINGBUFFER_UPDATEPAGETABLE,
) -> bool {
    if update.PageTableLevel != 0 || update.NumPageTableEntries == 0 {
        return true;
    }
    let Some(allocation) = (unsafe { resolve_alloc(update.hAllocation) }) else {
        return true;
    };
    let state = allocation.outer_gpuva.load(Ordering::Acquire);
    let Some(state) = (unsafe { state.as_ref() }) else {
        return true;
    };
    if update.hProcess.is_null() || update.pPageTableEntries.is_null() {
        return false;
    }
    let process = update.hProcess as usize;
    let page_count = update.NumPageTableEntries as u64;
    let Some(update_bytes) = page_count.checked_mul(PAGE as u64) else {
        return false;
    };
    let Some(update_end) = update.FirstPteVirtualAddress.checked_add(update_bytes) else {
        return false;
    };
    let repeat = update.Flags.Repeat() != 0;
    let pte_valid = |index: usize| {
        let pte_index = if repeat { 0 } else { index };
        let pte = unsafe { core::ptr::read_unaligned(update.pPageTableEntries.add(pte_index)) };
        let bits = unsafe { pte.__bindgen_anon_1.__bindgen_anon_1 };
        bits.Valid() != 0 && bits.Zero() == 0
    };

    let mut runs = 0usize;
    let mut in_run = false;
    for i in 0..update.NumPageTableEntries as usize {
        let valid = pte_valid(i);
        if valid {
            let Some(end) = update
                .AllocationOffsetInBytes
                .checked_add((i as u64 + 1) * PAGE as u64)
            else {
                return false;
            };
            if end > allocation.size as u64 {
                return false;
            }
            if !in_run {
                runs = runs.saturating_add(1);
                in_run = true;
            }
        } else {
            in_run = false;
        }
    }
    if runs > OUTER_GPUVA_MAX_RANGES {
        return false;
    }

    let mut mappings = state.mappings.lock();
    mappings.retain(|mapping| {
        if mapping.process != process {
            return true;
        }
        let mapping_end = mapping.gpuva.saturating_add(mapping.bytes);
        mapping_end <= update.FirstPteVirtualAddress || mapping.gpuva >= update_end
    });
    if !mappings.can_fit(runs) {
        return false;
    }

    let mut run_start = None;
    for i in 0..=update.NumPageTableEntries as usize {
        let valid = i < update.NumPageTableEntries as usize && pte_valid(i);
        match (run_start, valid) {
            (None, true) => run_start = Some(i),
            (Some(start), false) => {
                let count = i - start;
                let mapping = OuterGpuVaMapping {
                    process,
                    gpuva: update.FirstPteVirtualAddress + start as u64 * PAGE as u64,
                    allocation_offset: update.AllocationOffsetInBytes + start as u64 * PAGE as u64,
                    bytes: count as u64 * PAGE as u64,
                };
                if !mappings.push(mapping) {
                    return false;
                }
                run_start = None;
            }
            _ => {}
        }
    }
    true
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
/// touched. Segment placement and access flags are kept in one value so a
/// CPU-visible local allocation cannot omit its aperture fallback.
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
/// topology; `local_seg_id()` is the same question asked about local memory, and
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

/// The placement for an ordinary HWA2 allocation.
///
/// The local memory segment has no CPU mapping service. Every CPU-visible HWA2
/// allocation that prefers it therefore also names the ordinary aperture, so
/// VidMm can migrate content before supplying a CPU view. The BAR content engine
/// owns those transfers; no MapCpuHostAperture callback or fixed-offset mapper
/// participates.
fn vidmm_placement(
    bar_eligible: bool,
    local_seg_id: Option<u32>,
    is_primary: bool,
    cpu_visible: bool,
    segment_only: bool,
) -> VidMmPlacement {
    let aperture_bit = segment_bit(crate::ddi::gpummu::APERTURE_SEGMENT_ID);

    let (preferred_segment, supported_segments) =
        if let (true, Some(seg_id)) = (bar_eligible, local_seg_id) {
            // The aperture stays in the supported set by default. `BarSegOnly`
            // removes it, which is the only shape that FORCES a CPU lock
            // through the aperture DDI: DxgKrnl ETW (2026-08-30) shows VidMm
            // otherwise answering the lock by evicting the allocation out of
            // segment 2 into an aperture segment. Removing it alone was
            // measured at E_INVALIDARG on .394-.396; see the knob for why that
            // measurement does not settle the pair with `BarSegFlagsX=4`.
            let supported = if segment_only {
                segment_bit(seg_id)
            } else {
                segment_bit(seg_id) | aperture_bit
            };
            (seg_id, supported)
        } else {
            (crate::ddi::gpummu::APERTURE_SEGMENT_ID, aperture_bit)
        };
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
/// `ShareBackingStoreWithKmd` requires system-memory-only placement. The role
/// table therefore names the reported aperture as both preferred and solely
/// supported; no local-memory alternative can silently take custody of these bytes.
/// [`admit_hvm1`] verifies that exact segment before publishing an allocation.
fn hvm1_placement(role: Hvm1Role) -> VidMmPlacement {
    let contract = role.placement();
    let preferred = contract.preferred_segment;
    VidMmPlacement {
        preferred_segment: preferred,
        supported_segments: segment_bit(preferred),
        cpu_visible: contract.cpu_visible,
        // §10.7:2003-2005 — `Cached = 0` for every role, and the CPU publication
        // ordering proof depends on it: a `HOST_CACHED` mapping would break the
        // `HOST_VISIBLE|HOST_COHERENT` advertisement (§18.1:4763-4766). NOT
        // subject to the `AllocCached` knob, which has no authority here.
        cached: contract.cached,
        // §10.7:2007-2010 — truthful, not a contiguity request: the legacy
        // physical Render engine really does dereference the final
        // segment/physical-address capability patched from the allocation list.
        // ⚠ If K6's Render/Patch path ever stops dereferencing it, this bit
        // becomes a lie even though it is set exactly as specified.
        accessed_physically: contract.accessed_physically,
        // NOT maskable: this is part of the fixed HVM1 residency contract.
        explicit_residency_notification: contract.explicit_residency_notification,
        disable_partial_residency: contract.disable_partial_residency,
        restricted_to_single_segment: contract.restricted_to_single_segment,
    }
}

/// Current HOC1 placement.
///
/// The UMD12 pool is CPU-visible/write-combined and obtains its exact KMD view
/// through `ShareBackingStoreWithKmd`. A7 snapshots sealed extents directly
/// from that allocation object; there is no HPM1 page-table executor and no
/// renderer resource. The ordinary aperture is therefore both preferred and
/// solely supported. HOC1 deliberately does not claim `AccessedPhysically`:
/// no physical capability is patched for this allocation.
fn hoc1_placement() -> VidMmPlacement {
    VidMmPlacement {
        preferred_segment: crate::ddi::gpummu::APERTURE_SEGMENT_ID,
        supported_segments: segment_bit(crate::ddi::gpummu::APERTURE_SEGMENT_ID),
        // Nonprimary, allocation-private and CPU-visible/WC.
        cpu_visible: true,
        // Write-combined, and the D3D12 UMD's whole seal protocol depends on it:
        // §18.2:4946-4949 rejects device qualification if the returned mapping's
        // cache policy is not WC, because a cached mapping makes the x64 drain
        // that seals an HOB1 unobservable.
        cached: false,
        accessed_physically: false,
        // HVM1's Flags2/residency-notification set does not apply to HOC1.
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
    seq: u32,
) {
    sample_hvm1_backing(&ctx);
    let allocation_handle = (&*ctx as *const AllocationContext) as usize;
    // Retire the exact Windows/KMD allocation identity before any backing
    // resource or Venus image can be torn down. Ambiguous plane retirement
    // retains the backing until the verified reset barrier.
    // Step 2 is the one the static sweep singled out: `retire_allocation` takes
    // the scanout mutex, an untimed non-recursive SynchronizationEvent, and it
    // is the only unbounded wait reachable on this path.
    step(b"DaStep", seq, 2);
    if !super::direct_scanout::retire_allocation(
        passive,
        adapter,
        allocation_handle as HANDLE,
        ctx.generation,
        ctx.resource_id(),
    ) {
        drop(ctx);
        return;
    }

    step(b"DaStep", seq, 3);
    if ctx.resource_id() != 0 {
        // OwnerTable is the sole resource/backing owner. Terminal UNREF
        // extracts and runs the exact image/memory finalizer outside the owner
        // lock; an ambiguous detach or unref retains the row until reset.
        step(b"DaStep", seq, 4);
        let _ = crate::virtio::ctrl::forget_allocation_blob(passive, adapter, ctx.resource_id());
        step(b"DaStep", seq, 5);
        if crate::virtio::ctrl::ctx_detach_resource(passive, adapter, ctx.ctx_id, ctx.resource_id())
            .is_ok()
        {
            step(b"DaStep", seq, 6);
            let _ = crate::virtio::ctrl::resource_unref(passive, adapter, ctx.resource_id());
        }
    }
    step(b"DaStep", seq, 7);
    drop(ctx);
    step(b"DaStep", seq, 8);
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
/// Why it is a type rather than a bool at the call site: the local-memory
/// content engine may map bytes only when the host reported the backing extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackingSize {
    /// The host allocated it and reported the size back.
    HostAuthoritative(u64),
    /// The size is valid for accounting, but not authoritative for BAR mapping.
    NonHostAuthoritative(u64),
    /// Exact OS-owned section extent shared with KMD through the WDDM 3.1+
    /// backing-store callback. It is neither a BAR nor Venus allocation size.
    SharedBackingStore(u64),
}

impl BackingSize {
    pub(crate) fn bytes(self) -> u64 {
        match self {
            Self::HostAuthoritative(n)
            | Self::NonHostAuthoritative(n)
            | Self::SharedBackingStore(n) => n,
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
/// Passing an HWA2 word straight into `create_optimal_gdi_image`
/// would silently drop SAMPLED and add nothing, so this translation is
/// load-bearing rather than cosmetic.
///
/// Only the three bits `create_optimal_gdi_image` reads are mapped;
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
/// must stamp the same direct-scanout fact. Two independent spellings is exactly
/// the defect this closes — the derivation used to key on `swizzle_class ==
/// HELIOS_HWA2_SWIZZLE_LINEAR`, which is TRUE precisely when the producer said
/// "copy me": `umd/src/forward/alloc.rs` sets `DISPLAYABLE` **and**
/// `OPAQUE_OPTIMAL` together on the direct arm and plain `LINEAR` on the copy
/// arm. Inverted, the zero-copy primary never received the direct-scanout fact
/// and the copy-path primary did, so `set_vidpn_source_address` took the wrong
/// arm for both.
///
/// `HELIOS_HWA2_FLAG_DISPLAYABLE` is the successor of the retired
/// `HELIOS_WDDM_ALLOC_MISC_DIRECT_SCANOUT` bit, which the pre-retirement code
/// read directly (`41d13f8:create_allocation.rs:2584`). It is the producer's
/// claim, and it is the only field that carries it.
///
/// ⛔ `STANDARD` excludes the KMD's OWN shared primary, which sets `PRIMARY |
/// DISPLAYABLE` too (`dxgkddi_get_standard_allocation_driver_data`) and means
/// something different by it: those bytes ARE the adapter's LINEAR scan-out
/// image; it is not an ordinary direct-scanout primary.
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
    /// `DXGK_CREATEALLOCATIONFLAGS::Value`. The binding defines exactly one
    /// named bit (`Resource`) plus `Reserved`, so this is the whole word.
    flags: u32,
    /// Whether `DXGKARG_CREATEALLOCATION::hResource` was non-null on entry.
    has_resource_handle: bool,
    /// `DXGKARG_CREATEALLOCATION::NumAllocations`.
    num_allocations: u32,
    /// `DXGKARG_CREATEALLOCATION::PrivateDriverDataSize` — the RESOURCE-level
    /// private data. Both records require zero of it.
    resource_private_size: u32,
}

impl CreateCallShape {
    /// The exact unshared HOC1 shape.
    fn is_bare_single_allocation(self) -> bool {
        self.flags == 0
            && !self.has_resource_handle
            && self.num_allocations == 1
            && self.resource_private_size == 0
    }

    /// `CreateShared` requires `CreateResource`. Dxgkrnl exposes the latter as
    /// the KMD `Resource` bit and supplies a null input handle for a new
    /// one-allocation resource.
    fn is_shared_single_resource_allocation(self) -> bool {
        self.flags == DXGK_CREATEALLOCATION_FLAG_RESOURCE
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
    hvm1_role: u32,
    /// Ask dxgkrnl for the exact OS-owned CPU backing after create. HVM1 uses
    /// it to construct the stock Venus guest-memory view; HOC1 uses the same
    /// callback only to retain a KMD read view of the immutable command pool.
    /// Neither use turns the backing pointer into identity.
    share_backing_store: bool,
    generation: u64,
    /// Final HWA2 create-output bytes for the exact allocation object. HVM1
    /// and HOC1 use distinct records and therefore carry `None`.
    final_hwa2: Option<HeliosWddmAllocationDescV2>,
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

// HWA2 allocations already own the Venus backing created by `build_backing`.
// Requesting an additional OS shared backing makes dxgkrnl call
// `DxgkDdiSetAllocationBackingStore`, whose exact contract is intentionally
// limited to HVM1 and HOC1. Keep this a compile-time invariant: the accidental
// `true` value made every first D3D11 buffer fail its enclosing pfnAllocateCb
// with E_INVALIDARG after an otherwise successful HWA2 CreateAllocation.
const HWA2_SHARE_BACKING_STORE_WITH_KMD: bool = false;
const _: () = assert!(!HWA2_SHARE_BACKING_STORE_WITH_KMD);

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
    if claims_standard
        && created.venus_alloc_size != 0
        && created.venus_alloc_size != authored_byte_size
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
        desc.planes[0].slice_pitch = created.pitch.saturating_mul(desc.height).min(
            u32::try_from(desc.byte_size.saturating_sub(created.plane_offset)).unwrap_or(u32::MAX),
        );
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
    if !claims_standard
        && created.venus_alloc_size != 0
        && created.venus_alloc_size < desc.byte_size
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

    let local_seg_id = adapter.local_segment().map(|segment| segment.seg_id);
    // Keep the complete local-placement policy in host-testable logic. In
    // particular, HWA2 `SHARED` resources must remain aperture-only: the
    // pre-retirement adopted-resource path deliberately excluded this class
    // after local placement destabilized LogonUI/DWM, and dxgkrnl now rejects
    // the same shape during pfnAllocateCb after Create/Open have succeeded.
    // Ordinary CPU-visible linear allocations retain local placement.
    let bar_eligible =
        helios_kmd_logic::allocation_placement::hwa2_may_prefer_local_memory(
            &desc,
            created.blob_size.is_host_authoritative(),
            local_seg_id.is_some(),
            adapter.knobs().bar_local_shared,
        );
    let placement = vidmm_placement(
        bar_eligible,
        local_seg_id,
        is_primary,
        desc.has_flag(HELIOS_HWA2_FLAG_CPU_VISIBLE),
        adapter.knobs().bar_segment_only,
    );

    // VidMm is charged the LARGER of the descriptor's extent and the backing the
    // host actually produced. The descriptor is const and may under-state a
    // host-rounded image requirement; charging the smaller number would make
    // VidMm account less storage than the content engine owns.
    let charged = created.blob_size.bytes().max(desc.byte_size);
    let vidmm_size = round_up_page(if charged == 0 {
        PAGE
    } else {
        charged as SIZE_T
    });

    Ok(AdmittedAllocation {
        kind: desc.allocation_kind,
        hvm1_role: 0,
        share_backing_store: HWA2_SHARE_BACKING_STORE_WITH_KMD,
        generation,
        final_hwa2: Some(desc),
        vidmm_size,
        placement,
        // ⭐ THE SAME predicate `classify_hwa2` routed the backing with, so the
        // direct-scanout fact names exactly the allocation that was given a
        // bindable OPTIMAL image. It reads the producer's
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

/// Admit one HVM1 create-input record (§10.7) and request an OS-owned shared
/// backing store. `DxgkDdiSetAllocationBackingStore` creates the renderer view
/// later in the same allocation transaction, after dxgkrnl supplies its exact
/// system-space address. A1 and the probes exercise roles 1-3; the wider A3
/// renderer cutover remains a later unit.
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

    // A shared allocation must create a resource. The KMD sees that ordinary
    // resource association as DXGK_CREATEALLOCATIONFLAGS::Resource; the UMD
    // retains the returned process-local resource handle only for teardown.
    //
    // ⚠ The `pSystemMem = NULL` / `ExistingSysMem` / `CreateShared` /
    // `NtSecuritySharing` / `ExistingKernelSysMem` / `ExistingSection` /
    // `PermanentSysMem` half of that list is a `D3DDDI_ALLOCATIONINFO` /
    // `D3DKMT_CREATEALLOCATION` property that dxgkrnl consumes and does NOT
    // surface to the miniport: `DXGKARG_CREATEALLOCATION` carries only
    // `Flags{Resource, Reserved}`, `NumAllocations`, `hResource`, and the two
    // private-data pairs (grep `_DXGKARG_CREATEALLOCATION`). So this check
    // covers the four properties the KMD can see and CANNOT cover the other
    // seven — recorded here rather than implied. `CreateResource` is the one
    // outer property represented in the KMD call and is required exactly once.
    if !shape.is_shared_single_resource_allocation() {
        bump(&CREATE_CALL_SHAPE, b"AcShape");
        return Err(STATUS_INVALID_PARAMETER);
    }

    let cpu_visible = role.placement().cpu_visible;
    if cpu_visible && !adapter.share_backing_store_with_kmd() {
        bump_with_code(
            &CREATE_HVM1_MEMORY_CLASS_REFUSED,
            b"AcHvm1Mem",
            role.to_u32(),
        );
        return Err(STATUS_NOT_SUPPORTED);
    }
    if record.byte_size == 0
        || record.byte_size & (PAGE as u64 - 1) != 0
        || record.byte_size > u32::MAX as u64
    {
        bump(&CREATE_SIZE_REJECT, b"AcSize");
        return Err(STATUS_INVALID_PARAMETER);
    }
    // K2a imports one finite udmabuf scatter list.  The stock Linux limit is
    // exactly 1024 pages, so every CPU-visible HVM1 allocation is capped at
    // 4 MiB rather than relying on the importer to truncate or on an unproved
    // larger bound.  Role 4 never enters udmabuf and is bounded separately by
    // the u32/page-granular HVM1 contract above.
    const HVM1_CPU_VISIBLE_MAX_BYTES: u64 = 1024 * PAGE as u64;
    if cpu_visible && record.byte_size > HVM1_CPU_VISIBLE_MAX_BYTES {
        bump(&CREATE_SIZE_REJECT, b"AcSize");
        return Err(STATUS_NOT_SUPPORTED);
    }
    let placement = hvm1_placement(role);
    if !segment_is_reported(adapter, placement.preferred_segment) {
        let (counter, name) = role_segment_absent_counter(role);
        bump(counter, name);
        return Err(STATUS_NOT_SUPPORTED);
    }

    // Roles 1-3 receive their one OS-owned K2a backing later through
    // SetAllocationBackingStore.  Role 4 is the deliberate opposite: create a
    // non-mappable, shareable HOST3D blob now, backed by a pure DEVICE_LOCAL
    // VkDeviceMemory type.  There is no host-visible or memory-class fallback.
    let backing = if matches!(role, Hvm1Role::VulkanDeviceLocal) {
        match adapter.with_venus_client(passive, |client| {
            client.allocate_device_local_memory_blob(adapter, record.byte_size)
        }) {
            Ok(Ok(blob)) => Some(CreatedBacking {
                resource_id: blob.res_id,
                venus_memory_id: blob.blob_id,
                venus_image_id: 0,
                pitch: 0,
                plane_offset: 0,
                venus_alloc_size: blob.size,
                memory_type_index: blob.memory_type_index,
                blob_size: BackingSize::HostAuthoritative(blob.size),
            }),
            Ok(Err(_)) => {
                bump_with_code(
                    &CREATE_HVM1_MEMORY_CLASS_REFUSED,
                    b"AcHvm1Mem",
                    role.to_u32(),
                );
                return Err(STATUS_NOT_SUPPORTED);
            }
            Err(_) => {
                bump(&CREATE_BACKING_FAILED, b"AcBackFail");
                return Err(STATUS_DEVICE_NOT_READY);
            }
        }
    } else {
        None
    };

    let Some(generation) = allocation_object::mint() else {
        bump(&CREATE_GENERATION_EXHAUSTED, b"AcGenExh");
        if let Some(created) = backing.as_ref() {
            release_orphan_backing(passive, adapter, created);
        }
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
        if let Some(created) = backing.as_ref() {
            release_orphan_backing(passive, adapter, created);
        }
        return Err(STATUS_INVALID_PARAMETER);
    }
    // SAFETY: length proven exactly `HELIOS_HVM1_SIZE` above; the runtime owns a
    // writable buffer of that length for the call's duration.
    let out = unsafe { core::slice::from_raw_parts_mut(private, private_size) };
    out.copy_from_slice(bytes_of(&record));

    Ok(AdmittedAllocation {
        kind: ALLOC_KIND_HVM1,
        hvm1_role: role.to_u32(),
        share_backing_store: cpu_visible,
        generation,
        final_hwa2: None,
        vidmm_size: backing.as_ref().map_or(record.byte_size, |created| {
            created.blob_size.bytes().max(record.byte_size)
        }) as SIZE_T,
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
        // Shared-backing HVM1 is aperture-only.
        bar_eligible: false,
        size_provenance: backing.as_ref().map_or(
            BackingSize::SharedBackingStore(record.byte_size),
            |created| created.blob_size,
        ),
        backing,
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
    // F5 declined HPM1, so the existing WDDM 3.2 shared-backing callback is the
    // only authorized CPU view from which KMD can snapshot a sealed HOC1 extent.
    // This is not an HVM1 renderer view: no virtio/Venus resource is created,
    // and the pointer is retained only on this exact allocation object.
    if !adapter.share_backing_store_with_kmd() {
        bump(&CREATE_HOC1_REJECT, b"AcHoc1Rej");
        return Err(STATUS_NOT_SUPPORTED);
    }
    // Keep the same runtime segment-table cross-check as HVM1: a missing
    // aperture is a named refusal before dxgkrnl sees an impossible placement.
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
    // The KMD writes one immutable allocation generation. A7 later reaches the
    // exact pool only through the device/context/allocation ownership graph.
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
        hvm1_role: 0,
        share_backing_store: true,
        generation,
        final_hwa2: None,
        vidmm_size: round_up_page(record.byte_size as SIZE_T),
        // Validated by the segment check above; computed once so the placement
        // that was validated is the placement that ships.
        placement,
        // No Venus backing: this pool is not an HVM1 renderer resource. K2a's
        // OS-owned shared backing is retained directly on the allocation object
        // for A7 snapshotting; creating a renderer object would duplicate it.
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
    if crate::virtio::KMD_D2_OWNER_ENABLED {
        if crate::virtio::ctrl::ctx_detach_resource(
            passive,
            adapter,
            adapter.venus_ctx_id(),
            created.resource_id,
        )
        .is_ok()
        {
            let _ = crate::virtio::ctrl::resource_unref(passive, adapter, created.resource_id);
        }
        return;
    }
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

    let ctx = Box::new(AllocationContext {
        magic: ALLOCATION_CTX_MAGIC,
        ctx_id: adapter.venus_ctx_id(),
        resource_id: AtomicU32::new(resource_id),
        backing_store_state: AtomicU32::new(BACKING_STORE_UNBOUND),
        backing_store_va: AtomicUsize::new(0),
        backing_store_mdl: AtomicUsize::new(0),
        outer_gpuva: AtomicPtr::new(core::ptr::null_mut()),
        k11_session_binding: AtomicUsize::new(0),
        transport_instance: if crate::virtio::KMD_D2_OWNER_ENABLED {
            adapter
                .with_virtio(|gpu| gpu.scanout_transport_instance())
                .unwrap_or(0)
        } else {
            0
        },
        generation: admitted.generation,
        kind: admitted.kind,
        hvm1_role: admitted.hvm1_role,
        final_hwa2: admitted.final_hwa2,
        venus_memory_id: backing.map_or(0, |b| b.venus_memory_id),
        venus_image_id: backing.map_or(0, |b| b.venus_image_id),
        import_operand_substitutions: AtomicU32::new(0),
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
        bar_eligible: admitted.bar_eligible,
        size_provenance: admitted.size_provenance,
    });

    // ── VidMm metadata: segment placement + CPU visibility ──────────────────
    info.hAllocation = Box::into_raw(ctx) as HANDLE;
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
        // HVM1 alone requests explicit residency notification; ordinary D3D11
        // placement remains byte-identical.
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
        // These two residency constraints remain HVM1-only. The adjacent
        // shared-backing bit is additionally gated by the admitted OS feature.
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
        if admitted.share_backing_store {
            info.Flags2
                .__bindgen_anon_1
                .__bindgen_anon_1
                .set_ShareBackingStoreWithKmd(1);
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
    // `input_resource`, not `args.hResource`: a newly created HVM1 resource and
    // bare HOC1 both require a null input handle. A handle this DDI minted is
    // output state, so reading it here would answer the wrong question.
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
                    // seq 0 marks the CreateAllocation unwind, so a `DaStep`
                    // reading cannot be mistaken for the DestroyAllocation DDI.
                    unsafe { destroy_allocation_ctx(passive, adapter, ctx, 0) };
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

    // PASSIVE bounded dump for allocation counters that must not perform
    // registry I/O from their measured success paths.
    dump_alloc_counters();
    STATUS_SUCCESS
}

/// Bind the OS-owned shared section to the exact HVM1 allocation and import its
/// locked PFNs as a guest-memory Venus blob. No user VA or process identity is
/// carried: Lock2 maps the same section in the exact calling process by the
/// ordinary WDDM allocation lifetime.
const K2A_MDL_MAP_PRIORITY: u32 = 16 | 0x4000_0000; // NormalPagePriority | NX
const K2A_MDL_HAS_SYSTEM_VA: i16 = 0x0001 | 0x0004;

/// Build/reuse the stable kernel view of a successfully locked K2a MDL.
/// `MmUnlockPages` in the canonical resource finalizer releases a system mapping
/// made by this `MmGetSystemAddressForMdlSafe`-equivalent pattern.
unsafe fn k2a_mdl_system_va(mdl: PMDL, expected_bytes: u64) -> Option<usize> {
    unsafe { k2a_mdl_system_va_cached(mdl, expected_bytes, _MEMORY_CACHING_TYPE::MmCached) }
}

/// As [`k2a_mdl_system_va`], with the caching type stated.
///
/// ⚠ It must match how the guest maps the same pages. `Hvm1Placement::cached`
/// is false for every role, so a role-1 allocation is write-combined on the
/// guest side, and a cached kernel alias of it is not a trustworthy reader.
///
/// # Safety
/// As [`k2a_mdl_system_va`].
unsafe fn k2a_mdl_system_va_cached(
    mdl: PMDL,
    expected_bytes: u64,
    caching: MEMORY_CACHING_TYPE,
) -> Option<usize> {
    if mdl.is_null() || u64::from(unsafe { (*mdl).ByteCount }) != expected_bytes {
        return None;
    }
    if unsafe { (*mdl).MdlFlags } & K2A_MDL_HAS_SYSTEM_VA != 0 {
        return core::ptr::NonNull::new(unsafe { (*mdl).MappedSystemVa } as *mut u8)
            .map(|va| va.as_ptr() as usize);
    }
    let va = unsafe {
        MmMapLockedPagesSpecifyCache(
            mdl,
            0, // KernelMode
            caching,
            core::ptr::null_mut(),
            0, // BugCheckOnFailure = FALSE
            K2A_MDL_MAP_PRIORITY,
        )
    };
    core::ptr::NonNull::new(va as *mut u8).map(|va| va.as_ptr() as usize)
}

unsafe fn hoc1_mdl_system_va(mdl: PMDL, expected_bytes: u64) -> Option<usize> {
    if mdl.is_null() || u64::from(unsafe { (*mdl).ByteCount }) != expected_bytes {
        return None;
    }
    if unsafe { (*mdl).MdlFlags } & K2A_MDL_HAS_SYSTEM_VA != 0 {
        return core::ptr::NonNull::new(unsafe { (*mdl).MappedSystemVa } as *mut u8)
            .map(|va| va.as_ptr() as usize);
    }
    let va = unsafe {
        MmMapLockedPagesSpecifyCache(
            mdl,
            0,
            // HOC1 is declared WRITE_COMBINED. Preserve that cache type for
            // the KMD alias rather than creating a conflicting UC mapping.
            _MEMORY_CACHING_TYPE::MmWriteCombined,
            core::ptr::null_mut(),
            0,
            K2A_MDL_MAP_PRIORITY,
        )
    };
    core::ptr::NonNull::new(va as *mut u8).map(|va| va.as_ptr() as usize)
}

pub unsafe extern "C" fn dxgkddi_set_allocation_backing_store(
    h_adapter: IN_CONST_HANDLE,
    set_backing: IN_CONST_PDXGKARG_SETALLOCATIONBACKINGSTORE,
) -> NTSTATUS {
    if h_adapter.is_null() || set_backing.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if unsafe { KeGetCurrentIrql() } != crate::irql::PASSIVE_LEVEL_IRQL {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    if !adapter.share_backing_store_with_kmd() {
        return STATUS_NOT_SUPPORTED;
    }
    let args = unsafe { &*set_backing };
    if args.pBackingStore.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let Some(ctx) = (unsafe { resolve_alloc(args.hDriverAllocation) }) else {
        return STATUS_INVALID_HANDLE;
    };
    if ctx.kind == ALLOC_KIND_HOC1 {
        let bytes = ctx.size as u64;
        if bytes != HELIOS_HOC1_POOL_BYTES
            || !allocation_object::is_current(ctx.generation)
            || ctx.resource_id() != 0
            || bytes > u32::MAX as u64
            || (args.pBackingStore as usize) & (PAGE as usize - 1) != 0
        {
            return STATUS_INVALID_PARAMETER;
        }
        if ctx
            .backing_store_state
            .compare_exchange(
                BACKING_STORE_UNBOUND,
                BACKING_STORE_BINDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return STATUS_INVALID_DEVICE_REQUEST;
        }
        let mdl = unsafe {
            IoAllocateMdl(
                args.pBackingStore,
                bytes as u32,
                0,
                0,
                core::ptr::null_mut(),
            )
        };
        if mdl.is_null() || unsafe { helios_mm_probe_and_lock_pages_seh(mdl) } == 0 {
            if !mdl.is_null() {
                unsafe { IoFreeMdl(mdl) };
            }
            ctx.backing_store_state
                .store(BACKING_STORE_UNBOUND, Ordering::Release);
            return STATUS_NO_MEMORY;
        }
        let Some(kernel_va) = (unsafe { hoc1_mdl_system_va(mdl, bytes) }) else {
            unsafe {
                wdk_sys::ntddk::MmUnlockPages(mdl);
                IoFreeMdl(mdl);
            }
            ctx.backing_store_state
                .store(BACKING_STORE_UNBOUND, Ordering::Release);
            return STATUS_NO_MEMORY;
        };
        ctx.backing_store_mdl.store(mdl as usize, Ordering::Relaxed);
        ctx.backing_store_va.store(kernel_va, Ordering::Relaxed);
        ctx.backing_store_state
            .store(BACKING_STORE_BOUND, Ordering::Release);
        return STATUS_SUCCESS;
    }
    let Some(role) = Hvm1Role::from_u32(ctx.hvm1_role) else {
        return STATUS_INVALID_PARAMETER;
    };
    let current_transport = adapter
        .with_virtio(|gpu| gpu.scanout_transport_instance())
        .ok();
    if ctx.kind != ALLOC_KIND_HVM1
        || !role.placement().cpu_visible
        || ctx.resource_id() != 0
        || !allocation_object::is_current(ctx.generation)
        || ctx.ctx_id == 0
        || ctx.ctx_id != adapter.venus_ctx_id()
        || current_transport != Some(ctx.transport_instance)
    {
        return STATUS_NOT_SUPPORTED;
    }
    let bytes = ctx.size as u64;
    if !matches!(ctx.size_provenance, BackingSize::SharedBackingStore(n) if n == bytes)
        || bytes == 0
        || bytes > (u32::MAX as u64 - (PAGE as u64 - 1))
        || bytes & (PAGE as u64 - 1) != 0
        || (args.pBackingStore as usize) & (PAGE as usize - 1) != 0
    {
        return STATUS_INVALID_PARAMETER;
    }

    let page_count = (bytes >> HELIOS_HVM1_SEGMENT_PAGE_SHIFT) as usize;
    let mut entries = Vec::<VirtioGpuMemEntry>::new();
    if entries.try_reserve_exact(page_count).is_err() {
        return STATUS_NO_MEMORY;
    }
    if ctx
        .backing_store_state
        .compare_exchange(
            BACKING_STORE_UNBOUND,
            BACKING_STORE_BINDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return STATUS_INVALID_DEVICE_REQUEST;
    }

    let mdl = unsafe {
        IoAllocateMdl(
            args.pBackingStore,
            bytes as u32,
            0,
            0,
            core::ptr::null_mut(),
        )
    };
    if mdl.is_null() {
        ctx.backing_store_state
            .store(BACKING_STORE_UNBOUND, Ordering::Release);
        return STATUS_NO_MEMORY;
    }
    if unsafe { helios_mm_probe_and_lock_pages_seh(mdl) } == 0 {
        unsafe { IoFreeMdl(mdl) };
        ctx.backing_store_state
            .store(BACKING_STORE_UNBOUND, Ordering::Release);
        return STATUS_INVALID_PARAMETER;
    }

    // K11 alone needs a stable CPU alias, and only for the fixed 4-MiB role-1
    // pool. Mapping every future role-2/role-3 allocation here would consume an
    // unbounded number of system PTEs merely because shared backing exists.
    let kernel_va = if role == Hvm1Role::ReplyPool {
        let Some(kernel_va) = (unsafe { k2a_mdl_system_va(mdl, bytes) }) else {
            unsafe {
                wdk_sys::ntddk::MmUnlockPages(mdl);
                IoFreeMdl(mdl);
            }
            ctx.backing_store_state
                .store(BACKING_STORE_UNBOUND, Ordering::Release);
            return STATUS_NO_MEMORY;
        };
        kernel_va
    } else if role == Hvm1Role::VulkanHostVisible
        && BS_SAMPLE_MAPPED.load(Ordering::Relaxed) < BS_SAMPLE_MAX
    {
        // Diagnostic only, and non-fatal: failing to map costs a sample, not
        // the allocation. See BS_SAMPLE_MAPPED.
        match unsafe {
            k2a_mdl_system_va_cached(mdl, bytes, _MEMORY_CACHING_TYPE::MmWriteCombined)
        } {
            Some(va) => {
                BS_SAMPLE_MAPPED.fetch_add(1, Ordering::Relaxed);
                va
            }
            None => 0,
        }
    } else {
        0
    };

    let pfns = unsafe { helios_mm_get_mdl_pfn_array(mdl) };
    let mut valid = !pfns.is_null();
    for index in 0..page_count {
        let pfn = unsafe { *pfns.add(index) };
        if pfn > (u64::MAX >> HELIOS_HVM1_SEGMENT_PAGE_SHIFT) {
            valid = false;
            break;
        }
        let address = pfn << HELIOS_HVM1_SEGMENT_PAGE_SHIFT;
        if let Some(last) = entries.last_mut() {
            let end = last.addr.checked_add(last.length as u64);
            if end == Some(address) && last.length <= u32::MAX - PAGE as u32 {
                last.length += PAGE as u32;
                continue;
            }
        }
        entries.push(VirtioGpuMemEntry {
            addr: address,
            length: PAGE as u32,
            padding: 0,
        });
    }
    let exported = entries
        .iter()
        .try_fold(0u64, |sum, entry| sum.checked_add(entry.length as u64));
    if !valid || exported != Some(bytes) {
        unsafe {
            wdk_sys::ntddk::MmUnlockPages(mdl);
            IoFreeMdl(mdl);
        }
        ctx.backing_store_state
            .store(BACKING_STORE_UNBOUND, Ordering::Release);
        return STATUS_INVALID_PARAMETER;
    }

    let blob_flags = match role {
        Hvm1Role::ReplyPool | Hvm1Role::Feedback => VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE,
        Hvm1Role::VulkanHostVisible => {
            VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE | VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE
        }
        Hvm1Role::VulkanDeviceLocal => {
            unsafe {
                wdk_sys::ntddk::MmUnlockPages(mdl);
                IoFreeMdl(mdl);
            }
            ctx.backing_store_state
                .store(BACKING_STORE_UNBOUND, Ordering::Release);
            return STATUS_NOT_SUPPORTED;
        }
    };
    let passive = unsafe { PassiveLevel::assume() };
    let resource_id = match crate::virtio::ctrl::resource_create_guest_blob(
        passive,
        adapter,
        ctx.ctx_id,
        blob_flags,
        bytes,
        &entries,
        mdl as usize,
    ) {
        Ok(resource_id) => resource_id,
        Err(error) => {
            return error.into();
        }
    };

    // Retain the pre-K11 scalar publication edge for existing resource readers;
    // BOUND below additionally publishes resource + role-1 alias as one tuple.
    ctx.resource_id.store(resource_id, Ordering::Release);
    ctx.backing_store_va.store(kernel_va, Ordering::Relaxed);
    // Publish the complete alias as one state transition.  A K11 reader that
    // observes BOUND must also observe both the canonical resource and kernel
    // mapping above.
    ctx.backing_store_state
        .store(BACKING_STORE_BOUND, Ordering::Release);
    crate::diag::record_named_bytes(b"ShBkOk", resource_id);
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
        let seq = DESTROY_ALLOC_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
        step(b"DaStep", seq, 1);
        if let Some(ctx) = unsafe { take_alloc_ctx(handle) } {
            unsafe { destroy_allocation_ctx(passive, adapter, ctx, seq) };
        }
        step(b"DaStep", seq, 9);
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
/// # The create-open publishes HWA2/HVM1; ordinary opens are const
///
/// The no-restamp rule is the retirement's identity model (§10.3:1033-1034,
/// §18.1:4769, A.2 row 5694): two `write_open_identity` restamps used to live
/// here, one per entry and one call-level, and their existence is exactly why
/// two openers of one allocation could disagree about what they had. HOC1 and
/// every ordinary open remain const.
///
/// ⛔ It could NOT stand for the HVM1 create-output, and the premise underneath
/// it — that a create-time write reaches the caller — was FALSIFIED on the
/// target on 2026-08-11: dxgkrnl discards a KMD write into
/// `DXGK_ALLOCATIONINFO::pPrivateDriverData` for any user-supplied buffer, so
/// there was no channel left. [`stamp_open_hvm1`] and [`stamp_open_hwa2`] are
/// the two create-flagged publications through the WDK's actual in/out field.
/// Both copy the one canonical output already minted by CreateAllocation;
/// neither derives a per-open identity, and neither writes on an ordinary open.
///
/// The thing the restamps were FOR — giving a UMD opener of a KMD-created
/// standard allocation something to alias the venus resource with — is not
/// solved by writing a different record here. It is solved structurally: the
/// canonical allocation owns one final HWA2, the create-open publishes exactly
/// those bytes, and every later opener must match them byte-for-byte.
///
/// # The canonical open/allocation association
///
/// HWA2 still carries no host resource id.  D4 nevertheless needs the exact
/// allocation behind a DMA Present's `hDeviceSpecificAllocation`, and WDDM 2.0
/// provides that association directly: at PASSIVE OpenAllocation,
/// `DxgkCbAcquireHandleData(DXGK_HANDLE_ALLOCATION)` resolves the runtime
/// `D3DKMT_HANDLE` to the KMD allocation private data and returns the paired
/// release token. The Microsoft WDDM-2 compute sample releases that token inside
/// OpenAllocation after copying the KMD pointer; holding it until CloseAllocation
/// instead can pin dxgkrnl's own open/destroy transition. We follow that scoped
/// pattern and rely on the separate documented ordering that CloseAllocation is
/// called for every binding before DestroyAllocation. We never search by resource
/// id, geometry, current scanout, or list position. The D4 display path reads
/// only the canonical allocation projection.
unsafe fn canonical_open_allocation(
    adapter: &AdapterContext,
    runtime_handle: u32,
) -> Option<usize> {
    let dxgkrnl = adapter.dxgkrnl_opt()?;
    let acquire = dxgkrnl.DxgkCbAcquireHandleData?;
    let release = dxgkrnl.DxgkCbReleaseHandleData?;
    let mut args = unsafe { core::mem::zeroed::<DXGKARGCB_GETHANDLEDATA>() };
    args.hObject = runtime_handle;
    args.Type = _DXGK_HANDLE_TYPE::DXGK_HANDLE_ALLOCATION;
    args.Flags.__bindgen_anon_1.Value = 0;
    let mut release_handle: DXGKARG_RELEASE_HANDLE = core::ptr::null_mut();
    // SAFETY: OpenAllocation is PASSIVE_LEVEL; `runtime_handle` is the exact
    // live hAllocation from this DXGK_OPENALLOCATIONINFO entry and both argument
    // objects live for the synchronous callback.
    let allocation = unsafe { acquire(&args, &mut release_handle) } as HANDLE;
    if allocation.is_null() {
        return None;
    }
    // Validate the returned KMD private pointer while the scoped reference is
    // held. Keep only the boolean across the release: no Rust reference may
    // outlive the dxgkrnl reference that made this dereference legal.
    let valid = unsafe { resolve_alloc(allocation) }.is_some();
    let release_args = DXGKARGCB_RELEASEHANDLEDATA {
        ReleaseHandle: release_handle,
        Type: _DXGK_HANDLE_TYPE::DXGK_HANDLE_ALLOCATION,
    };
    // SAFETY: this is the exact token and type paired with the successful
    // acquire above. It is released before the pointer is published into the
    // per-open object, matching Microsoft's WDDM-2 sample ordering.
    unsafe { release(release_args) };
    valid.then_some(allocation as usize)
}

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
    let Some(device) = (unsafe { crate::device::DeviceHandleRef::from_raw(h_device) }) else {
        return STATUS_INVALID_PARAMETER;
    };
    let Some(adapter) = device.adapter() else {
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
    // D4 pre-resolves every runtime allocation before publishing the first
    // device-specific handle. A refusal therefore has no partially-open prefix
    // to unwind. The exact association is useful to the still-reachable legacy
    // DMA worker as well as the D2 owner path, replacing its deleted
    // resource-id reverse lookup without exposing D4 authority.
    let mut canonical_allocations = Vec::new();
    if canonical_allocations
        .try_reserve_exact(args.NumAllocations as usize)
        .is_err()
    {
        return STATUS_NO_MEMORY;
    }
    for i in 0..args.NumAllocations as usize {
        let info = unsafe { &*args.pOpenAllocation.add(i) };
        let Some(allocation) = (unsafe { canonical_open_allocation(adapter, info.hAllocation) })
        else {
            return STATUS_INVALID_HANDLE;
        };
        canonical_allocations.push(allocation);
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
    for (i, canonical_allocation) in canonical_allocations.into_iter().enumerate() {
        let info = unsafe { &mut *args.pOpenAllocation.add(i) };
        crate::diag::record(0x0C21_0000 | ((info.PrivateDriverDataSize as u32).min(0xFFFF)));
        crate::diag::record(0x0C35_0000 | ((info.hAllocation as usize as u32) & 0xFFFF));

        // Only the PER-ALLOCATION buffer participates. The resource-level
        // fallback remains gone: §10.3 associates one descriptor with one
        // allocation, so consulting that copy would read a different
        // allocation's identity. A create-flagged open publishes the exact
        // canonical HWA2 output because Windows discards the earlier create-DDI
        // write; an ordinary open is strictly read-only.
        let open_flags = unsafe { args.Flags.__bindgen_anon_1.Value };
        let desc = if open_flags & DXGK_OPENALLOCATION_FLAG_CREATE != 0 {
            unsafe {
                stamp_open_hwa2(
                    info.pPrivateDriverData,
                    info.PrivateDriverDataSize,
                    canonical_allocation,
                )
            }
        } else {
            unsafe {
                read_open_descriptor(
                    info.pPrivateDriverData,
                    info.PrivateDriverDataSize,
                    canonical_allocation,
                )
            }
        };

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
        let hvm1 = if open_flags & DXGK_OPENALLOCATION_FLAG_CREATE != 0 {
            unsafe {
                stamp_open_hvm1(
                    info.pPrivateDriverData,
                    info.PrivateDriverDataSize,
                    canonical_allocation,
                )
            }
        } else {
            unsafe {
                read_open_hvm1(
                    info.pPrivateDriverData,
                    info.PrivateDriverDataSize,
                    canonical_allocation,
                )
            }
        };
        // HOC1 follows K4's const-open rule. Unlike the one measured HVM1
        // exception above, its create-output must already be present in the
        // runtime-owned buffer; a create-input generation is not repaired here.
        let hoc1 = unsafe {
            read_open_hoc1(
                info.pPrivateDriverData,
                info.PrivateDriverDataSize,
                canonical_allocation,
            )
        };
        // What the guest was TOLD, for K6 to check its use records against.
        // HWA2's generation comes from the descriptor for the same reason: it is
        // the value the opener reads out of the identical bytes.
        let open_identity = match (hvm1, hoc1, desc) {
            (Some((role, byte_size, generation)), _, _) => Some(OpenIdentity {
                generation,
                kind: ALLOC_KIND_HVM1,
                hvm1_role: role.to_u32(),
                byte_size,
            }),
            (None, Some((byte_size, generation)), _) => Some(OpenIdentity {
                generation,
                kind: ALLOC_KIND_HOC1,
                hvm1_role: 0,
                byte_size,
            }),
            (None, None, Some(d)) => Some(OpenIdentity {
                generation: d.allocation_generation,
                kind: d.allocation_kind,
                hvm1_role: 0,
                byte_size: d.byte_size,
            }),
            (None, None, None) => None,
        };
        let mut reply_pool_session = None;
        if let Some((Hvm1Role::ReplyPool, byte_size, generation)) = hvm1 {
            reply_pool_session = crate::ddi::translation_session::bind_reply_pool(
                device.session_cell(),
                Hvm1Role::ReplyPool,
                byte_size,
                generation,
                canonical_allocation,
            );
        }
        let execution = if let Some((role, _, _)) = hvm1 {
            match role {
                // The role-1 open creates the provisional session and owns its
                // reference in `reply_pool_session`; executor attachment becomes
                // legal only after INIT transitions that exact object to Live.
                Hvm1Role::ReplyPool => reply_pool_session
                    .map(|session| OpenExecutionBinding::new(session, canonical_allocation, false)),
                Hvm1Role::VulkanHostVisible | Hvm1Role::Feedback | Hvm1Role::VulkanDeviceLocal => {
                    crate::ddi::translation_session::retain_execution_session(device.session_cell())
                        .map(|session| {
                            OpenExecutionBinding::new(session, canonical_allocation, true)
                        })
                }
            }
        } else if desc.is_some_and(|d| {
            helios_kmd_logic::outer_execution::hwa2_is_outer_execution_resource(&d)
        }) {
            Some(
                crate::ddi::translation_session::retain_execution_session(device.session_cell())
                    .map(|session| OpenExecutionBinding::new(session, canonical_allocation, true))
                    .unwrap_or_else(|| OpenExecutionBinding::new_unbound(canonical_allocation)),
            )
        } else {
            None
        };

        if open_identity.is_some() && !(unsafe { prepare_outer_gpuva_state(canonical_allocation) })
        {
            return STATUS_NO_MEMORY;
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

        let open = Box::new(OpenAllocationContext {
            magic: OPEN_ALLOCATION_CTX_MAGIC,
            allocation: canonical_allocation,
            identity: open_identity,
            reply_pool_session,
            execution,
            outer: OpenOuterBinding::new(),
        });
        if let Some(binding) = open.execution.as_ref() {
            // SAFETY: `open` is already heap-pinned and is not published until
            // `Box::into_raw` below.
            unsafe { binding.init_event() };
        }
        unsafe { open.outer.init_event() };
        record_alloc_event(
            0,
            desc.map_or(0, |d| d.width),
            desc.map_or(0, |d| d.height),
            0,
            true,
        );
        let raw_open = Box::into_raw(open);
        if !device.register_outer_open(raw_open as usize) {
            let mut open = unsafe { Box::from_raw(raw_open) };
            // Never published, so no guard can exist and there is nothing to
            // census.
            open.outer.close(|| 0);
            if let Some(execution) = open.execution.take() {
                execution.close(unsafe { PassiveLevel::assume() });
            }
            if let Some(session) = open.reply_pool_session.take() {
                unsafe {
                    crate::ddi::translation_session::close_reply_pool_binding(
                        session,
                        open.allocation,
                    )
                };
            }
            return STATUS_NO_MEMORY;
        }
        info.hDeviceSpecificAllocation = raw_open as HANDLE;
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
    canonical_allocation: usize,
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
    let Some(ctx) = (unsafe { resolve_alloc(canonical_allocation as HANDLE) }) else {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        return None;
    };
    if desc
        .validate_create_output(HELIOS_PACKAGE_GENERATION)
        .is_err()
        || ctx.final_hwa2 != Some(desc)
        || ctx.generation != desc.allocation_generation
        || ctx.kind != desc.allocation_kind
    {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        crate::diag::record(0x0C02_00E6);
        return None;
    }
    Some(desc)
}

/// Publish the canonical HWA2 create-output through a create-flagged
/// `DxgkDdiOpenAllocation`.
///
/// Windows was measured to discard the KMD's write into the user-supplied
/// `DXGK_ALLOCATIONINFO::pPrivateDriverData`, while this open buffer is the
/// WDK-annotated in/out channel. The generation is still minted exactly once by
/// `admit_hwa2` and stored on the canonical allocation; this function merely
/// copies that same final record. It accepts either the exact create-input or
/// the exact already-published output, so KMD-authored standard allocations and
/// an OS that preserves the earlier write converge on the same bytes.
///
/// # Safety
/// `private` is dxgkrnl's per-allocation private buffer and `private_size` its
/// authoritative length. The only write is bounded by the exact-size check and
/// occurs only on the create-flagged open selected by the caller.
unsafe fn stamp_open_hwa2(
    private: *mut c_void,
    private_size: UINT,
    canonical_allocation: usize,
) -> Option<HeliosWddmAllocationDescV2> {
    if private.is_null() || private_size as usize != HELIOS_HWA2_BYTES as usize {
        return None;
    }
    let parsed = {
        // SAFETY: non-null and exact length proven above. Keep the read slice
        // scoped before forming the mutable output slice below.
        let bytes = unsafe {
            core::slice::from_raw_parts(private as *const u8, HELIOS_HWA2_BYTES as usize)
        };
        HeliosWddmAllocationDescV2::from_private_data(bytes)
    };
    let Ok(desc) = parsed else {
        return None;
    };
    if desc.magic != HELIOS_HWA2_MAGIC {
        return None;
    }
    let Some(ctx) = (unsafe { resolve_alloc(canonical_allocation as HANDLE) }) else {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        return None;
    };
    let Some(canonical) = ctx.final_hwa2 else {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        return None;
    };
    if canonical
        .validate_create_output(HELIOS_PACKAGE_GENERATION)
        .is_err()
        || canonical.allocation_generation != ctx.generation
        || canonical.allocation_kind != ctx.kind
    {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        return None;
    }

    // A KMD-authored standard allocation can already carry the output because
    // dxgkrnl owns that buffer. Never rewrite an already-canonical record.
    if desc == canonical {
        return Some(canonical);
    }

    // Reconstruct the one legal create-input from the canonical output. HWA2's
    // field partition makes generation and the KMD-owned flag bits the only
    // differences for UMD-authored allocations. If KMD standard-allocation
    // extent adoption changed any other field, only the already-canonical arm
    // above is legal; a guessed reconstruction is refused.
    if desc
        .validate_create_input(HELIOS_PACKAGE_GENERATION)
        .is_err()
    {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        crate::diag::record(0x0C02_00E6);
        return None;
    }
    let expected_input = HeliosWddmAllocationDescV2 {
        allocation_generation: 0,
        flags: canonical.flags & !helios_protocol::HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
        ..canonical
    };
    if desc != expected_input {
        bump(&OPEN_HWA2_REJECT, b"OaHwa2Rej");
        crate::diag::record(0x0C02_00E6);
        return None;
    }

    // SAFETY: exact size and writable create-open buffer proven above.
    let out =
        unsafe { core::slice::from_raw_parts_mut(private as *mut u8, HELIOS_HWA2_BYTES as usize) };
    out.copy_from_slice(bytes_of(&canonical));
    bump(&OPEN_HWA2_STAMPED, b"OaHwa2Stamp");
    Some(canonical)
}

unsafe fn prepare_outer_gpuva_state(canonical_allocation: usize) -> bool {
    let Some(ctx) = (unsafe { resolve_alloc(canonical_allocation as HANDLE) }) else {
        return false;
    };
    if !ctx.outer_gpuva.load(Ordering::Acquire).is_null() {
        return true;
    }
    let candidate = Box::new(OuterGpuVaState {
        mappings: SpinLock::new(crate::sync::FixedVec::with_max(OUTER_GPUVA_MAX_RANGES)),
    });
    let raw = Box::into_raw(candidate);
    match ctx.outer_gpuva.compare_exchange(
        core::ptr::null_mut(),
        raw,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => true,
        Err(found) => {
            drop(unsafe { Box::from_raw(raw) });
            !found.is_null()
        }
    }
}

unsafe fn read_open_hoc1(
    private: *const c_void,
    private_size: UINT,
    canonical_allocation: usize,
) -> Option<(u64, u64)> {
    if private.is_null() || private_size as usize != HELIOS_HOC1_BYTES as usize {
        return None;
    }
    let bytes =
        unsafe { core::slice::from_raw_parts(private as *const u8, HELIOS_HOC1_BYTES as usize) };
    let record = HeliosOuterCommandAllocationV1::from_private_data(bytes).ok()?;
    if record.magic != HELIOS_HOC1_MAGIC
        || record
            .validate_create_output(HELIOS_PACKAGE_GENERATION)
            .is_err()
    {
        return None;
    }
    let ctx = unsafe { resolve_alloc(canonical_allocation as HANDLE) }?;
    (ctx.kind == ALLOC_KIND_HOC1
        && ctx.size as u64 == record.byte_size
        && ctx.generation == record.allocation_generation)
        .then_some((record.byte_size, record.allocation_generation))
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
    canonical_allocation: usize,
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
    let ctx = unsafe { resolve_alloc(canonical_allocation as HANDLE) }?;
    if ctx.kind != ALLOC_KIND_HVM1
        || ctx.hvm1_role != role.to_u32()
        || ctx.size as u64 != record.byte_size
        || ctx.generation != record.object_generation
    {
        return None;
    }
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

/// Acquire executor custody from the exact device-specific allocation handle
/// carried by this Render.  The expected session and generation come from the
/// direct HVC1 context and HNR2 use record respectively; neither is a lookup
/// key.  The returned guard keeps the open allocation, session attachment, and
/// canonical resource association live through terminal host completion.
///
/// # Safety
/// `h` is an `hDeviceSpecificAllocation` from the live Render allocation list.
pub(crate) unsafe fn open_allocation_execution_use(
    h: HANDLE,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    expected_generation: u64,
    passive: PassiveLevel,
) -> Option<OpenExecutionUse> {
    // `Nr2OaeWhy`: which of this function's five refusals ran last. Its caller
    // records only "could not resolve", which five different causes share.
    let why = |code: u32| crate::diag::record_named_bytes(b"Nr2OaeWhy", code);
    let Some(open) = (unsafe { open_allocation_context(h) }) else {
        why(1);
        return None;
    };
    let Some(identity) = open.identity else {
        why(2);
        return None;
    };
    if identity.generation != expected_generation {
        why(3);
        return None;
    }
    let Some(binding) = open.execution.as_ref() else {
        why(4);
        return None;
    };
    let Some(guard) = binding.acquire(passive, session, expected_generation) else {
        why(5);
        return None;
    };
    // The open-time record is the only identity Render is allowed to trust.
    // Cross-check every field the direct execution binding cached from the
    // canonical allocation before returning custody; do not let a matching
    // generation alone turn a different role or extent into an executor use.
    let role_matches = if identity.kind == ALLOC_KIND_HVM1 {
        identity.hvm1_role != 0 && guard.hvm1_role == identity.hvm1_role
    } else {
        identity.hvm1_role == 0 && guard.hvm1_role == 0
    };
    if guard.allocation_generation != identity.generation
        || guard.byte_size != identity.byte_size
        || !role_matches
    {
        why(6);
        return None;
    }
    Some(guard)
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
/// An already stamped buffer is republished as-is rather than re-minted, so
/// every ordinary or shared-resource open observes the canonical allocation's
/// one generation.
///
/// # Safety
/// `private` is dxgkrnl's per-allocation private buffer and `private_size` its
/// authoritative length. The write is bounded by the exact-length check below.
unsafe fn stamp_open_hvm1(
    private: *mut c_void,
    private_size: UINT,
    canonical_allocation: usize,
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
    let ctx = unsafe { resolve_alloc(canonical_allocation as HANDLE) }?;
    if ctx.kind != ALLOC_KIND_HVM1 {
        return None;
    }
    // Already complete: a second open, or a buffer dxgkrnl kept from a first one.
    if let Ok(role) = record.validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateOutput) {
        return (ctx.hvm1_role == role.to_u32()
            && ctx.size as u64 == record.byte_size
            && ctx.generation == record.object_generation)
            .then_some((role, record.byte_size, record.object_generation));
    }
    let Ok(role) = record.validate(HELIOS_PACKAGE_GENERATION, Hvm1Stage::CreateInput) else {
        bump(&OPEN_HVM1_REJECT, b"OaHvm1Rej");
        return None;
    };
    if ctx.hvm1_role != role.to_u32() || ctx.size as u64 != record.byte_size {
        bump(&OPEN_HVM1_REJECT, b"OaHvm1Rej");
        return None;
    }
    let generation = ctx.generation;
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
    h_device: IN_CONST_HANDLE,
    close_allocation: IN_CONST_PDXGKARG_CLOSEALLOCATION,
) -> NTSTATUS {
    if h_device.is_null() || close_allocation.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let Some(device) = (unsafe { crate::device::DeviceHandleRef::from_raw(h_device) }) else {
        return STATUS_INVALID_PARAMETER;
    };
    let args = unsafe { &*close_allocation };
    if args.NumAllocations != 0 && args.pOpenHandleList.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    for i in 0..args.NumAllocations as usize {
        let handle = unsafe { *args.pOpenHandleList.add(i) };
        if !handle.is_null() {
            crate::diag::record(0x0C37_0000 | ((handle as usize as u32) & 0xFFFF));
            // Remove the association before object storage can be reused.  A
            // resolver that already acquired it owns `outer` rundown and is
            // joined immediately below.
            let seq = CLOSE_ALLOC_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
            step(b"CaStep", seq, 1);
            device.unregister_outer_open(handle as usize);
            step(b"CaStep", seq, 2);
            if let Some(mut open) = unsafe { take_open_ctx(handle) } {
                // BEFORE the join, never after: `outer.close()` waits untimed,
                // and a batch dxgkrnl discarded without ever submitting holds a
                // guard nothing else will ever return. Retracting those is what
                // makes the join finite — see `retract_parked_referencing`.
                step(b"CaStep", seq, 3);
                if let Some(adapter) = device.adapter() {
                    crate::ddi::native_render::retract_parked_referencing(adapter, handle as usize);
                }
                step(b"CaStep", seq, 4);
                open.outer.close(|| {
                    device.adapter().map_or(0, |adapter| {
                        crate::ddi::native_render::census_open_holders(adapter, handle as usize)
                    })
                });
                step(b"CaStep", seq, 5);
                if let Some(execution) = open.execution.take() {
                    // CloseAllocation is PASSIVE_LEVEL.  Revoke new use,
                    // event-join exact host custody, then detach/release before
                    // any session role-1 teardown below.
                    execution.close(unsafe { PassiveLevel::assume() });
                }
                step(b"CaStep", seq, 6);
                if let Some(session) = open.reply_pool_session.take() {
                    // SAFETY: this is the strong reference the exact open took
                    // in `bind_reply_pool`; its canonical allocation remains
                    // live until after CloseAllocation returns.
                    unsafe {
                        crate::ddi::translation_session::close_reply_pool_binding(
                            session,
                            open.allocation,
                        )
                    };
                }
                step(b"CaStep", seq, 7);
                // Box drops only after K11 has revoked/drained/destroyed.
                drop(open);
                step(b"CaStep", seq, 8);
            }
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
