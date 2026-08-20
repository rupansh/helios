//! L4 — resources, heaps, residency and introspection.
//!
//! Owns 16 of `DEVICE_FUNCS_CORE_0109` (groups (g) 11, (h) 5), verified against
//! `DDI_REFERENCE.md` §3.2 and against `d3d12umddi.h`'s own member order in
//! `umd12/bindgen/cached/d3d12umddi.rs:54702-54723`.
//!
//! # ⭐ The shape of this lane: one fused create DDI, three public APIs
//!
//! `pfnCreateHeapAndResource` is ONE slot doing THREE jobs, and the arm is
//! selected by which of its two `_In_opt_` argument pointers are non-NULL
//! (`DDI_REFERENCE.md` §7.3(1), from `ResourceHeaps.md:1180`):
//!
//! | `pCreateHeap` | `pCreateResource` | DDI meaning | this file |
//! |---|---|---|---|
//! | non-NULL | non-NULL | fused heap + base resource | [`create_fused_heap_and_resource`] |
//! | non-NULL | NULL | `CreateHeap` | [`create_heap_only`] |
//! | NULL | non-NULL | child resource / reserved resource | [`create_placed_or_reserved`] |
//! | NULL | NULL | illegal | refused, `HeapResourceCreateBadArg` |
//!
//! DDI 0110 does not say whether the fused row came from an API committed
//! resource or an explicit heap with a base resource. The forwarding
//! representation is deliberately identical for both: an engine heap plus an
//! offset-zero engine resource. DDI 0115 added an `IMPLICIT` bit specifically
//! because distinguishing those API origins before then required heuristics;
//! this driver does not use one.
//!
//! ⛔ CLAUDE.md's *"validate every runtime-supplied size & offset per-arm, not
//! max-union"* applies literally here: the RenderGdi ~48 % drop bug was exactly
//! this mistake in D3D11. Each arm below reads only the fields its own arm
//! guarantees.
//!
//! ⭐ **Placed resources do not name a heap.** `ResourceHeaps.md:1212`, mined in
//! `SPECS.md` §9.7: *"The D3D12 DDI carries no heap-handle-plus-heap-offset
//! placement parameter. The only placement parent expressible in
//! `D3D12DDIARG_CREATERESOURCE_0111` is `ReuseBufferGPUVA`"* — a
//! `D3D12DDIARG_HRESOURCE_PLACEMENT { D3D12DDI_HRESOURCE hResource; UINT64
//! Offset }`. And *"a RESERVED resource is the resource-args-only arm with
//! `ReuseBufferGPUVA.BaseAddress.UMD.hResource == NULL`"* (`:1204`). So the
//! parent of a placed resource is another **resource handle**, and the engine
//! heap has to be reachable *through* it. That is why [`ResourceState`] carries
//! a [`HeapSpan`]: every object this file creates inside an engine heap records
//! which heap and at what offset, so a placement chain resolves without the DDI
//! ever naming the heap.
//!
//! # ⭐ The engine is reached through `ID3D12Device10`, not `ID3D12Device`
//!
//! `D3D12DDIARG_CREATERESOURCE_0111` carries `InitialBarrierLayout`,
//! `SamplerFeedbackMipRegion` and `NumCastableFormats`/`pCastableFormats`. Those
//! three fields exist on exactly one API entry point family —
//! `ID3D12Device10::CreateCommittedResource3` / `CreatePlacedResource2`, which
//! take `D3D12_RESOURCE_DESC1` (= `D3D12_RESOURCE_DESC` + the mip region) plus a
//! `D3D12_BARRIER_LAYOUT` and a castable-format list. The older
//! `CreateCommittedResource` would force this driver to invent a legacy
//! `D3D12_RESOURCE_STATES` and to silently drop two fields the runtime handed
//! it. vkd3d implements the whole family (`libs/vkd3d/device.c:9408`, `:9448`)
//! and its `QueryInterface` accepts `IID_ID3D12Device10` (`device.c:4647`).
//!
//! ⚠ Everything here is reachable from Rust through the borrowed `ID3D12Device`
//! the bridge already exposes. **No cxx bridge module was needed or added**, so
//! this whole lane type-checks on the Linux host (`PARALLEL.md` §7).
//!
//! # ⛔ Three ABI traps in the DDI-to-API translation, all of them silent
//!
//! 1. **`D3D12DDI_MEMORY_POOL` is offset by one from `D3D12_MEMORY_POOL`.** DDI
//!    `L0 = 0, L1 = 1` (`d3d12umddi.rs:48255-48256`); API `UNKNOWN = 0, L0 = 1,
//!    L1 = 2`. A blind cast turns "system memory" into "unknown".
//! 2. **`D3D12DDI_CPU_PAGE_PROPERTY` is offset by one too.** DDI
//!    `NOT_AVAILABLE = 0, WRITE_COMBINE = 1, WRITE_BACK = 2`
//!    (`d3d12umddi.rs:48248-48253`); API `UNKNOWN = 0, NOT_AVAILABLE = 1,
//!    WRITE_COMBINE = 2, WRITE_BACK = 3`. Same failure, worse: a
//!    not-CPU-visible heap becomes a driver-chooses heap.
//! 3. **The heap and resource flag enums are not the same bits, and two of them
//!    are INVERTED.** The DDI hands positive ALLOW-style heap bits
//!    (`NON_RT_DS_TEXTURES = 0x2`, `BUFFERS = 0x4`, `RT_DS_TEXTURES = 0x20`)
//!    where the API takes DENY bits (`ResourceHeaps.md:874`), and
//!    `D3D12DDI_RESOURCE_FLAG_0003_SHADER_RESOURCE = 0x10` is the positive form
//!    of the API's `DENY_SHADER_RESOURCE = 0x8`.
//!
//! ⇒ every enum crossing this boundary goes through an explicit `match`, never a
//! cast. That is the same finding the 80th memory records for formats: D3D11
//! harmonised its DDI enums with the API's and D3D12 did **not**.
//!
//! # ⛔ `DECISIONS.md` D13, and the ONE record this lane writes
//!
//! D13 binds this lane hardest: private data that CROSSES a module boundary is
//! declared once, in `helios_protocol`, and reused verbatim. Since the HPS2
//! retirement (`docs/retirement/K4-CONTRACT.md`) that is exactly **one** record on
//! this path — [`helios_protocol::HeliosWddmAllocationDescV2`] (`'HWA2'`, 168 bytes),
//! the versioned immutable create-time allocation descriptor.
//!
//! ⚠ **This block has been wrong twice and both reversals are recorded rather than
//! quietly edited.**
//!
//! 1. It first said this lane *"declares no such record and takes no
//!    `helios_protocol` dependency, because it writes none: it mints no WDDM
//!    allocation"*. UP-5 falsified that: [`create_committed_allocation`] calls
//!    `pfnAllocateCb` for every committed resource,
//!    [`deallocate_committed`] releases the handle in
//!    [`destroy_heap_and_resource`], and [`check_resource_allocation_handle`] answers
//!    with the real `D3DKMT_HANDLE` instead of 0.
//! 2. It then said the record written there was the pair `HeliosWddmAllocPrivate`
//!    (`'HWDM'`) + `HeliosWddmAllocMeta`, with
//!    `HeliosWddmAllocPrivate.adopt_resource_id = <venus resid>` so the KMD would
//!    **adopt** the engine's memory, and `HeliosWddmOpenIdentity` (`'HIDN'`) restamped
//!    back at open time so DWM's D3D11 opener worked unchanged. **That whole model is
//!    retired.** §10.3 forbids a UMD naming a host resource id at all; the create
//!    sends an HWA2 descriptor the KMD validates, echoes and completes, and an opener
//!    treats the result as `const` — `DxgkDdiOpenAllocation` writes no byte of it.
//!
//! # ⛔ The §5 gap this lane now carries, named
//!
//! The retired record's host resid was the link between the WDDM allocation and the
//! `VkDeviceMemory` vkd3d renders into. HWA2 has no successor field and one may not be
//! invented (`K4-CONTRACT.md` §5). ⇒ this driver mints a valid kernel allocation for
//! every committed resource and **cannot yet present or export its contents**:
//! `Hwa2VenusResIdDropped` counts every create that had a host resid available and did
//! not send it, and `PresentIdentityNoResourceId` refuses the frame's identity record.
//! Both name **mesa lane unit A3**, which replaces the mechanism rather than the field:
//! the ICD stops naming host resources and the KMD patches the resid in from
//! `HeliosNativeRenderPatch`. An ICD in this state cannot import; that is the
//! retirement's intended intermediate state, recorded rather than worked around.
//!
//! ⇒ `PARALLEL.md` §5's *"`umd12` does not yet depend on `helios_protocol`; the
//! first lane that needs a crossing record adds it, and says so"* is discharged
//! **here**: `umd12/Cargo.toml` takes it, and this is the saying-so.
//!
//! ⚠ Still not true, and stated so nothing reads more into the above than it
//! carries: `pfnOpenHeapAndResource` and its sizing call are **still refused** — see
//! [`open_heap_and_resource`] for exactly what is missing and why — and that is the
//! *other* direction of D3c, not this one. The size and layout asserts for the shared
//! record are not restated either: `protocol/src/wddm.rs`'s own `const _` block pins
//! all 24 HWA2 offsets, and a second copy of an assert is a second thing that can
//! drift. ⛔ There is no longer a local pair type to assert the **adjacency** of —
//! `AdoptedAllocPrivate` is deleted with the two records it joined, and HWA2 is one
//! struct whose layout `protocol` owns end to end.
//!
//! The per-object `pDrvPrivate` payloads below ([`HeapState`], [`ResourceState`])
//! are runtime-allocated, per-object, per-process and read by nothing outside
//! this DLL, which is precisely the class D13's refined form keeps local and
//! typed against `ddi12`.
//!
//! # What this lane deliberately does not do
//!
//! * **Cross-DDI / cross-process resource opening** (`pfnOpenHeapAndResource`,
//!   `pfnCalcPrivateOpenedHeapAndResourceSizes`) — refused with named counters.
//! * **Reserved (tiled) resources** — refused, because `caps12.rs:278` reports
//!   `TiledResourcesTier = NOT_SUPPORTED` and creating one would contradict the
//!   caps this device was accepted on.
//! * **Kernel allocation identity for non-committed arms.** Every committed
//!   resource receives one at create time because the negotiated DDI carries no
//!   later `SHARED` declaration and the runtime does not call
//!   `pfnCheckResourceAllocationHandle` before sharing. Placed resources remain
//!   represented by their explicit heap; reserved resources are refused above.
//!
//! Each is a named counter, never a silent stub (CLAUDE.md rule 2).

use core::ffi::c_void;

use helios_umd_common::hr::{Hresult, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, S_OK};
use helios_umd_common::refusals::RefusalCounter;
use helios_umd_common::slot::{Boxed, Slot};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12Device10, ID3D12Heap, ID3D12Resource, D3D12_BARRIER_LAYOUT, D3D12_BARRIER_LAYOUT_COMMON,
    D3D12_BARRIER_LAYOUT_COPY_DEST, D3D12_BARRIER_LAYOUT_COPY_SOURCE,
    D3D12_BARRIER_LAYOUT_GENERIC_READ, D3D12_BARRIER_LAYOUT_SHADER_RESOURCE,
    D3D12_BARRIER_LAYOUT_UNDEFINED, D3D12_BARRIER_LAYOUT_VIDEO_QUEUE_COMMON, D3D12_CLEAR_VALUE,
    D3D12_CLEAR_VALUE_0, D3D12_CPU_PAGE_PROPERTY, D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE,
    D3D12_CPU_PAGE_PROPERTY_WRITE_BACK, D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE,
    D3D12_DEPTH_STENCIL_VALUE, D3D12_HEAP_DESC, D3D12_HEAP_FLAGS, D3D12_HEAP_FLAG_DENY_BUFFERS,
    D3D12_HEAP_FLAG_DENY_NON_RT_DS_TEXTURES, D3D12_HEAP_FLAG_DENY_RT_DS_TEXTURES,
    D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_CUSTOM, D3D12_MEMORY_POOL,
    D3D12_MEMORY_POOL_L0, D3D12_MEMORY_POOL_L1, D3D12_MIP_REGION,
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DESC1,
    D3D12_RESOURCE_DIMENSION, D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_DIMENSION_TEXTURE1D,
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_DIMENSION_TEXTURE3D, D3D12_RESOURCE_FLAGS,
    D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER, D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
    D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
    D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS, D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE,
    D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_FLAG_RAYTRACING_ACCELERATION_STRUCTURE,
    D3D12_RESOURCE_FLAG_VIDEO_DECODE_REFERENCE_ONLY,
    D3D12_RESOURCE_FLAG_VIDEO_ENCODE_REFERENCE_ONLY, D3D12_SUBRESOURCE_FOOTPRINT,
    D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT, D3D12_TEXTURE_LAYOUT,
    D3D12_TEXTURE_LAYOUT_64KB_STANDARD_SWIZZLE, D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE,
    D3D12_TEXTURE_LAYOUT_ROW_MAJOR, D3D12_TEXTURE_LAYOUT_UNKNOWN,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};

use super::identity12;
use super::tables12::{stage, DeviceCoreTable, Filling};
use crate::device12::{self, HeliosD3D12Device};
use crate::{ddi12, log_error, note_refusal};

/// ⛔ **The one `D3D12_HEAP_FLAGS` bit in this file that the D3D12 API does not
/// define**, and the channel that makes a committed texture adoptable at all.
///
/// It asks the vkd3d fork for two properties on the resource's `VkDeviceMemory`:
/// **exportable**, which is what makes the Mesa venus ICD assign it a virtio
/// resource id (`vn_device_memory_alloc` takes its export arm only when
/// `export_handle_types` is non-zero, and only that arm sets `base_bo`, so
/// `helios_venus_memory_res_id` answers 0 for anything else), and **dedicated at
/// offset 0**, without which N swapchain buffers could share one venus resource id
/// and buffer rotation would collapse onto a single surface.
///
/// ⛔ **The authority for this value is the fork**, `vkd3d-proton-helios/libs/vkd3d/
/// vkd3d_private.h`'s `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT`, where the whole
/// argument lives. This is a mirror, kept in sync by hand, because the
/// `D3D12_HEAP_FLAGS` word is the only channel between the two and neither side can
/// include the other's header — the same arrangement as `HELIOS_VKD3D_FENCE_*` in
/// `umd12/bridge/vkd3d_bridge.h`. ⚠ A drift is not silent: the fork would take no
/// export arm, the memory would have no venus resource, and
/// `IdentityVenusUnresolved` would count every primary create while
/// `HeapPrimaryVenusExport` counted the same number of translations.
///
/// `1 << 30` is above every value `D3D12_HEAP_FLAGS` defines (the highest is
/// `TOOLS_USE_MANUAL_WRITE_TRACKING`, `0x2000`), and vkd3d's `validate_heap_desc`
/// rejects no unknown bits.
const HELIOS_HEAP_FLAG_VENUS_EXPORT: D3D12_HEAP_FLAGS = D3D12_HEAP_FLAGS(1 << 30);

/// What [`heap_flags`] should do with `D3D12DDI_HEAP_FLAG_PRIMARY`.
///
/// ⭐ A parameter and not a constant, because the answer is genuinely per-arm and
/// the two arms are in different files' worth of contract. On the **committed** arm
/// the primary declaration can be honoured — there is an `ID3D12Resource` to give a
/// kernel allocation to — so the flag is translated into
/// [`HELIOS_HEAP_FLAG_VENUS_EXPORT`]. On the **heap-only** arm there is no resource,
/// `ResourceHeaps.md:897` says that combination should not exist, and the fork would
/// silently ignore the export bit on a bare `CreateHeap` (nothing in
/// `vkd3d_allocate_heap_memory` reads it) — so passing it there would be a request
/// this driver knows is dropped, which is the shape CLAUDE.md rule 2 forbids.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PrimaryTranslation {
    /// Translate `PRIMARY` into the fork's private venus-export bit.
    VenusExport,
    /// Drop it and count `HeapPrimaryFlagDropped`.
    Dropped,
}

/// Short aliases for the bindgen enumerator names, which arrive doubled because
/// bindgen prefixes each constant with its enum's name and the SDK header
/// already did the same. Same device as `caps12`'s `mod v`: every line is a
/// compile-checked reference to the header rather than a transcribed number
/// (`ARCHITECTURE.md` §12 rule 1).
mod v {
    use crate::ddi12::*;

    // -- D3D12DDI_RESOURCE_TYPE (d3d12umddi.rs:48391-48394) -----------------
    pub(super) const RT_BUFFER: D3D12DDI_RESOURCE_TYPE = D3D12DDI_RESOURCE_TYPE_D3D12DDI_RT_BUFFER;
    pub(super) const RT_TEXTURE1D: D3D12DDI_RESOURCE_TYPE =
        D3D12DDI_RESOURCE_TYPE_D3D12DDI_RT_TEXTURE1D;
    pub(super) const RT_TEXTURE2D: D3D12DDI_RESOURCE_TYPE =
        D3D12DDI_RESOURCE_TYPE_D3D12DDI_RT_TEXTURE2D;
    pub(super) const RT_TEXTURE3D: D3D12DDI_RESOURCE_TYPE =
        D3D12DDI_RESOURCE_TYPE_D3D12DDI_RT_TEXTURE3D;

    // -- D3D12DDI_TEXTURE_LAYOUT (d3d12umddi.rs:48016-48023) ----------------
    pub(super) const TL_UNDEFINED: D3D12DDI_TEXTURE_LAYOUT =
        D3D12DDI_TEXTURE_LAYOUT_D3D12DDI_TL_UNDEFINED;
    pub(super) const TL_ROW_MAJOR: D3D12DDI_TEXTURE_LAYOUT =
        D3D12DDI_TEXTURE_LAYOUT_D3D12DDI_TL_ROW_MAJOR;
    pub(super) const TL_64KB_TILE_UNDEFINED_SWIZZLE: D3D12DDI_TEXTURE_LAYOUT =
        D3D12DDI_TEXTURE_LAYOUT_D3D12DDI_TL_64KB_TILE_UNDEFINED_SWIZZLE;
    pub(super) const TL_64KB_TILE_STANDARD_SWIZZLE: D3D12DDI_TEXTURE_LAYOUT =
        D3D12DDI_TEXTURE_LAYOUT_D3D12DDI_TL_64KB_TILE_STANDARD_SWIZZLE;

    // -- D3D12DDI_MEMORY_POOL (d3d12umddi.rs:48255-48256) -------------------
    pub(super) const POOL_L0: D3D12DDI_MEMORY_POOL = D3D12DDI_MEMORY_POOL_D3D12DDI_MEMORY_POOL_L0;
    pub(super) const POOL_L1: D3D12DDI_MEMORY_POOL = D3D12DDI_MEMORY_POOL_D3D12DDI_MEMORY_POOL_L1;

    // -- D3D12DDI_CPU_PAGE_PROPERTY (d3d12umddi.rs:48248-48253) -------------
    pub(super) const CPU_NOT_AVAILABLE: D3D12DDI_CPU_PAGE_PROPERTY =
        D3D12DDI_CPU_PAGE_PROPERTY_D3D12DDI_CPU_PAGE_PROPERTY_NOT_AVAILABLE;
    pub(super) const CPU_WRITE_COMBINE: D3D12DDI_CPU_PAGE_PROPERTY =
        D3D12DDI_CPU_PAGE_PROPERTY_D3D12DDI_CPU_PAGE_PROPERTY_WRITE_COMBINE;
    pub(super) const CPU_WRITE_BACK: D3D12DDI_CPU_PAGE_PROPERTY =
        D3D12DDI_CPU_PAGE_PROPERTY_D3D12DDI_CPU_PAGE_PROPERTY_WRITE_BACK;

    // -- D3D12DDI_HEAP_FLAGS (d3d12umddi.rs:48258-48264) --------------------
    // ⛔ POSITIVE "allow" bits. The API takes DENY bits (`ResourceHeaps.md:874`).
    pub(super) const HEAP_ALLOW_NON_RT_DS_TEXTURES: D3D12DDI_HEAP_FLAGS =
        D3D12DDI_HEAP_FLAGS_D3D12DDI_HEAP_FLAG_NON_RT_DS_TEXTURES;
    pub(super) const HEAP_ALLOW_BUFFERS: D3D12DDI_HEAP_FLAGS =
        D3D12DDI_HEAP_FLAGS_D3D12DDI_HEAP_FLAG_BUFFERS;
    pub(super) const HEAP_ALLOW_RT_DS_TEXTURES: D3D12DDI_HEAP_FLAGS =
        D3D12DDI_HEAP_FLAGS_D3D12DDI_HEAP_FLAG_RT_DS_TEXTURES;
    /// ⛔ The one dropped heap bit that changes what the object *is*, rather
    /// than how it may be used. `SPECS.md` §9.7 from `ResourceHeaps.md:897`:
    /// *"The PRIMARY heap flag is the D3D12 replacement for
    /// `DXGI_DDI_PRIMARY_DESC`: no primary description is ever passed to the
    /// UMD, the flag alone declares the primary"*. It gets its own counter, not
    /// the shared unrepresentable bucket — see [`super::heap_flags`].
    pub(super) const HEAP_PRIMARY: D3D12DDI_HEAP_FLAGS =
        D3D12DDI_HEAP_FLAGS_D3D12DDI_HEAP_FLAG_PRIMARY;

    // -- D3D12DDI_RESOURCE_OPTIMIZATION_FLAGS (d3d12umddi.rs:48846-48854) ----
    /// ⚠ `KMD_IMPACT.md` §14a.3's *"second signal"* for a primary — and it
    /// arrives on `pfnCheckResourceAllocationInfo`, NOT on the create. This
    /// enum appears in exactly one function-pointer family
    /// (`d3d12umddi.rs:51734`, `:59866`, `:75022`, `:76696`, `:79414`,
    /// `:87548`), and `D3D12DDIARG_CREATERESOURCE_0111` has no field of the
    /// type. See [`super::create_committed_allocation`].
    pub(super) const RESOURCE_OPT_PRIMARY: D3D12DDI_RESOURCE_OPTIMIZATION_FLAGS =
        D3D12DDI_RESOURCE_OPTIMIZATION_FLAGS_D3D12DDI_RESOURCE_OPTIMIZATION_FLAG_PRIMARY;

    // -- D3D12DDI_RESOURCE_FLAGS_0003 (d3d12umddi.rs:48362-48389) -----------
    pub(super) const RES_RENDER_TARGET: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0003_RENDER_TARGET;
    pub(super) const RES_DEPTH_STENCIL: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0003_DEPTH_STENCIL;
    pub(super) const RES_CROSS_ADAPTER: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0003_CROSS_ADAPTER;
    pub(super) const RES_SIMULTANEOUS_ACCESS: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0003_SIMULTANEOUS_ACCESS;
    /// ⛔ The INVERTED one: positive here, `DENY_SHADER_RESOURCE` in the API.
    pub(super) const RES_SHADER_RESOURCE: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0003_SHADER_RESOURCE;
    pub(super) const RES_VIDEO_DECODE_REFERENCE_ONLY: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0020_VIDEO_DECODE_REFERENCE_ONLY;
    pub(super) const RES_UNORDERED_ACCESS: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0022_UNORDERED_ACCESS;
    pub(super) const RES_VIDEO_ENCODE_REFERENCE_ONLY: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0080_VIDEO_ENCODE_REFERENCE_ONLY;
    pub(super) const RES_RAYTRACING_ACCELERATION_STRUCTURE: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0088_RAYTRACING_ACCELERATION_STRUCTURE;
    // ⚠ The five remaining enumerators, aliased for ONE purpose: `meta_bind_flags`
    // builds its known-bit mask out of these rather than out of a literal, so a
    // header revision that adds an enumerator makes its counter fire instead of
    // silently widening the mask. None of them is a bind.
    pub(super) const RES_CONTENT_PROTECTION: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0020_CONTENT_PROTECTION;
    pub(super) const RES_ONLY_NON_RT_DS_TEXTURE_PLACEMENT: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0041_ONLY_NON_RT_DS_TEXTURE_PLACEMENT;
    pub(super) const RES_ONLY_RT_DS_TEXTURE_PLACEMENT: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0041_ONLY_RT_DS_TEXTURE_PLACEMENT;
    pub(super) const RES_4MB_ALIGNED: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0041_4MB_ALIGNED;
    pub(super) const RES_SAMPLER_FEEDBACK: D3D12DDI_RESOURCE_FLAGS_0003 =
        D3D12DDI_RESOURCE_FLAGS_0003_D3D12DDI_RESOURCE_FLAG_0073_SAMPLER_FEEDBACK;

    // -- D3D12DDI_BARRIER_LAYOUT (d3d12umddi.rs:78589-78657) ----------------
    pub(super) const LAYOUT_UNDEFINED: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_UNDEFINED;
    pub(super) const LAYOUT_COMMON: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_COMMON;
    pub(super) const LAYOUT_VIDEO_QUEUE_COMMON: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_VIDEO_QUEUE_COMMON;
    /// The four LEGACY_* enumerators plus 31 exist ONLY in the DDI: they are how
    /// the runtime hands a driver a legacy `D3D12_RESOURCE_STATES`-derived
    /// initial layout. The API `D3D12_BARRIER_LAYOUT` stops at
    /// `VIDEO_QUEUE_COMMON = 30`.
    pub(super) const LAYOUT_LEGACY_GENERIC_READ: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_LEGACY_DIRECT_QUEUE_GENERIC_READ_COMPUTE_QUEUE_ACCESSIBLE;
    pub(super) const LAYOUT_LEGACY_COPY_SOURCE: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_LEGACY_COPY_SOURCE;
    pub(super) const LAYOUT_LEGACY_COPY_DEST: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_LEGACY_COPY_DEST;
    pub(super) const LAYOUT_LEGACY_SHADER_RESOURCE: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_LEGACY_SHADER_RESOURCE;
    pub(super) const LAYOUT_LEGACY_PIXEL_SHADER_RESOURCE: D3D12DDI_BARRIER_LAYOUT =
        D3D12DDI_BARRIER_LAYOUT_D3D12DDI_BARRIER_LAYOUT_LEGACY_PIXEL_SHADER_RESOURCE;
}

/// How many times any one bounded evidence line may repeat, per site.
///
/// Matches `caps12`'s and `misc.rs`'s idiom: the counter is unbounded, the log
/// line is not. T2 measured what an unbounded per-op logger costs.
const LOG_BUDGET: usize = 32;

/// The value [`check_subresource_info`] seeds its 32-bit `GetCopyableFootprints`
/// outputs with, and reads back as *"the engine did not answer"*.
///
/// ⛔ Not an invented convention: vkd3d `memset`s every out-parameter array to
/// `0xff` and sets `total = ~0ull` **before** validating the subresource range,
/// then `goto end`s without touching them if the range is bad
/// (`libs/vkd3d/device.c:9234-9268`). So `UINT_MAX` is the engine's own way of
/// saying it declined, and seeding the same value makes "the engine wrote the
/// sentinel" and "the engine wrote nothing" the same observation. No real
/// footprint has a `RowPitch` or row count of `UINT_MAX`: the pitch is aligned to
/// 256 B and a resource that large is not creatable.
const FOOTPRINT_UNANSWERED_U32: u32 = u32::MAX;

/// The 64-bit form of [`FOOTPRINT_UNANSWERED_U32`], for `pTotalBytes`.
const FOOTPRINT_UNANSWERED_U64: u64 = u64::MAX;

/// The largest castable-format list this driver will forward.
///
/// `D3D12DDIARG_CREATERESOURCE_0111::NumCastableFormats` is a `UINT32` read
/// straight from the runtime, and it becomes a slice length. DXGI defines fewer
/// than 200 formats, so anything above this is not a list.
const CASTABLE_FORMAT_LIMIT: usize = 1_024;

/// The alignment ceiling a driver may report for a non-multisampled resource.
///
/// `ResourceHeaps.md:1350` (mined in `SPECS.md` §9.7): *"The driver-reported
/// resource alignment is capped: it must be a power of two and must never exceed
/// 64KiB unless `SampleDesc.Count > 1` — the >64KiB (4MB-class) tier exists only
/// for MSAA."* vkd3d answers `GetResourceAllocationInfo` from its own Vulkan
/// memory requirements and is under no such obligation, so the clamp is this
/// driver's.
const MAX_NON_MSAA_ALIGNMENT: u64 = 64 * 1024;

/// `GetResourceAllocationInfo`'s documented failure answer.
///
/// The API has no HRESULT — it returns the struct by value — and reports failure
/// by setting `SizeInBytes` to `UINT64_MAX`. vkd3d does exactly that
/// (`libs/vkd3d/device.c`, `d3d12_device_GetResourceAllocationInfo`).
const ALLOCATION_INFO_FAILURE: u64 = u64::MAX;

/// Alignment for an absent driver-private metadata block.
///
/// The size is zero, but the alignment must still be a valid power of two.
/// D3D12Core runs both additional-data fields through its align-up arithmetic
/// unconditionally; zero makes `size + alignment - 1` underflow and turns an
/// otherwise valid allocation answer into the API failure sentinel.  One is
/// the identity alignment and adds no padding.
const EMPTY_ADDITIONAL_DATA_ALIGNMENT: u32 = 1;

// ---------------------------------------------------------------------------
// Per-object payloads — LOCAL and typed, per `DECISIONS.md` D13
// ---------------------------------------------------------------------------

/// Where an object lives inside an engine heap.
///
/// ⭐ This is what makes `CreatePlacedResource` reachable from a DDI that never
/// names a heap. `ReuseBufferGPUVA` names a **resource** and an offset within
/// it; every object this driver puts in a heap records the heap and its own base
/// offset, so resolving the parent resource yields `(heap, parent_base)` and the
/// child lands at `parent_base + requested_offset`.
struct HeapSpan {
    /// The engine heap. An owning reference: a placed resource must not outlive
    /// its heap, and holding a reference per child is how that is guaranteed
    /// without this driver tracking a child list.
    heap: ID3D12Heap,
    /// This object's byte offset from the start of `heap`.
    base_offset: u64,
}

/// What this driver stores in `D3D12DDI_HHEAP::pDrvPrivate`.
///
/// ⚠ Immutable after construction, deliberately. D3D12 DDIs are free-threaded
/// and `device12::device`'s note applies verbatim: concurrent **reads** are the
/// expected case and `&` permits them by construction. Nothing here needs
/// interior mutation, so nothing here has a lock.
struct HeapState {
    /// The engine heap. Current create paths always store `Some`; the option is
    /// retained for the opened-resource lane, which is still a named refusal.
    heap: Option<ID3D12Heap>,
    /// The resource [`map_heap`] maps to obtain the heap's CPU base address.
    ///
    /// ⭐ **At the DDI, Map lives on the HEAP and never on the resource**
    /// (`ResourceHeaps.md:454`, `SPECS.md` §9.7): `ID3D12Resource::Map` does not
    /// reach the driver as a resource map; the runtime maps the heap and derives
    /// per-resource pointers from `pfnCheckSubresourceInfo`. The D3D12 **API**
    /// has no heap map at all, so the only Rust-reachable way to obtain a heap's
    /// CPU address is to map a resource that covers it at offset 0:
    ///
    /// * fused arm -- the base resource, placed at offset 0 of the explicit
    ///   engine heap used for both pre-0115 DDI meanings;
    /// * heap-only arm -- a whole-heap buffer created alongside the heap, see
    ///   [`create_heap_span_buffer`].
    ///
    /// `None` means this heap cannot be mapped, and [`map_heap`] refuses with
    /// `MapHeapNoAnchor` rather than inventing an address.
    map_anchor: Option<ID3D12Resource>,
    /// The heap's size in bytes, as the runtime asked for it. Reported on the
    /// creation evidence line.
    byte_size: u64,
}

/// What this driver stores in `D3D12DDI_HRESOURCE::pDrvPrivate`.
struct ResourceState {
    /// The engine resource. `None` only for the whole-heap object of a heap
    /// whose flags deny buffers, where no covering resource can exist.
    resource: Option<ID3D12Resource>,
    /// Where this object sits inside an engine heap. Current create paths always
    /// store `Some`; the option keeps an unresolved opened-resource path loud
    /// rather than inventing a heap association.
    span: Option<HeapSpan>,
    /// The API description this driver built for the resource, cached so
    /// [`check_subresource_info`] is deterministic for a given resource and
    /// subresource (`ResourceHeaps.md:1437` requires exactly that) without a COM
    /// round trip per query.
    desc: D3D12_RESOURCE_DESC1,
    /// The allocation info this driver reported at create time, so
    /// [`check_existing_resource_allocation_info`] answers the same numbers
    /// [`check_resource_allocation_info`] did. Two independent computations of
    /// one answer is how they come to disagree.
    alloc_info: ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022,
    /// ⛔ **Whether the `hHeap` of this object's fused create/destroy pair is a
    /// private block THIS DRIVER OWNS**, i.e. whether the paired sizing call
    /// asked for one. `true` for the heap-only and committed arms, `false` for
    /// the placed arm, where `hHeap` is an **already-live** heap's handle rather
    /// than a fresh block.
    ///
    /// ⭐ It exists because `pfnDestroyHeapAndResource` is handed the same
    /// `(hHeap, hResource)` pair as the create but **not** the `pCreateHeap` /
    /// `pCreateResource` pointers the create classified the arm from, so the
    /// answer has to be carried on the object. Without it the destroy cannot
    /// tell "my heap block" from "someone else's live heap", and the two halves
    /// of one operation held contradictory beliefs about the same parameter:
    /// `create_heap_and_resource` refuses even to *clear* that word on the
    /// placed arm — at length, calling it *"the single most dangerous line in
    /// the lane if it is wrong"* — while the destroy took the box and dropped
    /// it on every arm. Under the create site's own premise, the first
    /// `Release()` of any placed resource tore down its live parent heap:
    /// `ID3D12Heap` released, map anchor gone, `pfnMapHeap` refusing thereafter,
    /// and a later placed create into that heap misdiagnosed as a *reserved*
    /// resource. Found by the `PARALLEL.md` §10 review, before any VM run.
    ///
    /// ⚠ Recorded on the RESOURCE and not on the heap deliberately: on the
    /// placed arm the heap's own `HeapState` says "I am owned" — truthfully, by
    /// its own create — so it cannot distinguish which operation is asking.
    /// The resource is the object whose create knew the answer.
    owns_heap_block: bool,
    /// Exact committed WDDM identity. Stored on the resource itself so
    /// teardown does not need an address-keyed process-global lookup.
    identity: Option<identity12::AllocationIdentity>,
}

// ⚠⚠ **`D3D12DDI_HRTRESOURCE` is still not a field of `ResourceState`, but the RULE
// that said it never could be is FALSE and is corrected here.**
//
// This block used to read: *"`DDI_REFERENCE.md` §7.3(3) says the `hRT` must be
// stored -- it is the token every callback about that object takes -- and it names
// the callbacks that make that true: `pfnCreateContextCb` takes an
// `HRTCOMMANDQUEUE`, `pfnSetCommandListErrorCb` takes an `HRTCOMMANDLIST`. **Nothing
// in the corelayer or kernel callback tables takes an `HRTRESOURCE`**, so a field
// holding it would be written once and read never."*
//
// The premise's last clause is wrong. `D3D12DDICB_ALLOCATE_0022::hResource` is a
// `HANDLE` at offset 16 (`d3d12umddi.rs:60044-60051`) and it takes exactly that
// token, and D3D11's own path states that the association is *"not optional for
// present-only allocations"* (`umd/src/forward/resource.rs:364-370`) -- which is what
// a presentable back buffer is. `D3D12DDICB_DEALLOCATE_0022::hResource` takes it
// again on the way out.
//
// ⇒ UP-5 threads the handle from `pfnCreateHeapAndResource` into
// [`create_committed_allocation`], which runs *inside* that DDI while the parameter is live, so
// it never needs to be stored on the resource at all. What does need it later is the
// paired `pfnDeallocateCb`, and that is kept in the [`identity12`] table beside the
// allocation handle it releases -- for the two reasons that table exists rather than
// a field: `ResourceState` is written once and never mutated, and the entry's
// lifetime is the allocation's rather than the resource's.
//
// ⛔ So the conclusion survives for `ResourceState` and the reasoning does not. A
// lane that needs the runtime handle for a *third* purpose must re-derive from the
// callback that takes it, not from this block's old claim that none does.

// ---------------------------------------------------------------------------
// Slot access — the D3D12 soundness argument, RE-DERIVED
// ---------------------------------------------------------------------------

/// Borrow the heap state behind a DDI heap handle.
///
/// # ⛔ Why this uses [`Slot::ptr`] and not `Slot::get`
///
/// `umd_common/src/slot.rs:304-322` states plainly that
/// `Slot<Boxed<S>>::get() -> &'static S`'s soundness argument rests on the D3D11
/// runtime's `CUseCountedObject` first-created / last-destroyed ordering, that
/// **no equivalent statement has been located for `d3d12umddi`**, and that a
/// `umd12` caller either owes the derivation or *"must reach for `Self::ptr` and
/// carry the lifetime themselves"*. `PARALLEL.md` §9.4 repeats it: D13 shares
/// declarations, not claims.
///
/// This takes the second option, and it is the stronger one. The returned
/// reference is **not** `'static`: it borrows for the caller's binding, and
/// every caller is one DDI invocation that drops it before returning. What is
/// still needed is that no borrow overlaps the box's teardown, and that argument
/// does not depend on `CUseCountedObject`:
///
/// * the box is written exactly once, inside [`create_heap_and_resource`], into
///   a `pDrvPrivate` block the runtime allocated for *this* object and has not
///   yet handed to any other DDI;
/// * it is taken exactly once, in [`destroy_heap_and_resource`];
/// * `pDrvPrivate` is memory **the runtime owns and frees itself** immediately
///   after that Destroy returns (`DDI_REFERENCE.md` §7.1 step 4). A runtime that
///   dispatched another DDI carrying the same handle concurrently with, or
///   after, its Destroy would be reading an allocation it is about to free or
///   has already freed — a defect in its own memory management, not a race this
///   driver could lose differently.
///
/// ⚠ Concurrent **reads** from FREETHREADED worker threads are permitted by `&`
/// and are the expected case; [`HeapState`] is immutable after construction so
/// there is nothing to serialise.
///
/// # Safety
/// `h_heap.pDrvPrivate`, when non-null, must be the private block this driver
/// wrote for a live heap, and the returned reference must not outlive the DDI
/// call that obtained it.
unsafe fn heap_state<'a>(h_heap: ddi12::D3D12DDI_HHEAP) -> Option<&'a HeapState> {
    // SAFETY: the caller guarantees the slot is either null or this driver's own
    // private block for a heap; `from_priv` only records non-nullness.
    let slot = unsafe { Slot::<Boxed<HeapState>>::from_priv(h_heap.pDrvPrivate) }?;
    // SAFETY: same precondition; `ptr` reads the slot word and casts it, which
    // is the one cast `slot` exists to confine to itself.
    let p = unsafe { slot.ptr() };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null per the check, points at the `Box<HeapState>` this driver
    // leaked into the slot, and the lifetime is the caller's per the argument
    // above.
    Some(unsafe { &*p })
}

/// Borrow the resource state behind a DDI resource handle.
///
/// The [`heap_state`] argument applies verbatim, with `pfnCreateHeapAndResource`
/// and `pfnDestroyHeapAndResource` as the same single writer and single taker.
///
/// # Safety
/// As [`heap_state`], for a resource handle.
unsafe fn resource_state<'a>(h_resource: ddi12::D3D12DDI_HRESOURCE) -> Option<&'a ResourceState> {
    // SAFETY: as `heap_state`.
    let slot = unsafe { Slot::<Boxed<ResourceState>>::from_priv(h_resource.pDrvPrivate) }?;
    // SAFETY: as `heap_state`.
    let p = unsafe { slot.ptr() };
    if p.is_null() {
        return None;
    }
    // SAFETY: as `heap_state`.
    Some(unsafe { &*p })
}

/// The engine resource behind a DDI resource handle, borrowed.
///
/// ⭐ `pub(crate)` because it is the seam every other lane needs: L3c's copies,
/// L5's view creation and L8's present all take a `D3D12DDI_HRESOURCE` and need
/// the `ID3D12Resource` behind it, and R803's scar is that the payload must be
/// derived from the handle **type** in one place rather than decoded at each
/// call site. This file owns resource handles, so this is that one place.
///
/// ⚠ A **shared reference**, not a `ManuallyDrop<ID3D12Resource>`. The state box
/// keeps the owning reference; borrowing it as `&` makes releasing it
/// unwritable, where a `ManuallyDrop` merely makes it unlikely. That is the
/// stronger half of `bridge12.rs`'s owned-vs-borrowed rule, available here only
/// because this driver keeps the object rather than receiving a raw `usize`
/// across the FFI.
///
/// # Safety
/// As [`resource_state`]. The returned reference must not outlive the DDI call.
pub(crate) unsafe fn engine_resource<'a>(
    h_resource: ddi12::D3D12DDI_HRESOURCE,
) -> Option<&'a ID3D12Resource> {
    // SAFETY: forwarded unchanged.
    let state = unsafe { resource_state::<'a>(h_resource) }?;
    state.resource.as_ref()
}

/// Copy the exact committed WDDM identity stored on this resource. The runtime
/// owns the resource for the duration of each caller, so this snapshot cannot
/// race its destroy; no process-global lookup or pointer key participates.
pub(crate) unsafe fn allocation_identity(
    h_resource: ddi12::D3D12DDI_HRESOURCE,
) -> Option<identity12::AllocationIdentity> {
    unsafe { resource_state(h_resource) }?.identity
}

/// The engine device, at the `ID3D12Device10` revision this lane forwards to.
///
/// Returns an **owned** reference (`Interface::cast` is a `QueryInterface`), so
/// the caller releases it by dropping. The QI is per call rather than cached in
/// `HeliosD3D12Device` because caching would mean appending a field to
/// `device12.rs`, a shared file three other lanes are also editing
/// (`PARALLEL.md` §5), for a GUID compare against an already-live object.
fn engine_device10(dev: &HeliosD3D12Device) -> Option<ID3D12Device10> {
    let Some(engine) = dev.engine.d3d12_device() else {
        // Unreachable by construction: `BridgeDevice12::create` folds a null
        // engine into `None`, so a live device always carries one. Counted
        // because "unreachable by construction" is a claim about a cross-FFI
        // contract, and this is where it would be observed breaking.
        note_refusal(&L4_REFUSALS.resource_no_device);
        return None;
    };
    match engine.cast::<ID3D12Device10>() {
        Ok(device10) => Some(device10),
        Err(err) => {
            L4_REFUSALS.resource_device10_unavailable.bump();
            let n = L4_REFUSALS.resource_device10_unavailable.get();
            if n <= LOG_BUDGET {
                log_error!(
                    "L4: the engine does not expose ID3D12Device10 (hr={:#010x}) -- resource \
                     creation needs CreateCommittedResource3/CreatePlacedResource2 for the \
                     barrier layout and castable formats the DDI carries (x{n})",
                    err.code().0 as u32,
                );
            }
            None
        }
    }
}

// ---------------------------------------------------------------------------
// DDI -> API translation. Every enum by `match`, never by cast.
// ---------------------------------------------------------------------------

/// `D3D12DDI_MEMORY_POOL` -> `D3D12_MEMORY_POOL`.
///
/// ⛔ **Offset by one.** DDI `L0 = 0, L1 = 1`; API `UNKNOWN = 0, L0 = 1,
/// L1 = 2`. A cast would report every system-memory heap as "driver chooses".
/// `ResourceHeaps.md:915`: *"the driver must honour the requested memory pool
/// and CPU page property exactly"*, so there is no defensible default here — an
/// unrecognised value is counted and mapped to L0, the conservative pool
/// (system memory is always addressable; local video memory is not).
fn memory_pool(pool: ddi12::D3D12DDI_MEMORY_POOL) -> D3D12_MEMORY_POOL {
    match pool {
        v::POOL_L0 => D3D12_MEMORY_POOL_L0,
        v::POOL_L1 => D3D12_MEMORY_POOL_L1,
        other => {
            L4_REFUSALS.heap_property_unrepresentable.bump();
            let n = L4_REFUSALS.heap_property_unrepresentable.get();
            if n <= LOG_BUDGET {
                log_error!("L4: unknown D3D12DDI_MEMORY_POOL {other} -> L0 (x{n})");
            }
            D3D12_MEMORY_POOL_L0
        }
    }
}

/// `D3D12DDI_CPU_PAGE_PROPERTY` -> `D3D12_CPU_PAGE_PROPERTY`.
///
/// ⛔ **Offset by one, and the failure is worse than the pool's.** DDI
/// `NOT_AVAILABLE = 0`; API `UNKNOWN = 0`. A cast would turn every
/// GPU-only heap into "driver chooses", which is how a device-local heap
/// silently becomes CPU-visible.
fn cpu_page_property(prop: ddi12::D3D12DDI_CPU_PAGE_PROPERTY) -> D3D12_CPU_PAGE_PROPERTY {
    match prop {
        v::CPU_NOT_AVAILABLE => D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE,
        v::CPU_WRITE_COMBINE => D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE,
        v::CPU_WRITE_BACK => D3D12_CPU_PAGE_PROPERTY_WRITE_BACK,
        other => {
            L4_REFUSALS.heap_property_unrepresentable.bump();
            let n = L4_REFUSALS.heap_property_unrepresentable.get();
            if n <= LOG_BUDGET {
                log_error!(
                    "L4: unknown D3D12DDI_CPU_PAGE_PROPERTY {other} -> NOT_AVAILABLE (x{n})"
                );
            }
            D3D12_CPU_PAGE_PROPERTY_NOT_AVAILABLE
        }
    }
}

fn cpu_page_property_is_visible(prop: ddi12::D3D12DDI_CPU_PAGE_PROPERTY) -> bool {
    matches!(prop, v::CPU_WRITE_COMBINE | v::CPU_WRITE_BACK)
}

/// `D3D12DDI_HEAP_FLAGS` (positive ALLOW bits) -> `D3D12_HEAP_FLAGS` (DENY bits).
///
/// ⛔ **The polarity inverts.** `ResourceHeaps.md:874`: *"the app writes
/// DENY_BUFFERS / DENY_RT_DS_TEXTURES / DENY_NON_RT_DS_TEXTURES, but the driver
/// receives positive ALLOW-style bits at exactly the values 0x2 / 0x4 / 0x20"*.
///
/// ⚠ And `ResourceHeaps.md:907`: *"Heap tier 1 does NOT mean the driver may
/// assume mutually exclusive category bits at the DDI — the runtime deliberately
/// sends heaps with both texture-category bits set and the UMD is required to
/// handle them."* This driver reports `ResourceHeapTier = 1` (`caps12.rs:570`)
/// and this function makes no exclusivity assumption: each bit is tested and
/// inverted on its own.
///
/// The DDI bits with no API counterpart — `COHERENT_SYSTEMWIDE`,
/// `_0041_DENY_L0_DEMOTION`, and any future one — are dropped and counted into
/// one shared bucket, which is honest because dropping either is benign.
///
/// ⛔ **`PRIMARY` is NOT in that bucket, and separating it is the point.**
/// `SPECS.md` §9.7 from `ResourceHeaps.md:897`: *"The PRIMARY heap flag is the
/// D3D12 replacement for `DXGI_DDI_PRIMARY_DESC`: no primary description is ever
/// passed to the UMD, the flag alone declares the primary, and receiving it
/// obliges the driver to create a resource simultaneously with the heap."* It is
/// therefore the only channel by which this driver can learn it is building the
/// thing that reaches the scanout, and dropping it into an aggregate this lane's
/// own evidence contract grades *"non-zero OK"* would hide a demoted primary
/// behind two genuinely benign bits.
///
/// ⭐ **At UP-5 it stopped being dropped on the committed arm.** `primary` selects
/// the arm's behaviour: [`PrimaryTranslation::VenusExport`] turns the declaration
/// into [`HELIOS_HEAP_FLAG_VENUS_EXPORT`], the fork's private request for
/// exportable dedicated memory, which is the only way the resource can end up with
/// a venus resource id for [`create_committed_allocation`] to hand the kernel.
/// [`PrimaryTranslation::Dropped`] keeps the old behaviour and the old counter, and
/// is reachable only from the heap-only arm — where the declaration cannot be
/// honoured at all.
fn heap_flags(flags: ddi12::D3D12DDI_HEAP_FLAGS, primary: PrimaryTranslation) -> D3D12_HEAP_FLAGS {
    let mut out = D3D12_HEAP_FLAG_NONE;
    if flags & v::HEAP_ALLOW_BUFFERS == 0 {
        out |= D3D12_HEAP_FLAG_DENY_BUFFERS;
    }
    if flags & v::HEAP_ALLOW_NON_RT_DS_TEXTURES == 0 {
        out |= D3D12_HEAP_FLAG_DENY_NON_RT_DS_TEXTURES;
    }
    if flags & v::HEAP_ALLOW_RT_DS_TEXTURES == 0 {
        out |= D3D12_HEAP_FLAG_DENY_RT_DS_TEXTURES;
    }
    if flags & v::HEAP_PRIMARY != 0 {
        match primary {
            PrimaryTranslation::VenusExport => {
                out |= HELIOS_HEAP_FLAG_VENUS_EXPORT;
                L4_REFUSALS.heap_primary_venus_export.bump();
                let n = L4_REFUSALS.heap_primary_venus_export.get();
                if n <= LOG_BUDGET {
                    log_error!(
                        "L4: D3D12DDI_HEAP_FLAG_PRIMARY arrived on the committed arm -- \
                         requesting exportable dedicated memory from the engine via the \
                         private heap flag {:#x}, which is what makes the memory \
                         allocator-DEDICATED so the HWA2 descriptor's byte_size is the \
                         whole bound VkDeviceMemory (x{n})",
                        HELIOS_HEAP_FLAG_VENUS_EXPORT.0,
                    );
                }
            }
            PrimaryTranslation::Dropped => {
                L4_REFUSALS.heap_primary_flag_dropped.bump();
                let n = L4_REFUSALS.heap_primary_flag_dropped.get();
                if n <= LOG_BUDGET {
                    log_error!(
                        "L4: D3D12DDI_HEAP_FLAG_PRIMARY arrived on an arm that cannot honour \
                         it -- dropped, because there is no resource to give a kernel \
                         allocation to (x{n})"
                    );
                }
            }
        }
    }
    let unmapped = flags
        & !(v::HEAP_ALLOW_BUFFERS
            | v::HEAP_ALLOW_NON_RT_DS_TEXTURES
            | v::HEAP_ALLOW_RT_DS_TEXTURES
            | v::HEAP_PRIMARY);
    if unmapped != 0 {
        L4_REFUSALS.heap_flag_unrepresentable.bump();
        let n = L4_REFUSALS.heap_flag_unrepresentable.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: D3D12DDI_HEAP_FLAGS {unmapped:#x} have no D3D12_HEAP_FLAGS counterpart -- \
                 dropped (x{n})"
            );
        }
    }
    out
}

/// `D3D12DDI_RESOURCE_TYPE` -> `D3D12_RESOURCE_DIMENSION`.
///
/// The two enums agree numerically (BUFFER 1, TEXTURE1D 2, TEXTURE2D 3,
/// TEXTURE3D 4) but they are still matched rather than cast, because agreement
/// today is not a contract and the compiler cannot see the coincidence.
fn resource_dimension(
    resource_type: ddi12::D3D12DDI_RESOURCE_TYPE,
) -> Option<D3D12_RESOURCE_DIMENSION> {
    match resource_type {
        v::RT_BUFFER => Some(D3D12_RESOURCE_DIMENSION_BUFFER),
        v::RT_TEXTURE1D => Some(D3D12_RESOURCE_DIMENSION_TEXTURE1D),
        v::RT_TEXTURE2D => Some(D3D12_RESOURCE_DIMENSION_TEXTURE2D),
        v::RT_TEXTURE3D => Some(D3D12_RESOURCE_DIMENSION_TEXTURE3D),
        _ => None,
    }
}

/// `D3D12DDI_TEXTURE_LAYOUT` -> `D3D12_TEXTURE_LAYOUT`.
///
/// The first four agree numerically; `D3D12DDI_TL_DEVICE_DEPENDENT_SWIZZLE_0`
/// (4) has no API counterpart and is counted, then passed as `UNKNOWN` — which
/// is the API's own name for "the driver picks", i.e. exactly what a
/// device-dependent swizzle is from the runtime's side.
fn texture_layout(layout: ddi12::D3D12DDI_TEXTURE_LAYOUT) -> D3D12_TEXTURE_LAYOUT {
    match layout {
        v::TL_UNDEFINED => D3D12_TEXTURE_LAYOUT_UNKNOWN,
        v::TL_ROW_MAJOR => D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        v::TL_64KB_TILE_UNDEFINED_SWIZZLE => D3D12_TEXTURE_LAYOUT_64KB_UNDEFINED_SWIZZLE,
        v::TL_64KB_TILE_STANDARD_SWIZZLE => D3D12_TEXTURE_LAYOUT_64KB_STANDARD_SWIZZLE,
        other => {
            L4_REFUSALS.resource_layout_unrepresentable.bump();
            let n = L4_REFUSALS.resource_layout_unrepresentable.get();
            if n <= LOG_BUDGET {
                log_error!(
                    "L4: D3D12DDI_TEXTURE_LAYOUT {other} has no D3D12_TEXTURE_LAYOUT counterpart \
                     -> UNKNOWN (x{n})"
                );
            }
            D3D12_TEXTURE_LAYOUT_UNKNOWN
        }
    }
}

/// `D3D12DDI_RESOURCE_FLAGS_0003` -> `D3D12_RESOURCE_FLAGS`.
///
/// ⛔ A different bit layout AND one inverted bit. The DDI's
/// `SHADER_RESOURCE = 0x10` is a positive "this resource may be sampled"; the
/// API expresses the same fact as the **absence** of
/// `DENY_SHADER_RESOURCE = 0x8`. Every other bit moves position:
/// `UNORDERED_ACCESS` 0x80 -> 0x4, `CROSS_ADAPTER` 0x4 -> 0x10,
/// `SIMULTANEOUS_ACCESS` 0x8 -> 0x20, `VIDEO_DECODE_REFERENCE_ONLY` 0x20 ->
/// 0x40, `VIDEO_ENCODE_REFERENCE_ONLY` 0x1000 -> 0x80,
/// `RAYTRACING_ACCELERATION_STRUCTURE` 0x2000 -> 0x100.
///
/// The DDI-only bits are dropped and counted: `_0020_CONTENT_PROTECTION`
/// (this driver reports no protected-resource support), the two
/// `_0041_ONLY_*_TEXTURE_PLACEMENT` placement hints, `_0041_4MB_ALIGNED` and
/// `_0073_SAMPLER_FEEDBACK`.
fn resource_flags(flags: ddi12::D3D12DDI_RESOURCE_FLAGS_0003) -> D3D12_RESOURCE_FLAGS {
    let mut out = D3D12_RESOURCE_FLAG_NONE;
    if flags & v::RES_RENDER_TARGET != 0 {
        out |= D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    }
    if flags & v::RES_DEPTH_STENCIL != 0 {
        out |= D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL;
    }
    if flags & v::RES_UNORDERED_ACCESS != 0 {
        out |= D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
    }
    if flags & v::RES_CROSS_ADAPTER != 0 {
        out |= D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER;
    }
    if flags & v::RES_SIMULTANEOUS_ACCESS != 0 {
        out |= D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS;
    }
    if flags & v::RES_VIDEO_DECODE_REFERENCE_ONLY != 0 {
        out |= D3D12_RESOURCE_FLAG_VIDEO_DECODE_REFERENCE_ONLY;
    }
    if flags & v::RES_VIDEO_ENCODE_REFERENCE_ONLY != 0 {
        out |= D3D12_RESOURCE_FLAG_VIDEO_ENCODE_REFERENCE_ONLY;
    }
    if flags & v::RES_RAYTRACING_ACCELERATION_STRUCTURE != 0 {
        out |= D3D12_RESOURCE_FLAG_RAYTRACING_ACCELERATION_STRUCTURE;
    }
    // ⛔ The inversion, and it is the whole reason this is not a cast.
    if flags & v::RES_SHADER_RESOURCE == 0 {
        out |= D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE;
    }

    let translated = v::RES_RENDER_TARGET
        | v::RES_DEPTH_STENCIL
        | v::RES_UNORDERED_ACCESS
        | v::RES_CROSS_ADAPTER
        | v::RES_SIMULTANEOUS_ACCESS
        | v::RES_VIDEO_DECODE_REFERENCE_ONLY
        | v::RES_VIDEO_ENCODE_REFERENCE_ONLY
        | v::RES_RAYTRACING_ACCELERATION_STRUCTURE
        | v::RES_SHADER_RESOURCE;
    let unmapped = flags & !translated;
    if unmapped != 0 {
        L4_REFUSALS.resource_flag_unrepresentable.bump();
        let n = L4_REFUSALS.resource_flag_unrepresentable.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: D3D12DDI_RESOURCE_FLAGS_0003 {unmapped:#x} have no D3D12_RESOURCE_FLAGS \
                 counterpart -- dropped (x{n})"
            );
        }
    }
    out
}

/// `D3D12DDI_BARRIER_LAYOUT` -> `D3D12_BARRIER_LAYOUT`.
///
/// The two enums agree exactly over `UNDEFINED = -1` through
/// `VIDEO_QUEUE_COMMON = 30`, which is asserted below rather than assumed. What
/// the DDI adds and the API does not have is the legacy family — value 31 and
/// the four negative `LEGACY_*` enumerators — which is how the runtime hands a
/// driver an initial layout derived from a legacy `D3D12_RESOURCE_STATES`. Each
/// maps to the nearest API layout and is counted, because the mapping is this
/// driver's judgement rather than the header's.
///
/// ⚠ **Buffers must be `UNDEFINED`.** vkd3d rejects any other layout for a
/// buffer outright — *"Using non-undefined layout for buffer. This is not
/// allowed."*, `libs/vkd3d/device.c:9427` and `:9463`, `E_INVALIDARG`. That is
/// the API's rule, not vkd3d's invention, so a buffer's layout is forced and
/// counted here rather than discovered as a failed create.
fn barrier_layout(layout: ddi12::D3D12DDI_BARRIER_LAYOUT, is_buffer: bool) -> D3D12_BARRIER_LAYOUT {
    if is_buffer {
        if layout != v::LAYOUT_UNDEFINED {
            L4_REFUSALS.resource_barrier_layout_coerced.bump();
            let n = L4_REFUSALS.resource_barrier_layout_coerced.get();
            if n <= LOG_BUDGET {
                log_error!(
                    "L4: buffer arrived with InitialBarrierLayout {layout} -- forcing UNDEFINED, \
                     which is the only layout a buffer may carry (x{n})"
                );
            }
        }
        return D3D12_BARRIER_LAYOUT_UNDEFINED;
    }

    // The shared range, passed through by value. The four anchors below are
    // asserted at compile time so "the enums agree" is checked, not claimed.
    const _: () = assert!(v::LAYOUT_UNDEFINED == D3D12_BARRIER_LAYOUT_UNDEFINED.0);
    const _: () = assert!(v::LAYOUT_COMMON == D3D12_BARRIER_LAYOUT_COMMON.0);
    const _: () =
        assert!(v::LAYOUT_VIDEO_QUEUE_COMMON == D3D12_BARRIER_LAYOUT_VIDEO_QUEUE_COMMON.0);
    if (v::LAYOUT_UNDEFINED..=v::LAYOUT_VIDEO_QUEUE_COMMON).contains(&layout) {
        return D3D12_BARRIER_LAYOUT(layout);
    }

    let coerced = match layout {
        v::LAYOUT_LEGACY_GENERIC_READ => D3D12_BARRIER_LAYOUT_GENERIC_READ,
        v::LAYOUT_LEGACY_COPY_SOURCE => D3D12_BARRIER_LAYOUT_COPY_SOURCE,
        v::LAYOUT_LEGACY_COPY_DEST => D3D12_BARRIER_LAYOUT_COPY_DEST,
        v::LAYOUT_LEGACY_SHADER_RESOURCE | v::LAYOUT_LEGACY_PIXEL_SHADER_RESOURCE => {
            D3D12_BARRIER_LAYOUT_SHADER_RESOURCE
        }
        // ⚠ COMMON and not UNDEFINED: UNDEFINED tells the engine the contents
        // are discardable, which is a claim about the resource rather than about
        // this driver's ignorance. vkd3d makes the same choice for an
        // unrecognised layout and says so (`device.c`, the generic fallback in
        // `vkd3d_barrier_layout_to_resource_state`).
        _ => D3D12_BARRIER_LAYOUT_COMMON,
    };
    L4_REFUSALS.resource_barrier_layout_coerced.bump();
    let n = L4_REFUSALS.resource_barrier_layout_coerced.get();
    if n <= LOG_BUDGET {
        log_error!(
            "L4: D3D12DDI_BARRIER_LAYOUT {layout} is outside the API enum -> {} (x{n})",
            coerced.0,
        );
    }
    coerced
}

/// Build the API resource description from the DDI's.
///
/// ⛔ **`Alignment` is always 0, and it is NOT a parameter.** The DDI moved the
/// field out of the struct — `D3D12DDIARG_CREATERESOURCE_0111` has no
/// `Alignment` while `D3D12_RESOURCE_DESC` does — and `pfnCreateHeapAndResource`
/// has no such argument either, so the create path can only ever say 0: the
/// API's *"the driver picks"*, which is exactly what the DDI means by omitting
/// it. `pfnCheckResourceAllocationInfo` does receive the application's requested
/// value as `AlignmentRestriction`, and it does **not** pass it here — see that
/// function's doc for why forwarding it would make the Check slots and the
/// create arms answer different questions about one resource. Making it a
/// constant rather than a parameter is what stops the two from drifting apart
/// again.
///
/// # Safety
/// `a` must be a live `D3D12DDIARG_CREATERESOURCE_0111`.
unsafe fn resource_desc1(
    a: &ddi12::D3D12DDIARG_CREATERESOURCE_0111,
) -> Option<D3D12_RESOURCE_DESC1> {
    if a.LayoutGuid.Data1 != 0
        || a.LayoutGuid.Data2 != 0
        || a.LayoutGuid.Data3 != 0
        || a.LayoutGuid.Data4 != [0; 8]
    {
        note_refusal(&L4_REFUSALS.resource_layout_guid_refused);
        return None;
    }
    let dimension = resource_dimension(a.ResourceType)?;
    Some(D3D12_RESOURCE_DESC1 {
        Dimension: dimension,
        Alignment: 0,
        Width: a.Width,
        Height: a.Height,
        DepthOrArraySize: a.DepthOrArraySize,
        MipLevels: a.MipLevels,
        Format: DXGI_FORMAT(a.Format),
        // Field by field, never a transmute: the two structs happen to be two
        // `UINT`s each, and `ARCHITECTURE.md` §12 rule 1 is that a happening is
        // not an ABI contract.
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: a.SampleDesc.Count,
            Quality: a.SampleDesc.Quality,
        },
        Layout: texture_layout(a.Layout),
        Flags: resource_flags(a.Flags),
        SamplerFeedbackMipRegion: D3D12_MIP_REGION {
            Width: a.SamplerFeedbackMipRegion.Width,
            Height: a.SamplerFeedbackMipRegion.Height,
            Depth: a.SamplerFeedbackMipRegion.Depth,
        },
    })
}

/// `D3D12_RESOURCE_DESC1` -> `D3D12_RESOURCE_DESC`, dropping the mip region.
///
/// The older entry points (`GetResourceAllocationInfo`, `GetCopyableFootprints`)
/// take the shorter struct. The mip region only describes sampler feedback,
/// which has no bearing on either answer.
fn resource_desc(desc1: &D3D12_RESOURCE_DESC1) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: desc1.Dimension,
        Alignment: desc1.Alignment,
        Width: desc1.Width,
        Height: desc1.Height,
        DepthOrArraySize: desc1.DepthOrArraySize,
        MipLevels: desc1.MipLevels,
        Format: desc1.Format,
        SampleDesc: desc1.SampleDesc,
        Layout: desc1.Layout,
        Flags: desc1.Flags,
    }
}

/// The optimized clear value, when the runtime supplied one.
///
/// ⚠ Which arm of the DDI union is live is decided by the **resource's** flags,
/// exactly as D3D12 defines an optimized clear value: a depth-stencil resource
/// carries `{Depth, Stencil}` and everything else carries a colour. There is no
/// discriminator in `D3D12DDI_CLEAR_VALUES` itself.
///
/// # Safety
/// `clear`, when non-null, must be a live `D3D12DDI_CLEAR_VALUES`.
unsafe fn clear_value(
    clear: *const ddi12::D3D12DDI_CLEAR_VALUES,
    ddi_flags: ddi12::D3D12DDI_RESOURCE_FLAGS_0003,
) -> Option<D3D12_CLEAR_VALUE> {
    if clear.is_null() {
        return None;
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_opt_ CONST`, so a
    // non-null pointer is a live struct for the duration of the call.
    let c = unsafe { &*clear };
    let anonymous = if ddi_flags & v::RES_DEPTH_STENCIL != 0 {
        // SAFETY: reading the union's `DepthStencil` arm. Both arms are plain
        // data with no invalid bit patterns (`[f32; 4]` and `{f32, u8}`), so any
        // initialisation the runtime performed makes this read defined; which
        // arm is *meaningful* is the resource-flag test above.
        D3D12_CLEAR_VALUE_0 {
            DepthStencil: D3D12_DEPTH_STENCIL_VALUE {
                Depth: unsafe { c.__bindgen_anon_1.DepthStencil }.Depth,
                Stencil: unsafe { c.__bindgen_anon_1.DepthStencil }.Stencil,
            },
        }
    } else {
        // SAFETY: as above, for the `Color` arm.
        D3D12_CLEAR_VALUE_0 {
            Color: unsafe { c.__bindgen_anon_1.Color },
        }
    };
    Some(D3D12_CLEAR_VALUE {
        Format: DXGI_FORMAT(c.Format),
        Anonymous: anonymous,
    })
}

// ---------------------------------------------------------------------------
// (g) pfnCalcPrivateHeapAndResourceSizes / pfnCreateHeapAndResource /
//     pfnDestroyHeapAndResource
// ---------------------------------------------------------------------------

/// The two private-block sizes, as ONE function of the arguments.
///
/// ⛔ **Called by both the sizing DDI and the create DDI**, for the reason
/// `device12.rs`'s `device_private_size` states for the device block: a size
/// that is a function of the arguments at one site and a constant at the other
/// is a buffer overrun waiting for whichever caller disagrees. Here the stakes
/// are higher than there, because there are **two** sizes and pairing them the
/// wrong way round would put a `Box<HeapState>` pointer in the resource slot.
///
/// * The **heap** block exists only when the runtime asked for a heap. ⭐ This
///   is not economy, it is required: on the placed arm (`pCreateHeap == NULL`)
///   the `hHeap` the runtime passes must remain the *existing* heap's, and
///   asking for a fresh block would invite the runtime to hand a newly allocated
///   empty one instead.
/// * The **resource** block is asked for unconditionally, even on the heap-only
///   arm. `D3D12DDIARG_CREATERESOURCE_0111` has no field naming a heap, and
///   placement is expressed as `ReuseBufferGPUVA.hResource` — a *resource*
///   handle (`ResourceHeaps.md:1212`). Whether the runtime hands this driver an
///   `hResource` for a heap's own address range is not established here and can
///   only be settled on the VM, so one pointer-sized block is reserved for it:
///   if it arrives, [`create_heap_only`] records the heap span in it and the
///   placement chain resolves; if it does not, eight bytes go unused.
///
/// Both sizes are one machine word because that is what the `umd_common::slot`
/// encoding stores — the payload lives in a `Box` this driver owns and the
/// runtime's block holds only the pointer. `umd/src/forward/resource.rs:12-17`
/// is the D3D11 site that answers `8` for exactly this reason.
fn heap_and_resource_private_sizes(
    p_heap: *const ddi12::D3D12DDIARG_CREATEHEAP_0001,
    p_resource: *const ddi12::D3D12DDIARG_CREATERESOURCE_0111,
) -> ddi12::D3D12DDI_HEAP_AND_RESOURCE_SIZES {
    let word = core::mem::size_of::<*mut c_void>() as ddi12::SIZE_T;
    let both_null = p_heap.is_null() && p_resource.is_null();
    ddi12::D3D12DDI_HEAP_AND_RESOURCE_SIZES {
        Heap: if p_heap.is_null() { 0 } else { word },
        Resource: if both_null { 0 } else { word },
    }
}

/// `pfnCalcPrivateHeapAndResourceSizes`.
///
/// ⚠ Returns a **two-word struct by value**. On MSVC x64 that is an sret return
/// through a hidden pointer, not `RAX:RDX` — which is why the signature comes
/// from bindgen and this function is only ever installed by assigning it to the
/// table field (`DDI_REFERENCE.md` §7.3(1): *"do not hand-write it"*).
///
/// # Safety
/// `p_heap` and `p_resource`, when non-null, must be live for the call. Neither
/// is dereferenced here — only their nullness selects the answer.
unsafe extern "C" fn calc_private_heap_and_resource_sizes(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    p_heap: *const ddi12::D3D12DDIARG_CREATEHEAP_0001,
    p_resource: *const ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    h_protected_session: ddi12::D3D12DDI_HPROTECTEDRESOURCESESSION_0030,
) -> ddi12::D3D12DDI_HEAP_AND_RESOURCE_SIZES {
    if !h_protected_session.pDrvPrivate.is_null() {
        note_refusal(&L4_REFUSALS.protected_session_ignored);
    }
    if p_heap.is_null() && p_resource.is_null() {
        // The fourth row of the arm table: illegal. Sizing cannot refuse -- the
        // DDI returns a struct -- so it answers zero and the paired create
        // refuses the same world explicitly.
        note_refusal(&L4_REFUSALS.heap_resource_calc_bad_arg);
    }
    heap_and_resource_private_sizes(p_heap, p_resource)
}

/// Create the whole-heap buffer that gives a standalone heap a CPU address and a
/// GPU virtual address.
///
/// ⭐ **Why this exists at all.** `pfnMapHeap` is heap-scoped and the D3D12 API
/// has no heap map; `pfnCheckResourceVirtualAddress` is resource-scoped and
/// `ID3D12Heap` has no GPU VA accessor. A buffer placed at offset 0 covering the
/// whole heap answers both, and it is the only construction reachable through
/// the public `ID3D12*` surface this driver forwards to.
///
/// ⚠ **This is the riskiest construct in the lane**, so it is arranged to fail
/// softly: it is attempted only when the heap's flags allow buffers, its failure
/// is counted rather than fatal, and a heap without one simply refuses
/// `pfnMapHeap` with its own counter. A heap that denies buffers (an RT/DS-only
/// heap on a tier-1 adapter, which is what `caps12.rs:570` reports) cannot have
/// one by construction, and such heaps are not CPU-mappable anyway.
///
/// The description is the D3D12 buffer canon — `Height`, `DepthOrArraySize` and
/// `MipLevels` all 1, `Format` UNKNOWN, one sample, `ROW_MAJOR` — and the
/// initial layout is `UNDEFINED`, which is the only layout a buffer may carry.
fn create_heap_span_buffer(
    device10: &ID3D12Device10,
    heap: &ID3D12Heap,
    byte_size: u64,
    ddi_heap_flags: ddi12::D3D12DDI_HEAP_FLAGS,
) -> Option<ID3D12Resource> {
    if ddi_heap_flags & v::HEAP_ALLOW_BUFFERS == 0 {
        L4_REFUSALS.heap_span_buffer_unavailable.bump();
        let n = L4_REFUSALS.heap_span_buffer_unavailable.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: heap flags {ddi_heap_flags:#x} deny buffers, so this heap gets no span \
                 buffer -- MapHeap and CheckResourceVirtualAddress will refuse for it (x{n})"
            );
        }
        return None;
    }

    let desc = D3D12_RESOURCE_DESC1 {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Alignment: 0,
        Width: byte_size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT(0),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
        SamplerFeedbackMipRegion: D3D12_MIP_REGION {
            Width: 0,
            Height: 0,
            Depth: 0,
        },
    };
    let mut out: Option<ID3D12Resource> = None;
    // SAFETY: `desc` is a live local of exactly the struct the entry point
    // names; `heap` is the engine heap this driver just created and holds a
    // reference to; `out` is a live local the engine writes an owned reference
    // into. No castable formats and no clear value for a plain buffer.
    let created = unsafe {
        device10.CreatePlacedResource2(
            heap,
            0,
            &desc,
            D3D12_BARRIER_LAYOUT_UNDEFINED,
            None,
            None,
            &mut out,
        )
    };
    match created {
        Ok(()) => out,
        Err(err) => {
            L4_REFUSALS.heap_span_buffer_unavailable.bump();
            let n = L4_REFUSALS.heap_span_buffer_unavailable.get();
            if n <= LOG_BUDGET {
                log_error!(
                    "L4: whole-heap span buffer ({byte_size} B) refused by the engine \
                     hr={:#010x} -- MapHeap and CheckResourceVirtualAddress will refuse for this \
                     heap (x{n})",
                    err.code().0 as u32,
                );
            }
            None
        }
    }
}

/// Report a heap node mask this single-node adapter did not expect.
///
/// ⛔ Reported, never rewritten. `ResourceHeaps.md:915`: *"the driver must
/// honour the requested memory pool and CPU page property exactly"* -- the same
/// posture applies to the node masks, and normalising one to 1 would be this
/// driver deciding which adapter the runtime meant. Both heap-creating arms call
/// it from the same `D3D12DDIARG_CREATEHEAP_0001` contract.
fn note_node_masks(a: &ddi12::D3D12DDIARG_CREATEHEAP_0001) {
    if a.CreationNodeMask == 1 && a.VisibleNodeMask == 1 {
        return;
    }
    L4_REFUSALS.heap_node_mask_unexpected.bump();
    let n = L4_REFUSALS.heap_node_mask_unexpected.get();
    if n <= LOG_BUDGET {
        log_error!(
            "L4: heap CreationNodeMask={} VisibleNodeMask={} on a single-node adapter -- \
             forwarded unchanged (x{n})",
            a.CreationNodeMask,
            a.VisibleNodeMask,
        );
    }
}

/// Arm 2 of the fused create: `pCreateHeap` alone -> `ID3D12Device::CreateHeap`.
///
/// # Safety
/// `a` must be a live `D3D12DDIARG_CREATEHEAP_0001`, and the two slots must be
/// this driver's own private blocks, already cleared by the caller.
unsafe fn create_heap_only(
    device10: &ID3D12Device10,
    a: &ddi12::D3D12DDIARG_CREATEHEAP_0001,
    heap_slot: Slot<Boxed<HeapState>>,
    resource_slot: Option<Slot<Boxed<ResourceState>>>,
) -> Hresult {
    note_node_masks(a);

    // ⛔ A `PRIMARY` heap with no resource description contradicts the DDI's own
    // contract: `SPECS.md` §9.7 from `ResourceHeaps.md:897` says receiving the
    // flag *"obliges the driver to create a resource simultaneously with the
    // heap"*, i.e. a primary is always the committed arm. If it ever arrives here
    // the UP-4 identity table cannot record it -- there is no `ID3D12Resource` to
    // key on -- so the primary would be silently unrecorded. Counted, not
    // assumed away, because `create_committed_allocation`'s admission predicate
    // depends on the obligation holding.
    if a.Flags & v::HEAP_PRIMARY != 0 {
        note_refusal(&L4_REFUSALS.heap_primary_without_resource);
        log_error!(
            "L4: D3D12DDI_HEAP_FLAG_PRIMARY on the heap-ONLY arm -- ResourceHeaps.md:897 says a \
             primary heap must come with a resource description, so this primary has no resource \
             to record an identity against (size={} flags={:#x})",
            a.ByteSize,
            a.Flags,
        );
    }

    let desc = D3D12_HEAP_DESC {
        SizeInBytes: a.ByteSize,
        Properties: D3D12_HEAP_PROPERTIES {
            // ⭐ CUSTOM is forced, not chosen. `SPECS.md` §9.7 from
            // `D3D12GPUUploadHeaps.md:41`: *"there is exactly one heap-creation
            // argument struct in SDK 26100, `D3D12DDIARG_CREATEHEAP_0001`, and
            // it carries no heap type"*. The DDI hands a memory pool and a CPU
            // page property, which is precisely what a CUSTOM heap is; there is
            // no way to recover UPLOAD/READBACK/DEFAULT and no need to.
            Type: D3D12_HEAP_TYPE_CUSTOM,
            CPUPageProperty: cpu_page_property(a.CPUPageProperty),
            MemoryPoolPreference: memory_pool(a.MemoryPool),
            CreationNodeMask: a.CreationNodeMask,
            VisibleNodeMask: a.VisibleNodeMask,
        },
        Alignment: a.Alignment,
        Flags: heap_flags(a.Flags, PrimaryTranslation::Dropped),
    };

    let mut heap: Option<ID3D12Heap> = None;
    // SAFETY: `desc` is a live local of exactly the struct `CreateHeap` names,
    // and `heap` is a live local the engine writes an owned reference into.
    let created = unsafe { device10.CreateHeap(&desc, &mut heap) };
    // ⚠ The engine's own HRESULT is returned rather than a substituted one: it
    // reaches the application through `ID3D12Device::CreateHeap`, and vkd3d's
    // answers (`E_OUTOFMEMORY`, `E_INVALIDARG`) are more informative than
    // anything this driver could invent. ⛔ It is never rewritten to
    // `DXGI_ERROR_DRIVER_INTERNAL_ERROR`, which `DECISIONS.md` §7.5 reserves for
    // a genuine driver fault.
    let hr = match created {
        Ok(()) => S_OK,
        Err(err) => err.code().0,
    };
    let Some(heap) = heap.filter(|_| hr >= 0) else {
        note_refusal(&L4_REFUSALS.heap_create_engine_failed);
        log_error!(
            "L4: CreateHeap FAILED hr={:#010x} size={} align={} pool={} cpu={} flags={:#x}",
            hr as u32,
            a.ByteSize,
            a.Alignment,
            a.MemoryPool,
            a.CPUPageProperty,
            a.Flags,
        );
        return if hr < 0 { hr } else { E_FAIL };
    };

    let span_buffer = create_heap_span_buffer(device10, &heap, a.ByteSize, a.Flags);

    // The whole-heap object, when the runtime gave a block for it. It is the
    // placement parent every later `ReuseBufferGPUVA` will name.
    if let Some(resource_slot) = resource_slot {
        let desc1 = D3D12_RESOURCE_DESC1 {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: a.Alignment,
            Width: a.ByteSize,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT(0),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
            SamplerFeedbackMipRegion: D3D12_MIP_REGION {
                Width: 0,
                Height: 0,
                Depth: 0,
            },
        };
        // SAFETY: `resource_slot` is this driver's own private block for the
        // object being created, cleared by the caller and written exactly once.
        unsafe {
            resource_slot.store(ResourceState {
                resource: span_buffer.clone(),
                span: Some(HeapSpan {
                    heap: heap.clone(),
                    base_offset: 0,
                }),
                desc: desc1,
                alloc_info: whole_heap_allocation_info(a.ByteSize, a.Alignment),
                // The heap-only arm: `pCreateHeap` was non-null, so the sizing
                // asked for a heap block and it is this driver's to reclaim.
                owns_heap_block: true,
                identity: None,
            });
        }
    }

    // SAFETY: as above, for the heap's own block.
    unsafe {
        heap_slot.store(HeapState {
            heap: Some(heap),
            map_anchor: span_buffer,
            byte_size: a.ByteSize,
        });
    }
    S_OK
}

/// The allocation info this driver reports for a heap's own whole-heap object.
///
/// It is the heap as the runtime described it, with no additional driver data.
/// The two additional-data sizes are therefore zero, while their alignments
/// are the identity alignment rather than zero; see
/// [`EMPTY_ADDITIONAL_DATA_ALIGNMENT`].
fn whole_heap_allocation_info(
    byte_size: u64,
    alignment: u64,
) -> ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022 {
    ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022 {
        ResourceDataSize: byte_size,
        AdditionalDataHeaderSize: 0,
        AdditionalDataSize: 0,
        ResourceDataAlignment: u32::try_from(alignment).unwrap_or(0),
        AdditionalDataHeaderAlignment: EMPTY_ADDITIONAL_DATA_ALIGNMENT,
        AdditionalDataAlignment: EMPTY_ADDITIONAL_DATA_ALIGNMENT,
        Layout: v::TL_ROW_MAJOR,
        MipLevelSwizzleTransition: [0; 5],
        PlaneSliceSwizzleTransition: [0; 2],
    }
}

/// Translate a `D3D12DDI_RESOURCE_FLAGS_0003` word into
/// [`helios_protocol::HeliosWddmAllocationDescV2::bind_flags`]'s vocabulary.
///
/// # ⛔⛔ Why this is not a cosmetic translation, and why the target vocabulary
/// CHANGED
///
/// ⚠ **This function used to target the D3D11 DDI bind word**, because the retired
/// `HeliosWddmAllocMeta::bind_flags`'s reader was the D3D11 driver's
/// `pfnOpenResource` and that field carried raw `D3D10DDI_BIND_*`. HWA2 offset 72 is
/// a **third, deliberately non-coincident** vocabulary — the `HELIOS_HWA2_BIND_*`
/// constants in `protocol/src/wddm.rs` (grep the symbol; that file churns and a line
/// range here goes stale). Their own comment states the design: *"The bit assignment
/// below is therefore chosen to be NON-COINCIDENT with both D3D11 and D3D12:
/// `SHADER_RESOURCE` is 0x1 here and 0x8 at the D3D11 DDI, `RENDER_TARGET` is 0x2 here
/// and 0x1 in the D3D12 DDI."* So targeting the D3D11 DDI word here would now be a
/// *wrong-vocabulary* bug of exactly the class the old comment described, one enum
/// further along.
///
/// The failure mode the old comment recorded is worth keeping, because it is the
/// reason this is a `match` and never a cast: a swapchain back buffer arrives with
/// `RENDER_TARGET | SHADER_RESOURCE` = `0x1 | 0x10` = `0x11`, and `0x11` read in the
/// D3D11 DDI vocabulary is **`VERTEX_BUFFER | STREAM_OUTPUT`** — an image with no
/// render-target and no shader-resource usage, which DWM could not sample. The
/// overlap is structural rather than unlucky: every one of these three enums numbers
/// the same concepts differently.
///
/// # The mapping, and what has no counterpart
///
/// | D3D12 DDI | HWA2 bind | note |
/// |---|---|---|
/// | `RENDER_TARGET` `0x1` | `HELIOS_HWA2_BIND_RENDER_TARGET` `1 << 1` | |
/// | `DEPTH_STENCIL` `0x2` | `HELIOS_HWA2_BIND_DEPTH_STENCIL` `1 << 2` | |
/// | `SHADER_RESOURCE` `0x10` | `HELIOS_HWA2_BIND_SHADER_RESOURCE` `1 << 0` | ⚠ the positive form; the *API* spells it `DENY_SHADER_RESOURCE` |
/// | `UNORDERED_ACCESS` `0x80` | `HELIOS_HWA2_BIND_UNORDERED_ACCESS` `1 << 3` | |
///
/// ⚠ **`CROSS_ADAPTER`, `SIMULTANEOUS_ACCESS`, the two video-reference bits,
/// `CONTENT_PROTECTION`, the three placement/alignment bits, `SAMPLER_FEEDBACK` and
/// `RAYTRACING_ACCELERATION_STRUCTURE` are dropped and that is correct, not lossy:**
/// none of them is a bind. `CROSS_ADAPTER` is not lost either — it becomes
/// `HELIOS_HWA2_FLAG_CROSS_ADAPTER` in [`hwa2_create_input`], which is where §10.3
/// puts it. `SIMULTANEOUS_ACCESS` in particular is a *sharing* property and is common
/// on a back buffer, so counting it as a dropped bit would make the counter fire on
/// every healthy frame. They are named here instead.
///
/// ⚠ **The eight HWA2 bind bits this D3D12 DDI cannot produce** —
/// `VERTEX_BUFFER`, `INDEX_BUFFER`, `CONSTANT_BUFFER`, `STREAM_OUTPUT`, `PRESENT`,
/// `VIDEO_DECODER`, `VIDEO_ENCODER` — are absent because D3D12 has no per-resource
/// bind declaration for them: a buffer is bound by view at draw time. Writing one
/// would be an inference §10.3 forbids, so the record says "not declared" rather
/// than guessing.
///
/// ⛔ **A bit outside the known set is counted**, because the only way it can appear
/// is a header revision adding an enumerator this table has not been told about —
/// and the failure mode of guessing would be a bind flag nobody asked for.
fn hwa2_bind_flags(flags: ddi12::D3D12DDI_RESOURCE_FLAGS_0003) -> u32 {
    let mut out = 0u32;
    if flags & v::RES_RENDER_TARGET != 0 {
        out |= helios_protocol::HELIOS_HWA2_BIND_RENDER_TARGET;
    }
    if flags & v::RES_DEPTH_STENCIL != 0 {
        out |= helios_protocol::HELIOS_HWA2_BIND_DEPTH_STENCIL;
    }
    if flags & v::RES_SHADER_RESOURCE != 0 {
        out |= helios_protocol::HELIOS_HWA2_BIND_SHADER_RESOURCE;
    }
    if flags & v::RES_UNORDERED_ACCESS != 0 {
        out |= helios_protocol::HELIOS_HWA2_BIND_UNORDERED_ACCESS;
    }
    // Every enumerator the header defines, mapped or deliberately not. The mask is
    // built from the aliases rather than written as a literal so that a header
    // revision adding a bit makes the counter fire instead of silently widening it.
    let known = v::RES_RENDER_TARGET
        | v::RES_DEPTH_STENCIL
        | v::RES_CROSS_ADAPTER
        | v::RES_SIMULTANEOUS_ACCESS
        | v::RES_SHADER_RESOURCE
        | v::RES_VIDEO_DECODE_REFERENCE_ONLY
        | v::RES_UNORDERED_ACCESS
        | v::RES_VIDEO_ENCODE_REFERENCE_ONLY
        | v::RES_RAYTRACING_ACCELERATION_STRUCTURE
        | v::RES_CONTENT_PROTECTION
        | v::RES_ONLY_NON_RT_DS_TEXTURE_PLACEMENT
        | v::RES_ONLY_RT_DS_TEXTURE_PLACEMENT
        | v::RES_4MB_ALIGNED
        | v::RES_SAMPLER_FEEDBACK;
    if flags & !known != 0 {
        L4_REFUSALS.meta_bind_flag_unknown.bump();
        let n = L4_REFUSALS.meta_bind_flag_unknown.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: D3D12DDI_RESOURCE_FLAGS {:#x} carries bits {:#x} this build's bind-flag \
                 translation does not know -- the allocation's HWA2 bind_flags is {:#x} and may \
                 be missing a usage an opener needs (x{n})",
                flags,
                flags & !known,
                out,
            );
        }
    }
    out
}

/// The HWA2 swizzle class (offset 88) for one create's `D3D12DDI_TEXTURE_LAYOUT`.
///
/// `None` means the layout has **no** HWA2 spelling and the create must be refused:
/// §10.3's enum is `LINEAR` or `OPAQUE_OPTIMAL` and nothing else, and Direct Flip
/// requires *equality* between two descriptors' classes — so a third layout mapped
/// onto either of those two would make two surfaces compare equal that are not.
///
/// | `D3D12DDI_TEXTURE_LAYOUT` | HWA2 swizzle class | why |
/// |---|---|---|
/// | `TL_ROW_MAJOR` | `LINEAR` | the definition of both |
/// | `TL_UNDEFINED` | `OPAQUE_OPTIMAL` | "driver chooses"; on Helios the engine picks a Vulkan `OPTIMAL` tiling whose layout no guest code may assume |
/// | `TL_64KB_TILE_UNDEFINED_SWIZZLE` | `OPAQUE_OPTIMAL` | same: an opaque, driver-defined swizzle |
/// | `TL_64KB_TILE_STANDARD_SWIZZLE` | ⛔ `None` | a *documented, portable* swizzle. Calling it `OPAQUE_OPTIMAL` would tell an opener the bytes are undefined when the API guarantees they are not, and calling it `LINEAR` is simply false |
///
/// ⚠ A buffer has no texture layout; [`hwa2_create_input`] answers `LINEAR` for it
/// directly rather than routing through here, because `D3D12DDIARG_CREATERESOURCE`'s
/// `Layout` field is not meaningful on that arm (CLAUDE.md's per-arm validation rule).
fn hwa2_swizzle_class(layout: ddi12::D3D12DDI_TEXTURE_LAYOUT) -> Option<u32> {
    if layout == v::TL_ROW_MAJOR {
        Some(helios_protocol::HELIOS_HWA2_SWIZZLE_LINEAR)
    } else if layout == v::TL_UNDEFINED || layout == v::TL_64KB_TILE_UNDEFINED_SWIZZLE {
        Some(helios_protocol::HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL)
    } else {
        None
    }
}

/// The `D3DDDIFORMAT` (HWA2 offset 52) for one `DXGI_FORMAT`.
///
/// ⛔ **Exactly one format is translated, and every other one answers 0.** The
/// D3DDDIFORMAT↔DXGI translation is lossy in the direction that historically cost
/// this project a rebuilt A8 mask read as 4 bpp BGRA (`protocol/src/wddm.rs`'s note
/// on `dxgi_format`), so a general table here would re-introduce exactly that. HWA2
/// carries `dxgi_format` at offset 48 *verbatim* precisely so no opener has to
/// consult this field; it is populated only for the one format the display path
/// itself speaks, and left 0 — "this creator states no D3DDDIFORMAT" — otherwise.
///
/// ⚠ The shared validator does not require a nonzero value for an image kind (it
/// requires **zero** for a non-image kind), so 0 is a legal answer rather than a gap.
fn hwa2_d3d_ddi_format(dxgi_format: u32) -> u32 {
    if dxgi_format == helios_protocol::DXGI_FORMAT_B8G8R8A8_UNORM {
        helios_protocol::D3DDDIFMT_A8R8G8B8
    } else {
        0
    }
}

/// Build the **HWA2 create-input descriptor** for one committed D3D12 resource.
///
/// `docs/retirement/K4-CONTRACT.md` §1.1 is normative for the field partition and
/// this function implements the producer half of it, exactly:
///
/// | field | who writes it | what this function does |
/// |---|---|---|
/// | `magic`, `abi_version`, `struct_size`, `package_generation` | UMD | written exactly, via [`helios_protocol::HeliosWddmAllocationDescV2::header`] |
/// | `allocation_generation` | **KMD only** | written **0**; a nonzero value on input is a hard reject |
/// | `flags & DIRECT_FLIP_COMPATIBLE` | **KMD only** | written **0** — §10.3: KMD sets it only for a non-protected, non-cross-adapter managed primary in a class the display backend implements, and *an opener never infers it* |
/// | `flags & D3D12_RUNTIME_PRIMARY` | **KMD only** | written **0** — §10.3 C44: KMD sets it only when the create record, the runtime `PRIMARY` flag and the sentinel all agree |
/// | everything else | UMD, echoed by KMD | written exactly; the KMD **refuses the create rather than correcting a field** |
///
/// ⇒ every value below is a statement this driver is prepared to have the create
/// fail on. There is no "close enough" field and no field left for the kernel to
/// fill in except the two it owns.
///
/// # ⭐ C44, and it is the one place D3D12 differs from D3D11 in this record
///
/// `vidpn_source` (offset 80) is [`helios_protocol::D3DDDI_ID_UNINITIALIZED`], and
/// that is required rather than convenient. A D3D12 Resource-Heaps primary has its
/// `VidPnSourceId` overwritten with that sentinel by the runtime, so the KMD's C44
/// cross-validation *demands* it there; and for a non-primary the shared validator
/// demands it too (`NonPrimaryVidPnSourceNotSentinel`). Writing a concrete source
/// would therefore be rejected on both branches. ⛔ The matching
/// `D3D12_RUNTIME_PRIMARY` bit is **not** set here even so: the bit and the sentinel
/// are cross-validated by the kernel and neither may be inferred by a producer.
///
/// # ⛔ What is deliberately NOT in this record
///
/// **No host resource id, and no Vulkan memory type index** (§10.3, K4-CONTRACT §5).
/// The retired `HeliosWddmAllocPrivate::adopt_resource_id` carried the first and the
/// retired `HeliosWddmAllocMeta::memory_type_index` the second; HWA2 has an
/// equivalent for neither and neither may be re-added under another name. The engine
/// still *knows* both — [`crate::bridge12::BridgeDevice12::resource_venus_identity`]
/// answers them — and this driver deliberately drops them on the floor, counting
/// `Hwa2VenusResIdDropped` so the drop is a number rather than a silence. The
/// mechanism that replaces them is **mesa lane unit A3**: the ICD stops naming host
/// resources at all and the KMD patches the resid in from `HeliosNativeRenderPatch`.
///
/// Returns `None` when some field of this create has no HWA2 spelling; the caller
/// counts `Hwa2GeometryUnrepresentable` and fails the create. ⛔ Never a partial
/// record: §10.3's "any malformed … descriptor makes create/open fail; it never
/// selects a legacy parser" cuts both ways, and a producer that shipped a descriptor
/// it knew was wrong would be asking the kernel to catch its own bug.
fn hwa2_create_input(
    heap_arg: &ddi12::D3D12DDIARG_CREATEHEAP_0001,
    res_arg: &ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    byte_size: u64,
    row_pitch: u32,
) -> Option<helios_protocol::HeliosWddmAllocationDescV2> {
    // ⛔ `allocation_generation` = 0. K4-CONTRACT §1.1: the KMD assigns it, and a
    // nonzero value on the input side is `AllocationGenerationNonZeroOnInput`.
    let mut desc = helios_protocol::HeliosWddmAllocationDescV2::header(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        0,
    );

    // ── kind ────────────────────────────────────────────────────────────────
    //
    // ⚠ No `STANDARD_*` kind is reachable from here and that is a statement, not an
    // omission: those three are `D3DKMDT_STANDARDALLOCATION` shapes dxgkrnl asks the
    // *kernel* to describe, and this is an application's committed resource. The
    // `STANDARD` flag and `standard_allocation_type` therefore stay 0 together, which
    // the shared validator cross-checks in both directions.
    let is_buffer = res_arg.ResourceType == v::RT_BUFFER;
    desc.allocation_kind = if is_buffer {
        helios_protocol::HELIOS_HWA2_KIND_BUFFER
    } else {
        helios_protocol::HELIOS_HWA2_KIND_IMAGE
    };

    // ── geometry, per kind class ────────────────────────────────────────────
    //
    // ⛔ The validator reads "zero only for a non-image kind" fail-closed in BOTH
    // directions, so a buffer must carry zero in every geometry field and
    // `DXGI_FORMAT_UNKNOWN`, and an image must carry a nonzero value in every one of
    // them. That is why this is two arms and not one arm with `min(1)` clamps: a
    // clamp would manufacture a geometry the runtime never stated.
    if is_buffer {
        // Every geometry field stays zero, as `header` left it, including
        // `dxgi_format = DXGI_FORMAT_UNKNOWN` and `d3d_ddi_format = 0`.
        //
        // ⛔ **The guard is `> 1`, not `!= 0`, and that distinction is the whole of
        // it.** D3D12 requires a buffer's description to carry the *canonical*
        // `Height = 1`, `DepthOrArraySize = 1`, `MipLevels = 1`, `SampleDesc = {1, 0}`
        // and `Format = UNKNOWN` — they are constants of the shape, not geometry — so
        // a `!= 0` test would refuse every buffer this driver has ever created. What
        // it must refuse is a buffer carrying something that is genuinely geometry,
        // which HWA2's non-image arm cannot describe and which zeroing would silently
        // drop.
        if res_arg.Format as u32 != helios_protocol::DXGI_FORMAT_UNKNOWN
            || res_arg.Height > 1
            || res_arg.DepthOrArraySize > 1
            || res_arg.MipLevels > 1
            || res_arg.SampleDesc.Count > 1
            || res_arg.SampleDesc.Quality != 0
        {
            return None;
        }
    } else {
        // ⚠ `Width` is a `UINT64` at the DDI even for a texture and HWA2's `width` is
        // 32-bit, so the narrowing is checked rather than truncated: a texture wider
        // than `u32::MAX` cannot be described by this record at all, and a truncated
        // width would describe a *different* surface.
        desc.width = u32::try_from(res_arg.Width).ok()?;
        desc.height = res_arg.Height;
        desc.depth_or_array_size = u32::from(res_arg.DepthOrArraySize);
        desc.mip_levels = u32::from(res_arg.MipLevels);
        desc.sample_count = res_arg.SampleDesc.Count;
        desc.sample_quality = res_arg.SampleDesc.Quality;
        desc.dxgi_format = res_arg.Format as u32;
        desc.d3d_ddi_format = hwa2_d3d_ddi_format(desc.dxgi_format);
        if desc.width == 0
            || desc.height == 0
            || desc.depth_or_array_size == 0
            || desc.mip_levels == 0
            || desc.sample_count == 0
            || desc.dxgi_format == helios_protocol::DXGI_FORMAT_UNKNOWN
        {
            return None;
        }
    }

    // ── the backing extent, and the plane records bounded against it ────────
    if byte_size == 0 {
        return None;
    }
    desc.byte_size = byte_size;
    if !is_buffer {
        // ⛔ **One plane record, and a 0 row pitch is now a REFUSAL rather than a
        // carried unknown.** The retired trailer tolerated a 0 pitch because nothing
        // on the windowed path read it; HWA2's validator does not
        // (`PlaneRowPitchZero`), so a descriptor with one would fail the create in the
        // kernel with a rejection this driver could have named itself. The pitch is
        // still the ENGINE's own `GetCopyableFootprints` answer and is never
        // re-derived here — the 15th/39th sessions are what a second derivation of a
        // stride costs.
        //
        // ⚠ **Bound: exactly one plane is described.** A planar DXGI format (NV12 and
        // friends) has two, and this driver has no format table that says so. It has
        // also never seen one on this DDI. Recorded as a bound rather than guessed at:
        // `plane_count` is a statement about the record, and stating 2 without knowing
        // the chroma offset would be worse than stating 1.
        //
        // ⛔ **`slice_pitch` is the WHOLE extent, and it is not
        // `GetCopyableFootprints`'s total.** The record's own doc defines it as "bytes
        // covered by this plane, i.e. the extent bounded against `byte_size`", and on
        // this arm one image occupies a dedicated `VkDeviceMemory` at offset 0 — the
        // caller has already refused a non-zero `memory_offset` — so the plane covers
        // all of it. ⚠ The engine's linear `pTotalBytes` was the other candidate and is
        // deliberately NOT used: for an `OPAQUE_OPTIMAL` image it describes a linear
        // footprint the allocation is not laid out in, and it can legitimately exceed
        // the tiled allocation's size, which would turn a healthy create into a refusal.
        let slice_pitch = u32::try_from(byte_size).ok()?;
        if row_pitch == 0 || row_pitch > slice_pitch {
            return None;
        }
        desc.plane_count = 1;
        desc.planes[0] = helios_protocol::HeliosWddmPlaneRecordV2 {
            // The engine dedicates the whole `VkDeviceMemory` to this resource and
            // binds it at offset 0 — asserted by the caller's `memory_offset == 0`
            // refusal, not assumed here.
            offset: 0,
            row_pitch,
            slice_pitch,
        };
    }

    // ── flags ───────────────────────────────────────────────────────────────
    //
    // ⛔ `DIRECT_FLIP_COMPATIBLE` and `D3D12_RUNTIME_PRIMARY` are KMD-owned and stay
    // 0; see this function's table. The rest are statements this driver can actually
    // make from the create it is inside:
    //
    // * `PRIMARY` / `DISPLAYABLE` — **not set.** The deployed runtime declares neither
    //   `D3D12DDI_HEAP_FLAG_PRIMARY` nor `RESOURCE_OPTIMIZATION_FLAG_PRIMARY` for
    //   flip-model back buffers, and the windowed DWM-composited present this lane
    //   targets never reaches `DxgkDdiPresent` at all. Claiming a primary would aim
    //   dxgkrnl's primary bookkeeping at VidPn source 0, the live desktop's.
    // * `SHARED` — **not set**, and this is the inference §10.3 most explicitly
    //   forbids. The negotiated D3D12 DDI carries no shared-resource declaration on
    //   the create; every fused resource is made venus-exportable so that it *could*
    //   be shared, which is a property of this driver's implementation and not a
    //   statement the runtime made about the resource.
    // * `STEREO` / `PROTECTED` — not set. This driver reports no protected-resource
    //   support and refuses stereo shapes upstream.
    // * `RESOURCE_ASSOCIATED` — **set**, because this create passes `hResource` to
    //   `pfnAllocateCb` below, which is precisely what makes dxgkrnl create a kernel
    //   resource object for the allocation. It is knowable at the write site, which is
    //   the only reason it is written.
    let mut flags = helios_protocol::HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED;
    if res_arg.Flags & v::RES_CROSS_ADAPTER != 0 {
        flags |= helios_protocol::HELIOS_HWA2_FLAG_CROSS_ADAPTER;
    }

    // ── memory class, and the CPU-visibility bit it is cross-checked against ─
    //
    // ⛔ The validator refuses `CPU_VISIBLE` class without the flag and `DEVICE_LOCAL`
    // class with it, so these two are decided together in one place. The source is the
    // *heap's* declared CPU page property, which on the fused arm is the heap this
    // very resource is placed in at offset 0.
    //
    // ⚠ `HELIOS_HWA2_MEMORY_SHARED` is never chosen: it means "backing shared with
    // another agent", and nothing on this arm says that.
    //
    // ⛔ An unrecognised page property coerces to NOT_AVAILABLE **exactly as
    // [`cpu_page_property`] coerces it** for the heap description handed to the
    // engine, so the record and the heap cannot disagree about CPU visibility. It is
    // spelled out here rather than routed through that function because that function
    // bumps `HeapPropertyUnrepresentable`, and one create must not count twice.
    let cpu_visible = cpu_page_property_is_visible(heap_arg.CPUPageProperty);
    if cpu_visible {
        flags |= helios_protocol::HELIOS_HWA2_FLAG_CPU_VISIBLE;
        desc.memory_class = helios_protocol::HELIOS_HWA2_MEMORY_CPU_VISIBLE;
    } else {
        desc.memory_class = helios_protocol::HELIOS_HWA2_MEMORY_DEVICE_LOCAL;
    }
    desc.flags = flags;

    desc.bind_flags = hwa2_bind_flags(res_arg.Flags);
    // ⚠ 0, and every one of the four `HELIOS_HWA2_MISC_*` bits is a statement this DDI
    // does not make. `GDI_COMPATIBLE` and `SHARED_NT_HANDLE` are D3D11 create
    // descriptions with no D3D12 DDI counterpart; `TEXTURE_CUBE` is a *view*
    // interpretation of an array in D3D12, not a resource property; `RESOURCE_CLAMP`
    // is a D3D11 mip-clamp facility. Writing any of them would be an invention.
    desc.misc_flags = 0;
    // ⭐ C44 — see this function's doc. The sentinel, on both the primary and the
    // non-primary branch, and never a concrete source.
    desc.vidpn_source = helios_protocol::D3DDDI_ID_UNINITIALIZED;
    desc.standard_allocation_type = 0;
    desc.swizzle_class = if is_buffer {
        // A buffer has no texture layout and its bytes are linear by definition; the
        // DDI's `Layout` field is not meaningful on this arm.
        helios_protocol::HELIOS_HWA2_SWIZZLE_LINEAR
    } else {
        hwa2_swizzle_class(res_arg.Layout)?
    };
    Some(desc)
}

/// Give a committed resource a kernel allocation described by an HWA2 create-input
/// descriptor, and record its identity.
///
/// `KMD_IMPACT.md` §14a.3 UP-5, retired onto the HPS2 successor record by K4.
///
/// ⚠ **This block used to describe ADOPTION and the reversal is recorded rather than
/// quietly edited.** It said the engine allocated the Vulkan memory and this driver
/// *"**adopts** it by calling `pfnAllocateCb` with
/// `HeliosWddmAllocPrivate.adopt_resource_id = <venus resid>`"*, which the KMD
/// classified as `AllocationBacking::AdoptedUmdResource` and then restamped as a
/// `HeliosWddmOpenIdentity` so DWM's D3D11 opener worked unchanged. **Every clause of
/// that is retired.** §10.3 forbids a UMD naming a host resource id at all
/// (K4-CONTRACT §5), so the record this create sends carries none, the KMD creates
/// the backing rather than adopting one, and the open-time restamp is deleted —
/// an opener reads the same immutable descriptor the creator sent.
///
/// ⛔ **What that costs until mesa unit A3 lands, stated rather than papered over.**
/// The kernel allocation this function mints is not yet the same host object as the
/// `VkDeviceMemory` vkd3d renders into; joining them is A3's job (the ICD stops
/// naming host resources and the KMD patches the resid in from
/// `HeliosNativeRenderPatch`). Until then this driver owns a valid WDDM allocation
/// for every committed resource and **cannot present or export its contents** —
/// `present12` refuses with `PresentIdentityNoResourceId` naming A3, which is the
/// retirement's intended intermediate state and not a regression. Nothing here
/// fabricates the missing link, and `Hwa2VenusResIdDropped` counts every create that
/// had a host resid available and did not send it.
///
/// # ⛔ Every failure fails the CREATE, and that is the contract rather than a
/// severity choice
///
/// The negotiated DDI provides no shared-resource bit on this create and the
/// deployed runtime does not call `pfnCheckResourceAllocationHandle` before
/// `CreateSharedHandle`. The committed arm is therefore the last contract-defined
/// point at which the driver can associate `hRTResource` with a WDDM allocation;
/// unlike bind flags, formats or geometry, it is not a heuristic admission test.
/// Returning `S_OK` with the allocation missing would hand the runtime an object
/// that can be created but not shared — the survivable lie CLAUDE.md rule 2 and
/// `METHOD.md` §2 Phase 4 both forbid — and the failure would surface much later as
/// `E_INVALIDARG` or a black window with no counter naming its cause. ⛔ It is
/// deliberately **not**
/// knob-gated: `METHOD.md` §2 Phase 4 consequence 1 records that a default chosen to
/// keep a run alive rather than to be correct is *"a hack wearing a knob's
/// clothes"*, with this driver's own `Umd12EclSubmitStrict` as the example.
///
/// Every distinguishable cause has its own counter, because they are different
/// findings with different fixes:
///
/// | counter | what it means | where the fix is |
/// |---|---|---|
/// | `IdentityVkMemoryUnresolved` | the engine could not name the memory a resource is bound to, so its **size** is unknown and HWA2's `byte_size` cannot be stated | the vkd3d fork / the interop interface |
/// | `IdentityOffsetNonZero` | the engine suballocated the resource, so the plane records cannot be bounded against a dedicated extent | the fork's dedicated-allocation arm |
/// | `Hwa2GeometryUnrepresentable` | some field of this create has no HWA2 spelling (a standard-swizzle layout, a 0 row pitch, a width above `u32::MAX`) | this file's translation, or the record |
/// | `Hwa2CreateInputInvalid` | the descriptor this driver built failed `validate_create_input` — **a bug in this file**, caught before the kernel sees it | this file |
/// | `Hwa2WriteBackAbsent` | `pfnAllocateCb` succeeded and the private buffer came back with `allocation_generation == 0`, i.e. the kernel's create-time write did not reach this buffer | dxgkrnl's propagation, or the KMD's write site |
/// | `Hwa2CreateOutputInvalid` | the kernel wrote a descriptor that failed `validate_create_output` | the KMD |
/// | `AllocateCbMissing` / `AllocateCbFailed` / `AllocateCbNoHandle` | dxgkrnl refused | the record, the flags, or the kernel |
/// | `IdentityRegistryAllocFailed` | process memory could not grow the identity registry | process memory pressure |
///
/// ⚠ `IdentityVenusUnresolved` is **no longer in that table**: it is now a census,
/// not a refusal. This create needs the *engine* half of the identity (the bound
/// memory's size) and needs nothing at all from the ICD, so an unexportable memory no
/// longer fails a create — it fails a *present*, later, in `present12`.
///
/// # ⚠ What this does NOT set, and why each omission is a decision
///
/// * **`D3D12DDI_ALLOCATION_INFO_FLAGS_0022_PRIMARY` is NOT set**, though
///   `KMD_IMPACT.md` §14a.3 UP-5 prescribes `Flags = PRIMARY`. That flag reaches
///   dxgkrnl as a VidPn-primary claim, and the HWA2 record this create sends declares
///   no `PRIMARY` either — the two must agree, and both say the same thing. The target
///   here is the **windowed** DWM-composited present, which never reaches
///   `DxgkDdiPresent` at all (measured: `PRESENT_FLAGS_HISTOGRAM` has only ever seen
///   `0x1` and `0xC`, unsampled and non-overflowing). It is the first field a
///   fullscreen-flip lane must revisit, together with `HELIOS_HWA2_FLAG_DISPLAYABLE`.
/// * **`D3D12DDI_ALLOCATION_INFO_0022::VidPnSourceId` is the C44 SENTINEL, not 0**,
///   and that changed with this record. It used to be 0 with the note *"only
///   meaningful with the PRIMARY flag this driver does not set"*. HWA2 offset 80 must
///   carry `D3DDDI_ID_UNINITIALIZED` for a non-primary **and** for a D3D12 runtime
///   primary, and §10.3 C44 has the KMD cross-validate the record against what
///   dxgkrnl was told — so leaving a 0 here would make the outer create disagree with
///   the descriptor inside it.
///
/// # Safety
/// `dev` must be the live device this create is running on, `resource` the engine
/// resource it just created, and `h_rt_resource` the runtime handle
/// `pfnCreateHeapAndResource` was called with — which is live for the duration of
/// the call, because this runs **inside** that DDI.
fn finish_cpu_backing_rollback(
    cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
    deallocated: bool,
) {
    if !deallocated {
        if let Some(backing) = cpu_backing {
            backing.leak();
        }
    }
}

unsafe fn create_committed_allocation(
    dev: &HeliosD3D12Device,
    device10: &ID3D12Device10,
    resource: &ID3D12Resource,
    heap_arg: &ddi12::D3D12DDIARG_CREATEHEAP_0001,
    res_arg: &ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    desc: &D3D12_RESOURCE_DESC1,
    h_rt_resource: ddi12::D3D12DDI_HRTRESOURCE,
    association: &helios_protocol::HeliosResourceAssociationV1,
    mut cpu_backing: Option<helios_umd_common::cpu_backing::CpuBacking>,
) -> Result<identity12::AllocationIdentity, Hresult> {
    let engine_resource = resource.as_raw() as usize;
    if association
        .validate(
            helios_protocol::HELIOS_PACKAGE_GENERATION,
            dev.translator.session_generation(),
        )
        .is_err()
        || association.outer_allocation_bytes != heap_arg.ByteSize
        || cpu_backing.as_ref().map(|backing| backing.as_ptr())
            != (!association.cpu_mapping.is_null()).then_some(association.cpu_mapping)
        || cpu_backing
            .as_ref()
            .is_some_and(|backing| u64::try_from(backing.bytes()).ok() != Some(heap_arg.ByteSize))
    {
        note_refusal(&L4_REFUSALS.identity_registry_alloc_failed);
        return Err(E_INVALIDARG);
    }

    // The immutable HRA1 was consumed by this exact vkd3d heap allocation.
    // Its byte extent is therefore the only size fact this outer WDDM
    // allocation needs.  No VkDeviceMemory handle, host resource id, memory
    // type, or ICD export is queried or retained.
    let allocation_bytes = association.outer_allocation_bytes;
    // ── 2. the row pitch and the subresource extent, from the ENGINE ───────
    //
    // ⛔ Asked, never computed. HWA2's plane record is what an opener lays rows out
    // with, and this driver has two ways to produce those numbers: ask the engine that
    // owns the layout, or re-derive `align(width * bpp, 256)` here. The second is a
    // *second* derivation of a number the engine already owns, and the 15th/39th
    // sessions are what a disagreeing stride costs (a sheared or black surface).
    //
    // ⚠ **A 0 pitch is now a REFUSAL, and that is a change.** The retired trailer
    // tolerated one because nothing on the windowed path read it; HWA2's shared
    // validator rejects `PlaneRowPitchZero` outright, so a descriptor carrying one
    // would fail in the kernel with a rejection this driver could have named itself.
    //
    let api_desc = resource_desc(desc);
    let mut layout = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
        Offset: 0,
        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
            Format: DXGI_FORMAT(0),
            Width: 0,
            Height: 0,
            Depth: 0,
            RowPitch: FOOTPRINT_UNANSWERED_U32,
        },
    };
    // SAFETY: `api_desc` and `layout` are live locals; `NumSubresources` is 1, so the
    // one array this call writes has exactly one element, and the three outputs this
    // call does not want are declined with `None`.
    unsafe {
        device10.GetCopyableFootprints(
            &api_desc,
            0,
            1,
            0,
            Some(core::ptr::from_mut(&mut layout)),
            None,
            None,
            None,
        );
    }
    let pitch = if layout.Footprint.RowPitch == FOOTPRINT_UNANSWERED_U32 {
        0
    } else {
        layout.Footprint.RowPitch
    };

    // ── 3. the HWA2 create-input descriptor ────────────────────────────────
    let Some(create_input) = hwa2_create_input(heap_arg, res_arg, allocation_bytes, pitch) else {
        note_refusal(&L4_REFUSALS.hwa2_geometry_unrepresentable);
        log_error!(
            "L4: COMMITTED create REFUSED -- this create has no HWA2 spelling: type={} {}x{}x{} \
             mips={} samples={}/{} fmt={} layout={} subresource0RowPitch={} byteSize={}. A \
             standard-swizzle layout, a declined row pitch, a dimension above u32::MAX and a \
             non-canonical buffer description are the four causes; none may be approximated, \
             because the kernel echoes every field it validates",
            res_arg.ResourceType,
            res_arg.Width,
            res_arg.Height,
            res_arg.DepthOrArraySize,
            res_arg.MipLevels,
            res_arg.SampleDesc.Count,
            res_arg.SampleDesc.Quality,
            res_arg.Format,
            res_arg.Layout,
            pitch,
            allocation_bytes,
        );
        return Err(E_FAIL);
    };

    // ⛔ **Validated BEFORE the kernel sees it, and a failure here is a bug in THIS
    // file.** K4-CONTRACT §1.1 makes the create-input stage a real validator with the
    // same cross-field core as the output stage, so every rule the kernel will apply
    // is applied here first. Sending a descriptor this driver could have known was
    // malformed would turn a local bug into an `AllocateCbFailed` with a kernel-side
    // rejection nobody can attribute.
    if let Err(reject) =
        create_input.validate_create_input(helios_protocol::HELIOS_PACKAGE_GENERATION)
    {
        note_refusal(&L4_REFUSALS.hwa2_create_input_invalid);
        log_error!(
            "L4: COMMITTED create REFUSED -- the HWA2 create-input descriptor this driver built \
             is INVALID: {:?}. kind={} flags={:#x} bind={:#x} misc={:#x} vidpn={:#x} swizzle={} \
             memory={} planes={} byteSize={} {}x{}x{} mips={} samples={} fmt={}",
            reject,
            create_input.allocation_kind,
            create_input.flags,
            create_input.bind_flags,
            create_input.misc_flags,
            create_input.vidpn_source,
            create_input.swizzle_class,
            create_input.memory_class,
            create_input.plane_count,
            create_input.byte_size,
            create_input.width,
            create_input.height,
            create_input.depth_or_array_size,
            create_input.mip_levels,
            create_input.sample_count,
            create_input.dxgi_format,
        );
        return Err(E_FAIL);
    }
    if (create_input.flags & helios_protocol::HELIOS_HWA2_FLAG_CPU_VISIBLE != 0)
        != cpu_backing.is_some()
    {
        note_refusal(&L4_REFUSALS.identity_registry_alloc_failed);
        return Err(E_INVALIDARG);
    }

    // ⛔ **A byte buffer, not a struct pointer handed to the kernel.** The buffer is
    // `[in/out]`: dxgkrnl passes it down and the KMD writes all 168 bytes of the
    // output descriptor back into it. Keeping it as bytes is what lets the read-back
    // below enter through `from_private_data` — the constructor whose own doc says
    // "every consumer must enter through here rather than casting the pointer",
    // because the runtime's buffer carries no alignment promise.
    let mut private_bytes = [0u8; helios_protocol::HELIOS_HWA2_BYTES as usize];
    // SAFETY: `HeliosWddmAllocationDescV2` is `#[repr(C)]` and `bytemuck::Pod`, so it
    // has no padding and no invalid bit pattern, and `protocol`'s own `const _` block
    // pins its size at `HELIOS_HWA2_BYTES`. Source and destination are distinct live
    // locals, so the ranges cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(
            core::ptr::from_ref(&create_input).cast::<u8>(),
            private_bytes.as_mut_ptr(),
            private_bytes.len(),
        );
    }

    // ── 4. pfnAllocateCb ───────────────────────────────────────────────────
    if dev.um_callbacks.is_null() {
        // Expected unreachable: `create_device` refuses a null `p12UMCallbacks`
        // before the device exists.
        note_refusal(&L4_REFUSALS.allocate_cb_missing);
        return Err(E_FAIL);
    }
    // SAFETY: non-null per the check. `um_callbacks` is the runtime's
    // `D3D12DDI_CORELAYER_DEVICECALLBACKS_0062`, stored by `create_device` and never
    // reassigned, for a table the runtime keeps alive at least as long as the device.
    let Some(allocate_cb) = (unsafe { (*dev.um_callbacks).pfnAllocateCb }) else {
        note_refusal(&L4_REFUSALS.allocate_cb_missing);
        log_error!(
            "L4: COMMITTED create REFUSED -- p12UMCallbacks->pfnAllocateCb is absent, so this \
             driver cannot mint a kernel allocation"
        );
        return Err(E_FAIL);
    };

    let private_ptr = private_bytes.as_mut_ptr().cast::<c_void>();
    let private_size = u32::from(helios_protocol::HELIOS_HWA2_BYTES);
    let mut allocation_info = ddi12::D3D12DDI_ALLOCATION_INFO_0022 {
        hAllocation: 0,
        // A CPU-visible heap uses this exact outer allocation's process-local
        // view. The pointer is data access only; HRA1's token remains the sole
        // allocation association key.
        pSystemMem: cpu_backing
            .as_ref()
            .map_or(core::ptr::null(), |backing| backing.as_ptr().cast_const()),
        pPrivateDriverData: private_ptr,
        PrivateDriverDataSize: private_size,
        // ⭐ C44's sentinel, and it must MATCH the descriptor's `vidpn_source` — see
        // this function's doc. Not 0.
        VidPnSourceId: helios_protocol::D3DDDI_ID_UNINITIALIZED,
        Flags: ddi12::D3D12DDI_ALLOCATION_INFO_FLAGS_0022_D3D12DDI_ALLOCATION_INFO_FLAGS_0022_NONE,
        // The KMD assigns the GPU VA; this driver never reserves one
        // (`pfnReserveGpuVirtualAddressCb` is not called anywhere in this crate).
        GpuVirtualAddress: 0,
        Priority: 0,
        // ⛔ ZEROED, and the runtime checks it: `Reserved fields in
        // D3D12DDI_ALLOCATION_INFO_0022 were not zero.` is a literal string in the
        // D3D12 runtime. `[0; 5]` rather than a `..Default::default()` tail so the
        // zeroing is visible at the write site and cannot be lost to a struct-update
        // that a later field addition silently reorders.
        Reserved: [0; 5],
    };
    let mut alloc = ddi12::D3D12DDICB_ALLOCATE_0022 {
        pPrivateDriverData: private_ptr,
        PrivateDriverDataSize: private_size,
        // ⛔⛔ THE ASSOCIATION, and it is NOT optional. D3D11 states it outright for
        // exactly this class of allocation: *"the association itself is not optional
        // for present-only allocations"* (`umd/src/forward/resource.rs`). It is also
        // what makes `HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED` in the descriptor above a
        // true statement rather than a hopeful one: passing `hResource` is precisely
        // what asks dxgkrnl to create the kernel resource object that flag records.
        hResource: h_rt_resource.handle,
        hKMResource: 0,
        NumAllocations: 1,
        pAllocationInfo: core::ptr::from_mut(&mut allocation_info),
    };

    // SAFETY: a non-null callback out of the runtime's own corelayer table, given
    // the device handle it supplied and two fully initialised out-structs that are
    // live locals. `pAllocationInfo` addresses exactly `NumAllocations` = 1 element.
    let hr = unsafe { allocate_cb(dev.h_rt_device, core::ptr::from_mut(&mut alloc)) };
    if hr < 0 {
        // ⛔ `hr < 0`, not `hr != S_OK`: a non-negative non-`S_OK` value is a success
        // code, and treating `S_FALSE` as a failure would fail a create that worked.
        note_refusal(&L4_REFUSALS.allocate_cb_failed);
        log_error!(
            "L4: pfnAllocateCb FAILED hr={:#010x} for the committed resource {:#x} (byteSize {} \
             rt_resource {:p} priv {} bytes)",
            hr as u32,
            engine_resource,
            create_input.byte_size,
            h_rt_resource.handle,
            private_size,
        );
        let deallocated = allocation_info.hAllocation == 0
            || unsafe {
                deallocate_committed(
                    dev,
                    allocation_info.hAllocation,
                    DeallocateForm::ByHandleList,
                )
            };
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
        return Err(hr);
    }
    let h_allocation = allocation_info.hAllocation;
    if h_allocation == 0 {
        // Success with no handle is a runtime contract violation this driver cannot
        // act on, and it is exactly the shape that would otherwise become a 0 in
        // `pfnPresent`'s `BroadcastSrcAllocation[0]`.
        note_refusal(&L4_REFUSALS.allocate_cb_no_handle);
        log_error!(
            "L4: pfnAllocateCb returned hr={:#010x} with hAllocation=0 for the committed resource {:#x}",
            hr as u32,
            engine_resource,
        );
        return Err(E_FAIL);
    }

    // ── 5. the KMD's write-back, validated ─────────────────────────────────
    //
    // ⭐⭐ **This is the output stage of K4-CONTRACT §1.1 and it is not optional.**
    // The buffer is `[in/out]`; the KMD validates the input in full and then writes
    // the complete 168-byte output descriptor, echoing every field it validated and
    // stamping the one field only the kernel can know. So the create is not finished
    // until this driver has read that descriptor back and found it valid: an
    // allocation whose descriptor never arrived is one no opener can ever describe,
    // and carrying on would be the survivable lie CLAUDE.md rule 2 forbids.
    //
    // ⚠ **Three outcomes, three counters, because they have three different fixes.**
    // A buffer that did not change at all says dxgkrnl did not propagate the kernel's
    // create-time write (recon's open question, and the answer is *this* counter);
    // a changed buffer that fails validation says the KMD wrote something wrong; and
    // a valid one is the success path.
    let read_back = helios_protocol::HeliosWddmAllocationDescV2::from_private_data(&private_bytes);
    let allocation_desc = match read_back {
        Ok(desc) if desc.allocation_generation == 0 => {
            // ⛔ The kernel's write did not reach this buffer: `allocation_generation`
            // is the one field the KMD must stamp and the one this driver sent as 0.
            note_refusal(&L4_REFUSALS.hwa2_write_back_absent);
            log_error!(
                "L4: COMMITTED create REFUSED -- pfnAllocateCb succeeded (alloc={:#x}) and the \
                 HWA2 private buffer came back with allocation_generation=0, i.e. the KMD's \
                 create-time write did not reach this UMD's buffer. Nothing downstream can \
                 describe this allocation, so it is rolled back",
                h_allocation,
            );
            // SAFETY: `h_allocation` is the handle `pfnAllocateCb` just minted for this
            // driver, not yet recorded anywhere, so this is its only reference.
            let deallocated =
                unsafe { deallocate_committed(dev, h_allocation, DeallocateForm::ByHandleList) };
            finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
            return Err(E_FAIL);
        }
        Ok(desc) => {
            match desc.validate_create_output(helios_protocol::HELIOS_PACKAGE_GENERATION) {
                Ok(()) => desc,
                Err(reject) => {
                    note_refusal(&L4_REFUSALS.hwa2_create_output_invalid);
                    log_error!(
                        "L4: COMMITTED create REFUSED -- the KMD's HWA2 output descriptor for \
                         alloc={:#x} is INVALID: {:?}. gen={} kind={} flags={:#x} bind={:#x} \
                         misc={:#x} vidpn={:#x} swizzle={} memory={} planes={} byteSize={}",
                        h_allocation,
                        reject,
                        desc.allocation_generation,
                        desc.allocation_kind,
                        desc.flags,
                        desc.bind_flags,
                        desc.misc_flags,
                        desc.vidpn_source,
                        desc.swizzle_class,
                        desc.memory_class,
                        desc.plane_count,
                        desc.byte_size,
                    );
                    // SAFETY: as above -- the only reference to a handle nothing recorded.
                    let deallocated = unsafe {
                        deallocate_committed(dev, h_allocation, DeallocateForm::ByHandleList)
                    };
                    finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
                    return Err(E_FAIL);
                }
            }
        }
        Err(reject) => {
            // Unreachable by construction — the buffer is exactly `HELIOS_HWA2_BYTES`
            // long and `from_private_data`'s only failure arm is a length mismatch —
            // and counted anyway, because "unreachable by construction" is a claim and
            // this is where it would be observed breaking.
            note_refusal(&L4_REFUSALS.hwa2_create_output_invalid);
            log_error!(
                "L4: COMMITTED create REFUSED -- the HWA2 private buffer could not be parsed \
                 back for alloc={:#x}: {:?}",
                h_allocation,
                reject,
            );
            // SAFETY: as above.
            let deallocated =
                unsafe { deallocate_committed(dev, h_allocation, DeallocateForm::ByHandleList) };
            finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
            return Err(E_FAIL);
        }
    };
    // ⚠ Not a refusal — the census that answers recon's open question with a number:
    // the kernel's create-time private write DOES reach this buffer. Its historical
    // name is kept because `D3D12 DDI refusals:` lines are diffed across builds.
    L4_REFUSALS.alloc_private_written_back.bump();

    // ⛔ **Echo check, and it is deliberately narrow.** K4-CONTRACT §1.1: the KMD
    // "refuses the create rather than correcting a field", so every field except
    // `allocation_generation` and the two KMD-owned flag bits must come back exactly
    // as sent. A silent correction would make the descriptor disagree with the
    // resource this driver believes it made — the exact failure the field partition
    // exists to prevent — so it is a finding, not an update.
    //
    // The mask is `protocol`'s, not a local re-derivation of it. This site used
    // to spell out `DIRECT_FLIP_COMPATIBLE | D3D12_RUNTIME_PRIMARY` by hand, and
    // so did three others (`protocol` itself, `kmd_logic`, `umd`): one rule,
    // four declarations, nothing comparing them. Add a third KMD-owned bit and
    // every hand copy keeps clearing two — the echo check then reports a
    // mismatch on a field the KMD legitimately stamped, which is a false finding
    // in the one place whose whole job is to be believed.
    let echoed = helios_protocol::HeliosWddmAllocationDescV2 {
        allocation_generation: 0,
        flags: allocation_desc.flags & !helios_protocol::HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
        ..allocation_desc
    };
    if echoed != create_input {
        note_refusal(&L4_REFUSALS.hwa2_echo_mismatch);
        log_error!(
            "L4: the KMD's HWA2 output for alloc={:#x} DOES NOT ECHO the create input -- sent \
             kind={} flags={:#x} bind={:#x} misc={:#x} vidpn={:#x} swizzle={} memory={} \
             planes={} byteSize={} pitch={}, got kind={} flags={:#x} bind={:#x} misc={:#x} \
             vidpn={:#x} swizzle={} memory={} planes={} byteSize={} pitch={}. A corrected field \
             means the descriptor and the resource disagree",
            h_allocation,
            create_input.allocation_kind,
            create_input.flags,
            create_input.bind_flags,
            create_input.misc_flags,
            create_input.vidpn_source,
            create_input.swizzle_class,
            create_input.memory_class,
            create_input.plane_count,
            create_input.byte_size,
            create_input.planes[0].row_pitch,
            allocation_desc.allocation_kind,
            allocation_desc.flags,
            allocation_desc.bind_flags,
            allocation_desc.misc_flags,
            allocation_desc.vidpn_source,
            allocation_desc.swizzle_class,
            allocation_desc.memory_class,
            allocation_desc.plane_count,
            allocation_desc.byte_size,
            allocation_desc.planes[0].row_pitch,
        );
        // SAFETY: as above -- the only reference to a handle nothing recorded.
        let deallocated =
            unsafe { deallocate_committed(dev, h_allocation, DeallocateForm::ByHandleList) };
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
        return Err(E_FAIL);
    }

    // ── 6. record ──────────────────────────────────────────────────────────
    //
    // ⚠ **No venus ownership transfer, and its absence is the retirement.** This step
    // No renderer identity or instance diagnostic is queried here. HRA1 plus
    // the exact WDDM callback results are the complete ownership graph.
    let identity = identity12::AllocationIdentity {
        // ⚠ An identity token, never dereferenced by the table -- see
        // `identity12`'s module doc for the whole argument, including how the
        // address-recycling hazard is closed.
        engine_resource,
        memory_offset: 0,
        memory_size: allocation_bytes,
        h_allocation,
        h_km_resource: alloc.hKMResource,
        h_rt_resource: h_rt_resource.handle as usize,
        gpu_virtual_address: allocation_info.GpuVirtualAddress,
        gpuva_reserved_bytes: 0,
        standalone: false,
        device_generation: association.device_generation,
        outer_allocation_token: association.outer_allocation_token,
        // ⭐ The KMD's own stamp, out of the validated output descriptor. ⛔ **Never
        // an identity lookup key** (§10.3): it is kept so a later stale-descriptor
        // finding has something to compare against and so the log line below reports
        // what the kernel assigned rather than what this driver hoped for.
        allocation_generation: allocation_desc.allocation_generation,
        geometry: identity12::IdentityGeometry {
            width: res_arg.Width,
            height: res_arg.Height,
            depth_or_array_size: res_arg.DepthOrArraySize,
            mip_levels: res_arg.MipLevels,
            sample_count: res_arg.SampleDesc.Count,
            dxgi_format: res_arg.Format as u32,
        },
        // ⭐ The SAME number that went into the descriptor's plane record above, not a
        // second derivation of it. ⚠ Read out of the local rather than out of the
        // read-back descriptor, so that the echo check above is comparing two
        // independent things rather than one thing with itself.
        pitch,
        heap_flags: heap_arg.Flags as u32,
        last_context_generation: 0,
        teardown_pending: false,
    };

    match identity12::lock(&dev.outer_allocations).commit(identity, cpu_backing.take()) {
        Ok(()) => {
            L4_REFUSALS.identity_recorded.bump();
        }
        Err((refusal, cpu_backing)) => {
            note_refusal(&L4_REFUSALS.identity_registry_alloc_failed);
            log_error!(
                "L4: device-owned identity commit REFUSED ({:?}) -- committed resource {:#x} \
                 cannot be recorded; rolling it back. {}x{} fmt={} heapFlags={:#x}",
                refusal,
                engine_resource,
                res_arg.Width,
                res_arg.Height,
                res_arg.Format,
                heap_arg.Flags,
            );
            // SAFETY: as above -- the only reference to a handle nothing recorded.
            let deallocated =
                unsafe { deallocate_committed(dev, h_allocation, DeallocateForm::ByHandleList) };
            finish_cpu_backing_rollback(cpu_backing, deallocated);
            return Err(E_FAIL);
        }
    }

    let n = L4_REFUSALS.identity_recorded.get();
    if n <= LOG_BUDGET {
        // ⚠ **Read out of the RECORD, field by field, not out of the locals it was
        // built from**, and that is not stylistic. It prints what was stored rather
        // than what was intended, so a field the construction above got wrong is
        // visible here; and an entry whose fields no code reads is `dead_code`, which
        // `PARALLEL.md` §10 forbids silencing on hand-written lines (R908) -- so
        // `pitch` is read out of the record too, even though the local it was built
        // from is two statements away.
        log_error!(
            "L4: COMMITTED alloc={:#x} km={:#x} rt={:#x} res={:#x} gen={} \
             off={} size={} gpuva=0x{:x} pitch={} {}x{}x{} mips={} samples={} fmt={} \
             heapFlags={:#x} token={} deviceGen={} (x{n})",
            identity.h_allocation,
            identity.h_km_resource,
            identity.h_rt_resource,
            identity.engine_resource,
            identity.allocation_generation,
            identity.memory_offset,
            identity.memory_size,
            identity.gpu_virtual_address,
            identity.pitch,
            identity.geometry.width,
            identity.geometry.height,
            identity.geometry.depth_or_array_size,
            identity.geometry.mip_levels,
            identity.geometry.sample_count,
            identity.geometry.dxgi_format,
            identity.heap_flags,
            identity.outer_allocation_token,
            identity.device_generation,
        );
    }
    Ok(identity)
}

unsafe fn deallocate_standalone_outer(
    outer: &device12::Vkd3dOuterContext12,
    h_allocation: ddi12::D3DKMT_HANDLE,
) -> bool {
    if h_allocation == 0 || outer.um_callbacks.is_null() {
        return false;
    }
    let Some(deallocate) = (unsafe { (*outer.um_callbacks).pfnDeallocateCb }) else {
        return false;
    };
    let allocation = h_allocation;
    let arg = ddi12::D3D12DDICB_DEALLOCATE_0022 {
        hResource: core::ptr::null_mut(),
        NumAllocations: 1,
        HandleList: core::ptr::from_ref(&allocation),
        Flags: ddi12::D3D12DDI_DEALLOCATE_FLAGS_0022_D3D12DDI_DEALLOCATE_FLAGS_0022_NONE,
    };
    (unsafe { deallocate(outer.h_rt_device, core::ptr::from_ref(&arg)) }) >= 0
}

/// Construct and retain the exact standalone WDDM allocation for one
/// vkd3d-internal `VkDeviceMemory`. The returned HRA1 is copied into that exact
/// synchronous `vkAllocateMemory` pNext chain; no WDDM handle or GPUVA crosses
/// the bridge. Its optional CPU data view is not an identity or lookup key.
pub(crate) unsafe fn allocate_vkd3d_internal_wddm_memory(
    outer: &device12::Vkd3dOuterContext12,
    bytes: u64,
    cpu_visible: bool,
    device_local: bool,
) -> Result<helios_protocol::HeliosResourceAssociationV1, i32> {
    if bytes == 0 || outer.device_generation == 0 || outer.um_callbacks.is_null() {
        return Err(E_INVALIDARG);
    }
    let Some(allocate) = (unsafe { (*outer.um_callbacks).pfnAllocateCb }) else {
        return Err(E_FAIL);
    };
    let mut cpu_backing = if cpu_visible {
        Some(helios_umd_common::cpu_backing::CpuBacking::new(bytes).ok_or(E_OUTOFMEMORY)?)
    } else {
        None
    };
    let cpu_mapping = cpu_backing
        .as_ref()
        .map_or(core::ptr::null_mut(), |backing| backing.as_ptr());
    let association = identity12::lock(&outer.outer_allocations)
        .reserve(outer.device_generation, bytes, cpu_mapping)
        .map_err(|_| E_OUTOFMEMORY)?;

    let mut create_input = helios_protocol::HeliosWddmAllocationDescV2::header(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        0,
    );
    create_input.byte_size = bytes;
    create_input.allocation_kind = helios_protocol::HELIOS_HWA2_KIND_BUFFER;
    create_input.swizzle_class = helios_protocol::HELIOS_HWA2_SWIZZLE_LINEAR;
    if cpu_visible {
        create_input.flags = helios_protocol::HELIOS_HWA2_FLAG_CPU_VISIBLE;
        create_input.memory_class = helios_protocol::HELIOS_HWA2_MEMORY_CPU_VISIBLE;
    } else if device_local {
        create_input.memory_class = helios_protocol::HELIOS_HWA2_MEMORY_DEVICE_LOCAL;
    } else {
        create_input.memory_class = helios_protocol::HELIOS_HWA2_MEMORY_SHARED;
    }
    create_input
        .validate_create_input(helios_protocol::HELIOS_PACKAGE_GENERATION)
        .map_err(|_| E_INVALIDARG)?;

    let mut private_bytes = [0u8; helios_protocol::HELIOS_HWA2_BYTES as usize];
    unsafe {
        core::ptr::copy_nonoverlapping(
            core::ptr::from_ref(&create_input).cast::<u8>(),
            private_bytes.as_mut_ptr(),
            private_bytes.len(),
        );
    }
    let private_ptr = private_bytes.as_mut_ptr().cast::<c_void>();
    let private_size = u32::from(helios_protocol::HELIOS_HWA2_BYTES);
    let mut allocation_info = ddi12::D3D12DDI_ALLOCATION_INFO_0022 {
        hAllocation: 0,
        pSystemMem: cpu_mapping.cast_const(),
        pPrivateDriverData: private_ptr,
        PrivateDriverDataSize: private_size,
        VidPnSourceId: helios_protocol::D3DDDI_ID_UNINITIALIZED,
        Flags: ddi12::D3D12DDI_ALLOCATION_INFO_FLAGS_0022_D3D12DDI_ALLOCATION_INFO_FLAGS_0022_NONE,
        GpuVirtualAddress: 0,
        Priority: 0,
        Reserved: [0; 5],
    };
    let mut arg = ddi12::D3D12DDICB_ALLOCATE_0022 {
        pPrivateDriverData: private_ptr,
        PrivateDriverDataSize: private_size,
        hResource: core::ptr::null_mut(),
        hKMResource: 0,
        NumAllocations: 1,
        pAllocationInfo: core::ptr::from_mut(&mut allocation_info),
    };
    let hr = unsafe { allocate(outer.h_rt_device, core::ptr::from_mut(&mut arg)) };
    if hr < 0 || allocation_info.hAllocation == 0 {
        let deallocated = allocation_info.hAllocation == 0
            || unsafe { deallocate_standalone_outer(outer, allocation_info.hAllocation) };
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
        return Err(if hr < 0 { hr } else { E_OUTOFMEMORY });
    }
    let h_allocation = allocation_info.hAllocation;
    let output =
        match helios_protocol::HeliosWddmAllocationDescV2::from_private_data(&private_bytes) {
            Ok(output)
                if output
                    .validate_create_output(helios_protocol::HELIOS_PACKAGE_GENERATION)
                    .is_ok() =>
            {
                output
            }
            _ => {
                let deallocated = unsafe { deallocate_standalone_outer(outer, h_allocation) };
                finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
                return Err(E_FAIL);
            }
        };
    let echoed = helios_protocol::HeliosWddmAllocationDescV2 {
        allocation_generation: 0,
        flags: output.flags & !helios_protocol::HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
        ..output
    };
    if echoed != create_input {
        let deallocated = unsafe { deallocate_standalone_outer(outer, h_allocation) };
        finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
        return Err(E_FAIL);
    }

    let (gpuva, gpuva_reserved_bytes) = match unsafe {
        outer
            .outer_command_pool
            .map_internal_allocation(h_allocation, bytes)
    } {
        Ok(mapping) => mapping,
        Err(hr) => {
            let deallocated = unsafe { deallocate_standalone_outer(outer, h_allocation) };
            finish_cpu_backing_rollback(cpu_backing.take(), deallocated);
            return Err(hr);
        }
    };
    let identity = identity12::AllocationIdentity {
        engine_resource: 0,
        memory_offset: 0,
        memory_size: bytes,
        allocation_generation: output.allocation_generation,
        h_allocation,
        h_km_resource: arg.hKMResource,
        h_rt_resource: 0,
        gpu_virtual_address: gpuva,
        gpuva_reserved_bytes,
        standalone: true,
        device_generation: association.device_generation,
        outer_allocation_token: association.outer_allocation_token,
        geometry: identity12::IdentityGeometry {
            width: 0,
            height: 0,
            depth_or_array_size: 0,
            mip_levels: 0,
            sample_count: 0,
            dxgi_format: 0,
        },
        pitch: 0,
        heap_flags: 0,
        last_context_generation: 0,
        teardown_pending: false,
    };
    if let Err((_refusal, cpu_backing)) =
        identity12::lock(&outer.outer_allocations).commit(identity, cpu_backing.take())
    {
        let _ = unsafe {
            outer
                .outer_command_pool
                .free_internal_gpuva(gpuva, gpuva_reserved_bytes)
        };
        let deallocated = unsafe { deallocate_standalone_outer(outer, h_allocation) };
        finish_cpu_backing_rollback(cpu_backing, deallocated);
        return Err(E_OUTOFMEMORY);
    }
    Ok(association)
}

/// Release one WDDM allocation this driver minted with `pfnAllocateCb`.
///
/// `KMD_IMPACT.md` §14a.3 UP-5's second half, and the one whose absence
/// `PENDING.md` §S-3 item 4 prices: without it every back buffer leaks one WDDM
/// allocation per `ResizeBuffers`, and `ResizeBuffers` fires on every window drag.
/// That is the 54th session's leak class, one API generation later.
///
/// # ⛔ The wire shape is EITHER/OR, and both together is `E_INVALIDARG`
///
/// `umd/src/forward/state.rs:90-105` states the contract and what breaking it cost:
/// *"EITHER `hResource` alone, OR `NumAllocations`+`HandleList` with `hResource`
/// NULL. Both together is E_INVALIDARG -- the old 0x80070057, which also leaked
/// opened resources."* [`DeallocateForm`] is what makes the illegal combination
/// unrepresentable here, exactly as D3D11's enum of the same name does: the caller
/// picks a form and this function writes the other half as the contract's zero.
///
/// # Safety
/// `h_allocation` must be a handle `pfnAllocateCb` returned to this driver and which
/// has not already been deallocated; passing one twice is a kernel-handle double
/// free. On [`DeallocateForm::ByResource`] the handle must be the runtime resource
/// the allocation was associated with at the create.
unsafe fn deallocate_committed(
    dev: &HeliosD3D12Device,
    h_allocation: ddi12::D3DKMT_HANDLE,
    form: DeallocateForm,
) -> bool {
    if dev.um_callbacks.is_null() {
        note_refusal(&L4_REFUSALS.deallocate_cb_missing);
        return false;
    }
    // SAFETY: non-null per the check; the same table `create_committed_allocation` read.
    let Some(deallocate_cb) = (unsafe { (*dev.um_callbacks).pfnDeallocateCb }) else {
        // ⛔ A leaked WDDM allocation, and it is counted rather than ignored: the
        // handle is now unreachable and only process exit frees it.
        note_refusal(&L4_REFUSALS.deallocate_cb_missing);
        log_error!(
            "L4: p12UMCallbacks->pfnDeallocateCb is absent -- the WDDM allocation {:#x} is \
             LEAKED for the lifetime of this process",
            h_allocation,
        );
        return false;
    };
    // ⚠ `handle` must outlive the call: `HandleList` points into it on the
    // handle-list arm. Declared before `arg` so that is a lifetime the compiler
    // enforces rather than a comment.
    let handle = h_allocation;
    let arg = match form {
        DeallocateForm::ByResource(h_rt_resource) => ddi12::D3D12DDICB_DEALLOCATE_0022 {
            hResource: h_rt_resource.handle,
            // ⛔ 0 and null, because `hResource` is set. Both forms at once is
            // `E_INVALIDARG`.
            NumAllocations: 0,
            HandleList: core::ptr::null(),
            Flags: ddi12::D3D12DDI_DEALLOCATE_FLAGS_0022_D3D12DDI_DEALLOCATE_FLAGS_0022_NONE,
        },
        DeallocateForm::ByHandleList => ddi12::D3D12DDICB_DEALLOCATE_0022 {
            // ⛔ NULL, because the handle list is set.
            hResource: core::ptr::null_mut(),
            NumAllocations: 1,
            HandleList: core::ptr::from_ref(&handle),
            Flags: ddi12::D3D12DDI_DEALLOCATE_FLAGS_0022_D3D12DDI_DEALLOCATE_FLAGS_0022_NONE,
        },
    };
    // SAFETY: a non-null callback out of the runtime's own corelayer table, given
    // the device handle it supplied and a fully initialised live local.
    let hr = unsafe { deallocate_cb(dev.h_rt_device, core::ptr::from_ref(&arg)) };
    if hr < 0 {
        note_refusal(&L4_REFUSALS.deallocate_cb_failed);
        log_error!(
            "L4: pfnDeallocateCb FAILED hr={:#010x} for allocation {:#x} rtResource={:p} \
             handleList={} -- the allocation is leaked",
            hr as u32,
            h_allocation,
            arg.hResource,
            arg.NumAllocations,
        );
        return false;
    }
    true
}

/// Complete reverse teardown after vkd3d/Mesa accepted the exact terminal
/// allocation scope. The caller has already removed the pending token from the
/// device registry, so this is the WDDM allocation's sole remaining owner.
pub(crate) unsafe fn retire_outer_allocation(
    outer: &device12::Vkd3dOuterContext12,
    identity: identity12::AllocationIdentity,
) -> bool {
    let retired = if identity.standalone {
        let unmapped = unsafe {
            outer
                .outer_command_pool
                .free_internal_gpuva(identity.gpu_virtual_address, identity.gpuva_reserved_bytes)
        };
        let deallocated = unsafe { deallocate_standalone_outer(outer, identity.h_allocation) };
        unmapped && deallocated
    } else {
        let Some(dev) = (unsafe { device12::device(outer.h_device) }) else {
            return false;
        };
        unsafe {
            deallocate_committed(
                dev,
                identity.h_allocation,
                DeallocateForm::ByResource(ddi12::D3D12DDI_HRTRESOURCE {
                    handle: identity.h_rt_resource as *mut c_void,
                }),
            )
        }
    };
    if retired {
        L4_REFUSALS.identity_removed.bump();
    }
    retired
}

/// The one legal shape of a `D3D12DDICB_DEALLOCATE_0022` call.
///
/// ⭐ **A mirror of `umd/src/forward/state.rs`'s `DeallocateForm`, and it exists for
/// the same measured reason**: the wire contract is *either* `hResource` *or*
/// `NumAllocations`+`HandleList`, and both together returned `0x80070057` and leaked.
/// Constructing this enum is the only way [`deallocate_committed`] builds the struct,
/// so the illegal combination is unrepresentable rather than merely avoided.
///
/// ⚠ **Which arm goes where is the D3D11 pairing, not a preference**, and the two
/// arms are used from two different places for reasons that do not generalise to each
/// other:
///
/// * [`Self::ByHandleList`] on the **rollback inside the create**. This is what
///   `umd/src/forward/resource.rs`'s `deallocate_standalone` does, and the reason is
///   that the create is about to fail: the runtime resource is being abandoned, and
///   naming it would ask the runtime to release every allocation it tracks for a
///   resource whose creation never completed. Naming the one handle this driver made
///   is exactly what needs undoing.
/// * [`Self::ByResource`] on the **destroy**. `DeallocateForm::select` prefers it
///   there for a stated reason — *"the runtime then releases every allocation it
///   tracks for the resource — created AND opened instances"* — which is the correct
///   behaviour when the resource itself is going away.
enum DeallocateForm {
    /// Name the runtime resource; the runtime releases what it tracks for it.
    ByResource(ddi12::D3D12DDI_HRTRESOURCE),
    /// Name this driver's one allocation handle, with `hResource` NULL.
    ByHandleList,
}

/// Arm 1 of the fused create: both argument pointers -> an explicit engine heap
/// plus an engine resource placed at offset zero.
///
/// ⭐ The DDI deliberately does not distinguish an API committed resource from
/// an explicit heap created with a base resource. `ResourceHeaps.md:1186` gives
/// both the same fused `(heap, resource)` shape, and DDI 0115 eventually added
/// `D3D12DDI_HEAP_FLAG_0115_IMPLICIT` because pre-0115 drivers otherwise needed
/// fragile heuristics to tell them apart. Helios negotiates 0110, so it must not
/// guess. The one representation valid for both cases is an explicit engine
/// heap with the resource at offset zero: it preserves the fused allocation for
/// committed resources and also leaves a real heap behind when later child
/// resources name the base resource through `ReuseBufferGPUVA`.
///
/// The resource is the heap's map anchor. Its [`HeapSpan`] also makes the DDI's
/// resource-only child arm resolve to this same heap without an `hHeap`, which
/// is the measured Windows 11 call shape.
///
/// ⭐ **It is also the only arm that creates physical backing and a resource in
/// one DDI declaration**, so it is where [`create_committed_allocation`] runs —
/// see that function for the whole create-time identity path and for why an
/// unadoptable fused resource fails the create.
///
/// # Safety
/// `heap_arg` and `res_arg` must be live for the call; both slots must be this
/// driver's own private blocks, already cleared by the caller. `h_rt_resource` must
/// be the runtime handle `pfnCreateHeapAndResource` was invoked with — it is live
/// for the duration of that DDI, which is the duration of this call.
///
// ⚠ Eight arguments, and each one is a distinct piece of the DDI call this arm is
// forwarding: the device, the engine, the two argument structs, the optional clear
// value, the runtime handle and the two private blocks. Bundling them into a struct
// would be a second shape of `pfnCreateHeapAndResource`'s parameter list that has to
// be kept in step with the real one. Same lint and same judgement as
// `umd/src/forward/pipeline.rs:8`.
#[allow(clippy::too_many_arguments)]
unsafe fn create_fused_heap_and_resource(
    dev: &HeliosD3D12Device,
    device10: &ID3D12Device10,
    heap_arg: &ddi12::D3D12DDIARG_CREATEHEAP_0001,
    res_arg: &ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    p_clear: *const ddi12::D3D12DDI_CLEAR_VALUES,
    h_rt_resource: ddi12::D3D12DDI_HRTRESOURCE,
    heap_slot: Slot<Boxed<HeapState>>,
    resource_slot: Slot<Boxed<ResourceState>>,
) -> Hresult {
    note_node_masks(heap_arg);
    // SAFETY: `res_arg` is live per the caller's guarantee.
    let Some(desc) = (unsafe { resource_desc1(res_arg) }) else {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        log_error!(
            "L4: fused heap+resource create with unknown D3D12DDI_RESOURCE_TYPE {}",
            res_arg.ResourceType,
        );
        return E_INVALIDARG;
    };
    // SAFETY: `p_clear` is `_In_opt_` and live for the call per the caller.
    let clear = unsafe { clear_value(p_clear, res_arg.Flags) };
    let is_buffer = desc.Dimension == D3D12_RESOURCE_DIMENSION_BUFFER;
    // SAFETY: `res_arg` is live; the slice is bounded and used only for this
    // call.
    let castable = unsafe { castable_formats(res_arg) };

    let mut engine_heap_flags = heap_flags(heap_arg.Flags, PrimaryTranslation::VenusExport);
    // ⛔ The negotiated D3D12 DDI has no SHARED heap bit. The deployed runtime
    // sends neither HEAP_FLAG_PRIMARY nor RESOURCE_OPTIMIZATION_FLAG_PRIMARY for
    // flip-model back buffers, then later asks dxgkrnl to share the fused
    // resource's allocation. Therefore every fused resource must be born
    // exportable: this is the exact DDI arm that creates the backing heap, not a
    // format/bind/geometry guess. The fork's private flag makes the memory
    // allocator-dedicated and venus-exportable.
    //
    // ⚠ **The second half of that sentence used to read "so pfnAllocateCb can ADOPT
    // it below" and it is restated rather than left stale.** Nothing is adopted:
    // `HeliosWddmAllocationDescV2` carries no host resource token (§10.3,
    // `docs/retirement/K4-CONTRACT.md` §5) and the KMD creates the allocation's
    // backing. What the flag still buys the create is the **dedicated** half — a
    // resource that owns its whole `VkDeviceMemory` at offset 0, which is the
    // precondition `create_committed_allocation` asserts before it can state HWA2's
    // `byte_size` and bound a plane record against it.
    engine_heap_flags |= HELIOS_HEAP_FLAG_VENUS_EXPORT;
    L4_REFUSALS.committed_venus_export.bump();

    let engine_heap_desc = D3D12_HEAP_DESC {
        SizeInBytes: heap_arg.ByteSize,
        Properties: D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_CUSTOM,
            CPUPageProperty: cpu_page_property(heap_arg.CPUPageProperty),
            MemoryPoolPreference: memory_pool(heap_arg.MemoryPool),
            CreationNodeMask: heap_arg.CreationNodeMask,
            VisibleNodeMask: heap_arg.VisibleNodeMask,
        },
        Alignment: heap_arg.Alignment,
        Flags: engine_heap_flags,
    };

    let mut cpu_backing = if cpu_page_property_is_visible(heap_arg.CPUPageProperty) {
        match helios_umd_common::cpu_backing::CpuBacking::new(heap_arg.ByteSize) {
            Some(backing) => Some(backing),
            None => {
                note_refusal(&L4_REFUSALS.identity_registry_alloc_failed);
                return E_OUTOFMEMORY;
            }
        }
    } else {
        None
    };
    let cpu_mapping = cpu_backing
        .as_ref()
        .map_or(core::ptr::null_mut(), |backing| backing.as_ptr());
    let association = match identity12::lock(&dev.outer_allocations).reserve(
        dev.translator.session_generation(),
        heap_arg.ByteSize,
        cpu_mapping,
    ) {
        Ok(association) => association,
        Err(refusal) => {
            note_refusal(&L4_REFUSALS.identity_registry_alloc_failed);
            log_error!("L4: HRA1 token reservation REFUSED: {:?}", refusal);
            return E_FAIL;
        }
    };
    // SAFETY: the descriptor is live for the synchronous bridge call. The
    // bridge copies `association` into vkd3d's exact vkAllocateMemory pNext
    // chain and transfers one owned heap reference on success.
    let Some(engine_heap) = (unsafe {
        dev.engine.create_associated_heap(
            (&engine_heap_desc as *const D3D12_HEAP_DESC) as usize,
            &association,
        )
    }) else {
        note_refusal(&L4_REFUSALS.heap_create_engine_failed);
        log_error!(
            "L4: associated fused CreateHeap REFUSED token={} size={} align={} pool={} cpu={} flags={:#x}",
            association.outer_allocation_token,
            heap_arg.ByteSize,
            heap_arg.Alignment,
            heap_arg.MemoryPool,
            heap_arg.CPUPageProperty,
            heap_arg.Flags,
        );
        return E_FAIL;
    };

    let mut out: Option<ID3D12Resource> = None;
    // SAFETY: every pointer argument is a live local or a bounded slice built
    // above; `out` is a live local the engine writes an owned reference into.
    // The protected-session parameter is explicitly `None`: this driver reports
    // no protected-resource support, and an arriving session is counted in
    // `create_heap_and_resource` rather than forwarded.
    let created = unsafe {
        device10.CreatePlacedResource2(
            &engine_heap,
            0,
            &desc,
            barrier_layout(res_arg.InitialBarrierLayout, is_buffer),
            clear.as_ref().map(core::ptr::from_ref),
            castable,
            &mut out,
        )
    };
    // ⚠ As `create_heap_only`: the engine's HRESULT is forwarded, not replaced.
    let hr = match created {
        Ok(()) => S_OK,
        Err(err) => err.code().0,
    };
    let Some(resource) = out.filter(|_| hr >= 0) else {
        note_refusal(&L4_REFUSALS.resource_create_engine_failed);
        log_error!(
            "L4: fused CreatePlacedResource2(offset=0) FAILED hr={:#010x} type={} fmt={} {}x{}x{} mips={} \
             samples={} flags={:#x} layout={} heapFlags={:#x}",
            hr as u32,
            res_arg.ResourceType,
            res_arg.Format,
            res_arg.Width,
            res_arg.Height,
            res_arg.DepthOrArraySize,
            res_arg.MipLevels,
            res_arg.SampleDesc.Count,
            res_arg.Flags,
            res_arg.InitialBarrierLayout,
            heap_arg.Flags,
        );
        return if hr < 0 { hr } else { E_FAIL };
    };

    // ── UP-5: every committed resource gets a kernel allocation here ────────
    //
    // ⛔ **Before the state is stored, and the ordering is what makes the failure
    // clean.** On this path `resource` is still an owned local: returning drops it
    // and the engine releases the resource, so a refused primary leaves nothing
    // behind — no half-built `ResourceState`, no heap block, and the two slots the
    // caller cleared stay null, which is exactly what a failed create is required to
    // leave (`umd_common::slot`'s `DdiHandle` doc). Storing first and then failing
    // would leak an `ID3D12Resource` and an `ID3D12Heap` per refused swapchain
    // buffer.
    //
    // ⚠ **This IS a change of severity from UP-4**, where the identity was recorded
    // and *"never fails the create"*. UP-4 recorded bookkeeping; this mints a kernel
    // object the runtime may later ask dxgkrnl to share, and a committed resource
    // without one cannot satisfy that contract. `create_committed_allocation`'s doc
    // carries the argument.
    // SAFETY: `dev` is the live device this create is running on, `resource` the
    // engine resource just created (owned here, so alive for the call), and
    // `h_rt_resource` the runtime handle this DDI was invoked with.
    let identity = match unsafe {
        create_committed_allocation(
            dev,
            device10,
            &resource,
            heap_arg,
            res_arg,
            &desc,
            h_rt_resource,
            &association,
            cpu_backing.take(),
        )
    } {
        Ok(identity) => identity,
        Err(hr) => return hr,
    };

    // SAFETY: `resource_slot` is this driver's private block for the resource
    // being created, cleared by the caller and written exactly once.
    unsafe {
        resource_slot.store(ResourceState {
            resource: Some(resource.clone()),
            // The fused DDI resource starts at zero in the explicit engine
            // heap. A later resource-only child names this resource and adds
            // its `ReuseBufferGPUVA.Offset` to this base.
            span: Some(HeapSpan {
                heap: engine_heap.clone(),
                base_offset: 0,
            }),
            desc,
            // ⚠ The visible-node mask comes from the heap the runtime asked
            // for, not from a constant: it is the one arm where the DDI states
            // it. The placed arm has no heap argument and passes 1, which is
            // this single-node adapter's only legal value (`misc.rs`'s
            // `HELIOS_PHYSICAL_ADAPTER_COUNT`).
            alloc_info: allocation_info_from_engine(
                device10,
                &desc,
                res_arg,
                heap_arg.VisibleNodeMask,
            ),
            // The fused arm: both arg pointers were non-null, so the sizing
            // asked for a heap block and the explicit heap stored below is this
            // driver's to reclaim.
            owns_heap_block: true,
            identity: Some(identity),
        });
    }
    // SAFETY: as above, for the fused heap's block.
    unsafe {
        heap_slot.store(HeapState {
            heap: Some(engine_heap),
            map_anchor: Some(resource),
            byte_size: heap_arg.ByteSize,
        });
    }
    S_OK
}

/// Arm 3 of the fused create: `pCreateResource` alone.
///
/// `ReuseBufferGPUVA.BaseAddress.UMD.hResource` is the spec's discriminator:
/// non-NULL is a **placed** resource whose parent's span gives the engine heap
/// and base offset; NULL is a **reserved** (tiled) resource
/// (`ResourceHeaps.md:1204`), which this driver refuses because `caps12.rs:278`
/// reports `TiledResourcesTier = NOT_SUPPORTED` and creating one would
/// contradict the caps this device was accepted on.
///
/// # ⭐ Two ways to find the target heap, tried in the spec's order
///
/// 1. **`ReuseBufferGPUVA`**, which is what `ResourceHeaps.md:1212` documents as
///    the only placement parent the DDI can express. Resolving the parent
///    resource yields its [`HeapSpan`], i.e. an engine heap and the parent's own
///    base offset within it; the child lands at `base + requested`.
/// 2. **`hHeap`**, the argument `pfnCreateHeapAndResource` carries on *every*
///    arm. ⚠ Whether the runtime populates it on the placed arm is not
///    established by any document in this set, so it is a **fallback and not the
///    primary**: taking it first would make the driver depend on behaviour
///    nobody has measured, while taking it second turns an unresolved
///    `ReuseBufferGPUVA` from a refusal into a working create wherever the
///    runtime does supply it. Which path was taken is visible, because path 2
///    only runs after path 1 has already bumped `ResourcePlacementUnresolved`.
///
/// ⛔ **The reserved test comes AFTER both paths, not before, and the ordering
/// is load-bearing.** Testing `hResource == NULL` first made path 2 unreachable
/// on every possible input — a NULL parent returned `E_NOTIMPL` before `hHeap`
/// was ever consulted, and a non-NULL one that failed to resolve then landed on
/// a slot the caller had just nulled. An unreachable fallback is not an
/// instrument, and `ResourcePlacementUnresolved` could never have read non-zero
/// with placed creates still succeeding, which is exactly the gate check the
/// lane's own §6 U3 rests on. So a resource is declared *reserved* only when
/// **neither** channel names a heap: no `ReuseBufferGPUVA` parent and no usable
/// `hHeap`. ⚠ The residual risk is the converse — a genuine reserved create that
/// arrives carrying a live `hHeap` would be placed rather than refused. That
/// cannot happen through the sizing contract this driver states (a reserved
/// resource has no heap for the runtime to name), and if it ever does it shows
/// up as a `CreatePlacedResource2` that succeeded for a resource `caps12` says
/// cannot exist, not as a silent wrong answer.
///
/// # Safety
/// `res_arg` must be live for the call, and `resource_slot` must be this
/// driver's own private block, already cleared by the caller.
unsafe fn create_placed_or_reserved(
    device10: &ID3D12Device10,
    res_arg: &ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    h_heap: ddi12::D3D12DDI_HHEAP,
    p_clear: *const ddi12::D3D12DDI_CLEAR_VALUES,
    resource_slot: Slot<Boxed<ResourceState>>,
) -> Hresult {
    // SAFETY: the union has exactly one member, `UMD`, so any initialisation the
    // runtime performed makes this read defined. `D3D12DDIARG_BUFFER_PLACEMENT`
    // is a by-value field of the live `res_arg`.
    let placement = unsafe { res_arg.ReuseBufferGPUVA.BaseAddress.UMD };
    let names_parent = !placement.hResource.pDrvPrivate.is_null();

    // Path 1 -- the documented placement parent.
    let via_parent = if names_parent {
        // SAFETY: the runtime named a resource handle it obtained from this
        // driver's own create, so the block is ours; the borrow ends inside this
        // function.
        let parent = unsafe { resource_state(placement.hResource) };
        let resolved = parent
            .and_then(|p| p.span.as_ref())
            .and_then(|span| Some((&span.heap, span.base_offset.checked_add(placement.Offset)?)));
        if resolved.is_none() {
            L4_REFUSALS.resource_placement_unresolved.bump();
            let n = L4_REFUSALS.resource_placement_unresolved.get();
            if n <= LOG_BUDGET {
                log_error!(
                    "L4: ReuseBufferGPUVA hResource={:p} offset={} did not resolve to an engine \
                     heap -- falling back to hHeap (x{n})",
                    placement.hResource.pDrvPrivate,
                    placement.Offset,
                );
            }
        }
        resolved
    } else {
        None
    };

    // Path 2 -- the fallback, now reachable from both directions: a named parent
    // that did not resolve, and no named parent at all.
    let target = match via_parent {
        Some(target) => Some(target),
        // SAFETY: `h_heap`, when non-null, is a heap block this driver wrote --
        // and on this arm the caller deliberately did NOT clear it, because the
        // paired sizing call asked for no heap block. The borrow ends inside
        // this function.
        None => unsafe { heap_state(h_heap) }
            .and_then(|state| state.heap.as_ref())
            .map(|heap| (heap, placement.Offset)),
    };

    let Some((heap, heap_offset)) = target else {
        if !names_parent {
            // ⛔ Both channels are empty, which is `ResourceHeaps.md:1204`'s
            // definition of a reserved (tiled) resource.
            note_refusal(&L4_REFUSALS.resource_reserved_refused);
            log_error!(
                "L4: reserved (tiled) resource refused -- no ReuseBufferGPUVA parent and no \
                 hHeap, and this driver reports TiledResourcesTier = NOT_SUPPORTED (caps12), so \
                 no tiled resource may exist. type={} fmt={} {}x{}",
                res_arg.ResourceType,
                res_arg.Format,
                res_arg.Width,
                res_arg.Height,
            );
            return E_NOTIMPL;
        }
        log_error!(
            "L4: placed resource refused -- neither ReuseBufferGPUVA nor hHeap names an engine \
             heap this driver owns (hHeap.pDrvPrivate={:p})",
            h_heap.pDrvPrivate,
        );
        return E_INVALIDARG;
    };

    // SAFETY: `res_arg` is live per the caller's guarantee.
    let Some(desc) = (unsafe { resource_desc1(res_arg) }) else {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        log_error!(
            "L4: CreatePlacedResource with unknown D3D12DDI_RESOURCE_TYPE {}",
            res_arg.ResourceType,
        );
        return E_INVALIDARG;
    };
    // SAFETY: `_In_opt_` and live for the call per the caller.
    let clear = unsafe { clear_value(p_clear, res_arg.Flags) };
    let is_buffer = desc.Dimension == D3D12_RESOURCE_DIMENSION_BUFFER;
    // SAFETY: `res_arg` is live; the slice is bounded and used only for this
    // call.
    let castable = unsafe { castable_formats(res_arg) };

    let mut out: Option<ID3D12Resource> = None;
    // SAFETY: the target heap and all description pointers are live for the call.
    let created = unsafe {
        device10.CreatePlacedResource2(
            heap,
            heap_offset,
            &desc,
            barrier_layout(res_arg.InitialBarrierLayout, is_buffer),
            clear.as_ref().map(core::ptr::from_ref),
            castable,
            &mut out,
        )
    };
    // ⚠ As `create_heap_only`: the engine's HRESULT is forwarded, not replaced.
    let hr = match created {
        Ok(()) => S_OK,
        Err(err) => err.code().0,
    };
    let Some(resource) = out.filter(|_| hr >= 0) else {
        note_refusal(&L4_REFUSALS.resource_create_engine_failed);
        log_error!(
            "L4: CreatePlacedResource2 FAILED hr={:#010x} at heap offset {heap_offset} type={} \
             fmt={} {}x{}x{} mips={} flags={:#x}",
            hr as u32,
            res_arg.ResourceType,
            res_arg.Format,
            res_arg.Width,
            res_arg.Height,
            res_arg.DepthOrArraySize,
            res_arg.MipLevels,
            res_arg.Flags,
        );
        return if hr < 0 { hr } else { E_FAIL };
    };

    // SAFETY: this driver's own private block for the resource, cleared by the
    // caller and written exactly once.
    unsafe {
        resource_slot.store(ResourceState {
            resource: Some(resource),
            span: Some(HeapSpan {
                heap: heap.clone(),
                base_offset: heap_offset,
            }),
            desc,
            alloc_info: allocation_info_from_engine(device10, &desc, res_arg, 1),
            // ⛔ The placed arm: `pCreateHeap` was NULL, the sizing answered
            // `Heap = 0`, and the `hHeap` this create was handed is an
            // **already-live** heap owned by its own create. Destroying this
            // resource must not touch it.
            owns_heap_block: false,
            identity: None,
        });
    }
    S_OK
}

/// The castable-format list, borrowed from the runtime's array.
///
/// `DXGI_FORMAT` is `#[repr(transparent)]` over `i32` in the `windows` crate and
/// the DDI's `DXGI_FORMAT` is a `c_int`, so the array can be viewed in place
/// with no copy and no transmute of a compound type. The count is runtime data,
/// so it is bounded: DXGI defines fewer than 200 formats and anything above
/// [`CASTABLE_FORMAT_LIMIT`] is not a list.
///
/// # Safety
/// `a` must be live, and `a.pCastableFormats` must address
/// `a.NumCastableFormats` `DXGI_FORMAT`s for the duration of the call.
unsafe fn castable_formats(a: &ddi12::D3D12DDIARG_CREATERESOURCE_0111) -> Option<&[DXGI_FORMAT]> {
    let count = a.NumCastableFormats as usize;
    if count == 0 || a.pCastableFormats.is_null() {
        return None;
    }
    if count > CASTABLE_FORMAT_LIMIT {
        L4_REFUSALS.castable_formats_dropped.bump();
        let n = L4_REFUSALS.castable_formats_dropped.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: NumCastableFormats={count} exceeds the {CASTABLE_FORMAT_LIMIT} bound -- the \
                 list is dropped rather than read (x{n})"
            );
        }
        return None;
    }
    // SAFETY: non-null and `count` elements per the caller's guarantee, and
    // `DXGI_FORMAT` is `#[repr(transparent)]` over the `i32` the DDI array
    // holds, so the element type is layout-identical.
    Some(unsafe { core::slice::from_raw_parts(a.pCastableFormats.cast::<DXGI_FORMAT>(), count) })
}

/// `pfnCreateHeapAndResource` — the fused create.
///
/// # Safety
/// `p_heap` / `p_resource` / `p_clear`, when non-null, must be live for the
/// call. `h_heap.pDrvPrivate` and `h_resource.pDrvPrivate` must address the
/// blocks the paired [`calc_private_heap_and_resource_sizes`] sized.
///
/// ⚠ **`h_rt_resource` used to be `_h_rt_resource` and discarded**, under a rule in
/// [`ResourceState`]'s doc that no callback takes an `HRTRESOURCE`. UP-5 falsified
/// that rule for exactly one callback — `D3D12DDICB_ALLOCATE_0022::hResource` — so
/// the handle is now threaded to the committed arm. ⛔ It is still not *stored*: it
/// is a live parameter of this DDI and [`create_committed_allocation`] runs inside it, so the
/// only place it needs to survive to is the identity table, which keeps it as an
/// integer for the paired `pfnDeallocateCb`.
unsafe extern "C" fn create_heap_and_resource(
    h_device: ddi12::D3D12DDI_HDEVICE,
    p_heap: *const ddi12::D3D12DDIARG_CREATEHEAP_0001,
    h_heap: ddi12::D3D12DDI_HHEAP,
    h_rt_resource: ddi12::D3D12DDI_HRTRESOURCE,
    p_resource: *const ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    p_clear: *const ddi12::D3D12DDI_CLEAR_VALUES,
    h_protected_session: ddi12::D3D12DDI_HPROTECTEDRESOURCESESSION_0030,
    h_resource: ddi12::D3D12DDI_HRESOURCE,
) -> Hresult {
    // ⛔ **Only a slot the paired sizing call ASKED FOR may be cleared**, and
    // this is the single most dangerous line in the lane if it is wrong. Every
    // Create nulls the private blocks of the objects it is creating, so a failed
    // create leaves a null handle rather than stale garbage
    // (`umd_common::slot`'s `DdiHandle` doc) — but on the placed arm
    // `pCreateHeap` is NULL, [`heap_and_resource_private_sizes`] answers
    // `Heap = 0` on purpose, and the `hHeap` the runtime then passes is the
    // **already-live** heap's handle rather than a fresh block. Clearing that
    // one would null a slot whose `Box<HeapState>` this driver still owns:
    // `Slot::clear` is documented as nulling the word *without* touching what it
    // pointed at, so the `ID3D12Heap` and its map anchor would leak, and every
    // later `pfnMapHeap` / `pfnUnmapHeap` / `pfnDestroyHeapAndResource` for that
    // heap would find nothing and refuse with `ResourceHandleUnresolved`. It
    // would also falsify [`heap_state`]'s own soundness argument, which rests on
    // the box being written once and taken once.
    //
    // ⇒ the expected sizes are computed FIRST, from the same function that told
    // the runtime how many bytes to allocate, and each slot is cleared only when
    // that function asked for it.
    let expected = heap_and_resource_private_sizes(p_heap, p_resource);
    // SAFETY: each slot, when non-null, is this driver's own private block;
    // clearing writes one machine word and touches nothing it pointed at.
    let heap_slot = unsafe { Slot::<Boxed<HeapState>>::from_priv(h_heap.pDrvPrivate) };
    if expected.Heap != 0 {
        if let Some(slot) = heap_slot {
            // SAFETY: as above, and the block is one this create is building.
            unsafe { slot.clear() };
        }
    }
    // SAFETY: as above, for the resource's block; `from_priv` only records
    // non-nullness and dereferences nothing.
    let resource_slot = unsafe { Slot::<Boxed<ResourceState>>::from_priv(h_resource.pDrvPrivate) };
    if expected.Resource != 0 {
        if let Some(slot) = resource_slot {
            // SAFETY: as above, and the block is one this create is building.
            unsafe { slot.clear() };
        }
    }

    if !h_protected_session.pDrvPrivate.is_null() {
        note_refusal(&L4_REFUSALS.protected_session_ignored);
    }

    // SAFETY: device-scope DDI, so the runtime passes a handle `create_device`
    // returned `S_OK` for; the borrow ends with this call.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L4_REFUSALS.resource_no_device);
        return E_INVALIDARG;
    };
    let Some(device10) = engine_device10(dev) else {
        return E_NOTIMPL;
    };

    // ⛔ The size site and the write site agree by construction: the same
    // function that told the runtime how many bytes to allocate now says which
    // slots must exist. A block the sizing call asked for that did not arrive is
    // a refusal, not something to work around.
    //
    // ⚠ **One-directional, deliberately.** The converse -- a slot arriving that
    // the sizing call did not ask for -- is NOT an error, and asserting it was
    // would break the arm this driver most needs to work: on the placed arm the
    // sizing call answers `Heap = 0` (see [`heap_and_resource_private_sizes`])
    // precisely so the runtime keeps passing the *existing* heap's `hHeap`, and
    // that handle is non-null by design. Only "asked for and absent" is a fault.
    // (`expected` is the same value computed at the top of this function, where
    // it also decides which slots may be cleared.)
    if expected.Heap != 0 && heap_slot.is_none() {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        log_error!(
            "L4: CreateHeapAndResource private-block mismatch -- sized Heap={} but hHeap is null",
            expected.Heap,
        );
        return E_INVALIDARG;
    }
    // ⚠ Only the description-present case is a fault: the heap-only arm asks for
    // a resource block speculatively (see the sizing function's second bullet),
    // so a runtime that declines to supply one there is not misbehaving.
    if !p_resource.is_null() && resource_slot.is_none() {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        log_error!(
            "L4: CreateHeapAndResource private-block mismatch -- sized Resource={} but hResource \
             is null",
            expected.Resource,
        );
        return E_INVALIDARG;
    }

    // SAFETY: both pointers are `_In_opt_ CONST` and live for the call when
    // non-null, per the caller's guarantee; each arm reads only its own arg.
    let heap_arg = unsafe { p_heap.as_ref() };
    let res_arg = unsafe { p_resource.as_ref() };

    // ⛔ `CreateAtVirtualAddress` and `pRowMajorLayout` cannot be honoured, and
    // that is counted rather than passed over. `DDI_REFERENCE.md` §9.7 records
    // that `RecreateAt` is gated behind DDI 0111, which is absent from SDK
    // 10.0.26100.0 — so a non-zero address arriving means the build assumption
    // behind this driver's `RecreateAtTier = NOT_SUPPORTED` answer is wrong.
    if let Some(a) = res_arg {
        if a.CreateAtVirtualAddress != 0 {
            note_refusal(&L4_REFUSALS.resource_create_at_virtual_address);
            log_error!(
                "L4: CreateAtVirtualAddress={:#x} arrived, but RecreateAt is gated behind DDI \
                 0111 which this SDK does not define -- ignored",
                a.CreateAtVirtualAddress,
            );
        }
        if !a.pRowMajorLayout.is_null() {
            note_refusal(&L4_REFUSALS.resource_row_major_layout_ignored);
        }
    }

    match (heap_arg, res_arg) {
        (Some(heap_arg), Some(res_arg)) => {
            let Some(resource_slot) = resource_slot else {
                note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
                return E_INVALIDARG;
            };
            let Some(heap_slot) = heap_slot else {
                note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
                return E_INVALIDARG;
            };
            // SAFETY: both args are live references; both slots are this
            // driver's own blocks, cleared above; `dev` is the device this DDI was
            // dispatched on and `h_rt_resource` the runtime handle it carried.
            unsafe {
                create_fused_heap_and_resource(
                    dev,
                    &device10,
                    heap_arg,
                    res_arg,
                    p_clear,
                    h_rt_resource,
                    heap_slot,
                    resource_slot,
                )
            }
        }
        (Some(heap_arg), None) => {
            let Some(heap_slot) = heap_slot else {
                note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
                return E_INVALIDARG;
            };
            // SAFETY: as above.
            unsafe { create_heap_only(&device10, heap_arg, heap_slot, resource_slot) }
        }
        (None, Some(res_arg)) => {
            let Some(resource_slot) = resource_slot else {
                note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
                return E_INVALIDARG;
            };
            // SAFETY: as above.
            unsafe { create_placed_or_reserved(&device10, res_arg, h_heap, p_clear, resource_slot) }
        }
        (None, None) => {
            // Row four of the arm table.
            note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
            log_error!("L4: CreateHeapAndResource with neither a heap nor a resource description");
            E_INVALIDARG
        }
    }
}

/// `pfnDestroyHeapAndResource`.
///
/// Returns `VOID`, so the only channel is the counters (`DECISIONS.md` §7.6).
/// The resource is dropped before the heap: a placed resource holds a reference
/// to its heap, so releasing in that order keeps the engine's own teardown in
/// the order it would see from an application.
///
/// ⭐ **It is also where the UP-5 WDDM allocation is released**, and that is not
/// optional: `PENDING.md` §S-3 item 4 prices its absence at *"one leaked WDDM
/// allocation per back buffer per `ResizeBuffers`"*, and `ResizeBuffers` fires on
/// every window drag.
///
/// # Safety
/// `h_heap` and `h_resource`, when their `pDrvPrivate` is non-null, must be
/// handles [`create_heap_and_resource`] returned `S_OK` for and which have not
/// already been destroyed. Passing one twice is a double free.
unsafe extern "C" fn destroy_heap_and_resource(
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_heap: ddi12::D3D12DDI_HHEAP,
    h_resource: ddi12::D3D12DDI_HRESOURCE,
) {
    let mut seen = false;

    // ⛔ **Whether this driver may reclaim `hHeap` is decided by the RESOURCE**,
    // not by `hHeap`'s own contents, and the default is "no". `ResourceState`'s
    // `owns_heap_block` doc carries the whole argument; the short version is that
    // on the placed arm `hHeap` is an already-live heap belonging to a different
    // create, so taking its box would tear down a live `ID3D12Heap` under its
    // owner.
    //
    // ⚠ Defaulting to `false` when the resource block does not resolve is the
    // deliberate direction: not reclaiming is a leak, which is counted below and
    // is diagnosable; reclaiming something that is not ours is a use-after-free
    // in the runtime's own object graph, which is not.
    let mut owns_heap_block = false;

    // SAFETY: the caller guarantees a live, not-yet-destroyed handle; `take`
    // nulls the slot first, so a second call finds nothing and does nothing.
    if let Some(slot) = unsafe { Slot::<Boxed<ResourceState>>::from_priv(h_resource.pDrvPrivate) } {
        // SAFETY: as above.
        if let Some(state) = unsafe { slot.take() } {
            owns_heap_block = state.owns_heap_block;
            // Arm reverse teardown before either the resource or heap COM
            // reference can release the associated VkDeviceMemory. The final
            // heap release enters vkd3d's immutable begin/finish/retire edge;
            // only retire removes the token and releases the WDDM allocation.
            if let (Some(engine), Some(stored_identity)) = (state.resource.as_ref(), state.identity)
            {
                match unsafe { device12::device(h_device) } {
                    Some(dev) => {
                        let armed = identity12::lock(&dev.outer_allocations).arm_teardown(
                            engine.as_raw() as usize,
                            stored_identity.device_generation,
                            stored_identity.outer_allocation_token,
                        );
                        if let Err(refusal) = armed {
                            note_refusal(&L4_REFUSALS.identity_replaced);
                            log_error!(
                                "L4: exact token teardown arm REFUSED token={} resource={:#x}: {:?}",
                                stored_identity.outer_allocation_token,
                                stored_identity.engine_resource,
                                refusal,
                            );
                            let _ = device12::set_error(dev, E_FAIL);
                            // Fail closed: keep the COM/WDDM graph alive so the
                            // engine address cannot be recycled under a live
                            // association. Device rundown owns the leak.
                            core::mem::forget(state);
                            return;
                        }
                    }
                    None => {
                        note_refusal(&L4_REFUSALS.resource_no_device);
                        note_refusal(&L4_REFUSALS.deallocate_cb_missing);
                        core::mem::forget(state);
                        return;
                    }
                }
            }
            drop(state);
            seen = true;
        }
    }

    if owns_heap_block {
        // SAFETY: as above, for the heap.
        if let Some(slot) = unsafe { Slot::<Boxed<HeapState>>::from_priv(h_heap.pDrvPrivate) } {
            // SAFETY: as above.
            if let Some(state) = unsafe { slot.take() } {
                drop(state);
                seen = true;
            } else {
                // The paired create asked for a heap block and the destroy found
                // none: the `ID3D12Heap` and its map anchor are now unreachable.
                // ⚠ A separate counter from `ResourceHandleUnresolved` because
                // `seen` is already true here — the resource resolved — so the
                // aggregate could never have shown this.
                note_refusal(&L4_REFUSALS.heap_block_unreclaimed);
            }
        } else if !h_heap.pDrvPrivate.is_null() {
            note_refusal(&L4_REFUSALS.heap_block_unreclaimed);
        }
    }

    if !seen {
        note_refusal(&L4_REFUSALS.resource_handle_unresolved);
    }
}

// ---------------------------------------------------------------------------
// (g) pfnMapHeap / pfnUnmapHeap
// ---------------------------------------------------------------------------

/// `pfnMapHeap`.
///
/// ⭐ **Map is heap-scoped at the DDI and resource-scoped in the API.** The
/// runtime maps a heap once, ref-counts and serialises `pfnMapHeap` /
/// `pfnUnmapHeap` across application threads so the driver sees at most one live
/// mapping per heap (`ResourceHeaps.md:454`), and derives every per-resource
/// pointer from `pfnCheckSubresourceInfo` (`:1437`). This driver has no heap map
/// to forward to, so it maps the heap's anchor resource — see
/// [`HeapState::map_anchor`].
///
/// ⚠ Both ranges are passed as `NULL`, which is the API's *"the CPU may read /
/// may have written the whole resource"*. The DDI carries no range at all, so
/// the conservative direction is the only defensible one: a narrower claim would
/// be this driver inventing a promise on the application's behalf.
///
/// The D3D11 precedent is `umd/src/forward/transfer.rs:182-229`, which forwards
/// `pfnResourceMap` to the engine's `Map` and copies out the pointer; the shape
/// here is the same one, moved up to the heap.
///
/// # Safety
/// `h_heap` must be a live heap handle from [`create_heap_and_resource`], and
/// `out` must address one writable pointer the runtime owns.
unsafe extern "C" fn map_heap(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_heap: ddi12::D3D12DDI_HHEAP,
    out: *mut *mut c_void,
) -> Hresult {
    if out.is_null() {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        return E_INVALIDARG;
    }
    // ⛔ Written before anything can fail, so no path leaves the runtime reading
    // whatever was on its stack as a heap base address.
    // SAFETY: non-null per the check; the DDI declares it an out-parameter.
    unsafe { core::ptr::write_unaligned(out, core::ptr::null_mut()) };

    // SAFETY: the runtime passes a heap handle this driver wrote; the borrow
    // ends with this call.
    let Some(state) = (unsafe { heap_state(h_heap) }) else {
        note_refusal(&L4_REFUSALS.resource_handle_unresolved);
        return E_INVALIDARG;
    };
    let Some(anchor) = state.map_anchor.as_ref() else {
        L4_REFUSALS.map_heap_no_anchor.bump();
        let n = L4_REFUSALS.map_heap_no_anchor.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: MapHeap on a {} B heap with no mappable anchor -- see \
                 HeapSpanBufferUnavailable for why this heap has none (x{n})",
                state.byte_size,
            );
        }
        return E_NOTIMPL;
    };

    let mut data: *mut c_void = core::ptr::null_mut();
    // SAFETY: `anchor` is the engine resource this driver holds a reference to
    // for the heap's whole life; `data` is a live local. A null read range means
    // "the CPU may read all of it", which is the conservative reading.
    let mapped = unsafe { anchor.Map(0, None, Some(&mut data)) };
    if let Err(err) = mapped {
        L4_REFUSALS.map_heap_engine_failed.bump();
        let n = L4_REFUSALS.map_heap_engine_failed.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: MapHeap: the engine refused Map on the anchor hr={:#010x} (x{n})",
                err.code().0 as u32,
            );
        }
        return err.code().0;
    }
    // SAFETY: as the first write above.
    unsafe { core::ptr::write_unaligned(out, data) };
    L4_REFUSALS.map_heap_calls.bump();
    S_OK
}

/// `pfnUnmapHeap`.
///
/// A null written-range is the conservative direction for the same reason
/// [`map_heap`]'s null read-range is: the DDI carries no range, so claiming the
/// CPU wrote nothing would be this driver making a promise it cannot check.
///
/// # Safety
/// `h_heap` must be a live heap handle that [`map_heap`] returned `S_OK` for.
unsafe extern "C" fn unmap_heap(_h_device: ddi12::D3D12DDI_HDEVICE, h_heap: ddi12::D3D12DDI_HHEAP) {
    // SAFETY: as `map_heap`.
    let Some(state) = (unsafe { heap_state(h_heap) }) else {
        note_refusal(&L4_REFUSALS.resource_handle_unresolved);
        return;
    };
    let Some(anchor) = state.map_anchor.as_ref() else {
        // A heap that could not be mapped cannot be unmapped. `map_heap` already
        // refused and counted; counting again here would double every event.
        return;
    };
    // SAFETY: `anchor` is the engine resource this driver holds for the heap's
    // whole life; `Unmap` on a resource that is not mapped is a documented
    // no-op, so an unbalanced call cannot corrupt anything.
    unsafe { anchor.Unmap(0, None) };
}

// ---------------------------------------------------------------------------
// (g) pfnMakeResident / pfnEvict / pfnOfferResources / pfnReclaimResources
// ---------------------------------------------------------------------------

/// Zero a paging-fence array the runtime handed as an out-parameter.
///
/// ⛔ **This is the `pfnQueryNodeMap` class, and it already cost this project a
/// device once** (`forward12/misc.rs`): an `_Out_` the driver leaves untouched
/// is the runtime reading its own uninitialised memory as a value it acts on.
/// `D3D12DDIARG_MAKERESIDENT_0001::pPagingFenceValue` is
/// `_Field_size_(NumAdapters) UINT64*`, so every entry the runtime sized is
/// written, and the count comes from the argument.
///
/// # Safety
/// `values`, when non-null, must address `count` writable `UINT64`s the runtime
/// owns.
unsafe fn zero_paging_fences(values: *mut ddi12::UINT64, count: ddi12::UINT) {
    if values.is_null() {
        return;
    }
    for index in 0..count as usize {
        // SAFETY: the caller guarantees `count` writable `UINT64`s, and `index`
        // is strictly below that count.
        unsafe { core::ptr::write_unaligned(values.add(index), 0) };
    }
}

/// `pfnMakeResident`.
///
/// ⭐ **Hit twice inside one `D3D12CreateDevice` on the passing `D12-G7` run**,
/// which is why it needed a real body before anything else could work.
///
/// The honest answer is `S_OK` with no paging fence, and the reasoning is
/// `DDI_REFERENCE.md` §9.8's: **VidMm owns residency**, and it owns it for
/// *allocations*, of which this driver has none. vkd3d's memory is minted by the
/// Mesa venus ICD through its own `D3DKMT` path, so no object reachable from
/// this DDI carries a `D3DKMT_HANDLE` this driver could hand to
/// `pfnMakeResidentCb` — which is the same fact
/// [`check_resource_allocation_handle`] reports. There is nothing to page in,
/// so the operation is already complete, and "already complete" is `S_OK`.
///
/// ⛔ **`E_PENDING` is the one thing that must never be faked here.** §9.8: an
/// `E_PENDING` without a valid `pPagingFenceValue` / `WaitMask` *"hangs the
/// caller with no error anywhere"*. Returning `S_OK` makes the fence protocol
/// unreachable by construction rather than merely unused.
///
/// ⚠ The paging fences and `WaitMask` are still written. The header marks them
/// meaningful *"only if MakeResident returns E_PENDING"*, so a runtime that
/// obeys its own comment never reads them — but the cost of writing zeros is
/// nothing and the cost of being wrong about that is a device.
///
/// # Safety
/// `arg`, when non-null, must be a live `D3D12DDIARG_MAKERESIDENT_0001` whose
/// `pPagingFenceValue` addresses `NumAdapters` writable `UINT64`s.
unsafe extern "C" fn make_resident(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *mut ddi12::D3D12DDIARG_MAKERESIDENT_0001,
) -> Hresult {
    if arg.is_null() {
        note_refusal(&L4_REFUSALS.residency_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it a live in/out struct.
    let a = unsafe { &mut *arg };
    // SAFETY: the struct's own `_Field_size_` names `NumAdapters` as the count.
    unsafe { zero_paging_fences(a.pPagingFenceValue, a.NumAdapters) };
    a.WaitMask = 0;

    L4_REFUSALS.make_resident_calls.bump();
    let n = L4_REFUSALS.make_resident_calls.get();
    if n <= LOG_BUDGET {
        // ⚠ `D3DDDI_MAKERESIDENT_FLAGS` is a bitfield struct, not an integer;
        // its union carries a `Value: UINT` arm that covers every bit.
        // SAFETY: reading the `Value` arm of a union whose other arm is a
        // bitfield over the same `UINT` is defined for any initialisation the
        // runtime can have performed -- there is no invalid `u32`.
        let flags = unsafe { a.Flags.__bindgen_anon_1.Value };
        log_error!(
            "L4: MakeResident objects={} adapters={} flags={flags:#x} -> S_OK, no paging fence \
             (VidMm owns residency; this driver mints no allocations) (x{n})",
            a.NumObjects,
            a.NumAdapters,
        );
    }
    S_OK
}

/// `pfnEvict`.
///
/// The counterpart of [`make_resident`] and honest for the same reason: there is
/// no allocation to evict, so there is nothing to fail.
///
/// # Safety
/// `arg`, when non-null, must be a live `D3D12DDIARG_EVICT`.
unsafe extern "C" fn evict(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_EVICT,
) -> Hresult {
    if arg.is_null() {
        note_refusal(&L4_REFUSALS.residency_bad_arg);
        return E_INVALIDARG;
    }
    L4_REFUSALS.evict_calls.bump();
    S_OK
}

/// `pfnOfferResources`.
///
/// An offer is advisory: it tells the driver the application no longer needs the
/// contents and the driver *may* discard them. This driver keeps them, which is
/// a legal response to every offer priority, and [`reclaim_resources`] then
/// truthfully reports that nothing was discarded. The counter is what stops
/// "we accepted and did nothing" reading as "we implemented offer/reclaim".
///
/// # Safety
/// `arg`, when non-null, must be a live `D3D12DDIARG_OFFERRESOURCES`.
unsafe extern "C" fn offer_resources(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_OFFERRESOURCES,
) -> Hresult {
    if arg.is_null() {
        note_refusal(&L4_REFUSALS.residency_bad_arg);
        return E_INVALIDARG;
    }
    L4_REFUSALS.offer_resources_accepted.bump();
    S_OK
}

/// `pfnReclaimResources`.
///
/// ⛔ **`pDiscarded` is an `_Out_ BOOL` per object and it is not optional.**
/// [`offer_resources`] never discards anything, so every entry is `FALSE` — and
/// leaving the array untouched would have the runtime tell the application its
/// contents are gone, on the strength of whatever was in its own buffer. Same
/// class as `pfnQueryNodeMap`, same fix: write every entry the runtime asked
/// for, with the count from the argument.
///
/// # Safety
/// `arg`, when non-null, must be a live `D3D12DDIARG_RECLAIMRESOURCES_0001`
/// whose `pDiscarded` addresses `NumObjects` writable `BOOL`s and whose
/// `pPagingFenceValue` addresses `NumAdapters` writable `UINT64`s.
unsafe extern "C" fn reclaim_resources(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *mut ddi12::D3D12DDIARG_RECLAIMRESOURCES_0001,
) -> Hresult {
    if arg.is_null() {
        note_refusal(&L4_REFUSALS.residency_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it a live in/out struct.
    let a = unsafe { &mut *arg };
    if !a.pDiscarded.is_null() {
        for index in 0..a.NumObjects as usize {
            // SAFETY: the DDI sizes `pDiscarded` by `NumObjects` and `index` is
            // strictly below that count. `FALSE` is 0 for `BOOL`.
            unsafe { core::ptr::write_unaligned(a.pDiscarded.add(index), 0) };
        }
    }
    // SAFETY: sized by `NumAdapters`, exactly as in `make_resident`.
    unsafe { zero_paging_fences(a.pPagingFenceValue, a.NumAdapters) };
    a.WaitMask = 0;

    L4_REFUSALS.reclaim_resources_calls.bump();
    S_OK
}

// ---------------------------------------------------------------------------
// (g) pfnCalcPrivateOpenedHeapAndResourceSizes / pfnOpenHeapAndResource
// ---------------------------------------------------------------------------

/// `pfnCalcPrivateOpenedHeapAndResourceSizes` — refused, sized zero.
///
/// Paired with [`open_heap_and_resource`]'s refusal: no block is asked for,
/// because no object will be built.
///
/// # Safety
/// `_arg`, when non-null, must be live for the call. It is not dereferenced.
unsafe extern "C" fn calc_private_opened_heap_and_resource_sizes(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    _arg: *const ddi12::D3D12DDIARG_OPENHEAP_0003,
    _h_protected_session: ddi12::D3D12DDI_HPROTECTEDRESOURCESESSION_0030,
) -> ddi12::D3D12DDI_HEAP_AND_RESOURCE_SIZES {
    note_refusal(&L4_REFUSALS.open_heap_calc_refused);
    ddi12::D3D12DDI_HEAP_AND_RESOURCE_SIZES {
        Heap: 0,
        Resource: 0,
    }
}

/// `pfnOpenHeapAndResource` — REFUSED, and this is the lane's one deliberate
/// scope cut.
///
/// ⚠ It is one of the runtime's nine hard NULL-checks (`DDI_REFERENCE.md` §9.7,
/// strings:103), so it is a **real function that refuses**, never a NULL slot.
///
/// # What is missing, precisely
///
/// This slot is what discharges `DECISIONS.md` D3c — *"D3D12-created resources
/// must be able to be opened by DWM, using D3D11 and the 11 DDI"*. Two things stand
/// between here and an implementation, and neither is a line of Rust:
///
/// 1. **There is no engine entry point to adopt a foreign allocation.**
///    `D3D12DDIARG_OPENHEAP_0003` carries `D3DDDI_OPENALLOCATIONINFO` plus a
///    `D3D12DDI_HKMRESOURCE` — kernel identity from `Dxgkrnl`. vkd3d's only
///    adoption path is `ID3D12Device::OpenSharedHandle`, which takes an **NT
///    handle** from `CreateSharedHandle`, not a `D3DKMT` allocation. Bridging
///    the two means a new C++ entry point into vkd3d's internals to build a
///    `d3d12_resource` over Vulkan external memory the Mesa ICD already owns.
///    ⚠⚠ **The item that used to sit here was STALE and is deleted rather than
///    reworded**, because a lane reading only this block would have concluded the
///    D3D12 writer does not exist. It said *"the other half of the channel does not
///    exist yet either: for DWM to open a D3D12-created resource through the 11 DDI,
///    this driver must first write `HeliosWddmAllocPrivate` at create time — which it
///    cannot, because it mints no WDDM allocation at all"*. Both clauses are false:
///    [`create_committed_allocation`] writes the create-time descriptor (now
///    `HeliosWddmAllocationDescV2`) and calls `pfnAllocateCb` for **every** committed
///    resource. The channel's creator half exists; what is missing is item 1 and the
///    §5 host-resource-id gap the module doc names.
/// 2. **Nothing on the triangle path needs it.** `DDI_REFERENCE.md` §14.0's
///    measured `D12-G5` trace — device, queue, swapchain, two PSOs, two draws,
///    three presents — never reaches this slot.
///
/// ⇒ `PARALLEL.md` §9.1's *"implemented **or** explicitly refused with a named
/// counter"*, taken deliberately, with the counter and this comment as the
/// record. `GATES.md` §7.23 is where the settling experiment already lives.
///
/// `E_NOTIMPL` and not `DXGI_ERROR_UNSUPPORTED`: the latter is this project's
/// code for declining a DDI **negotiation** (`umd_common/src/hr.rs:51-56`), and
/// this is a create that a future build will implement. Neither is
/// `DXGI_ERROR_DRIVER_INTERNAL_ERROR`, which `DECISIONS.md` §7.5 reserves for a
/// genuine driver fault.
///
/// ⛔ **Neither slot is touched, and that is the same rule
/// [`create_heap_and_resource`] follows**: a Create may null only a private
/// block its own paired sizing call asked for.
/// [`calc_private_opened_heap_and_resource_sizes`] answers `{0, 0}`, so this
/// driver owns no block here — any non-null `pDrvPrivate` arriving belongs to
/// something else (an existing heap being opened into), and clearing it would
/// leak that object's `Box` and make it unresolvable for the rest of the
/// process. Refusing without writing is the only honest action for a slot that
/// asked for nothing.
///
/// # Safety
/// `_arg`, when non-null, must be live for the call. Nothing is dereferenced and
/// nothing is written.
unsafe extern "C" fn open_heap_and_resource(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    _arg: *const ddi12::D3D12DDIARG_OPENHEAP_0003,
    _h_heap: ddi12::D3D12DDI_HHEAP,
    _h_rt_resource: ddi12::D3D12DDI_HRTRESOURCE,
    _h_protected_session: ddi12::D3D12DDI_HPROTECTEDRESOURCESESSION_0030,
    _h_resource: ddi12::D3D12DDI_HRESOURCE,
) -> Hresult {
    note_refusal(&L4_REFUSALS.open_heap_and_resource_refused);
    E_NOTIMPL
}

// ---------------------------------------------------------------------------
// (h) The five introspection slots
// ---------------------------------------------------------------------------

/// Ask the engine for a resource's allocation size and alignment, and shape the
/// answer into the DDI's struct.
///
/// ⭐ **The runtime acts on all four numbers**, so a zero-fill here is a lie and
/// not a default. `DDI_REFERENCE.md` §9.7 quotes four runtime strings that
/// reject a driver's answer for a resource whose layout the runtime already
/// knows — a wrong `Layout`, a wrong `ResourceDataSize`, and any non-zero
/// `AdditionalDataSize` or `AdditionalDataHeaderSize`.
///
/// * **`Layout` is echoed verbatim.** `ResourceHeaps.md:2390`: the runtime may
///   ask with `Layout` = `_UNDEFINED`, *"in which case the driver may choose any
///   texture layout, but must echo `STANDARD_SWIZZLE` or `ROW_MAJOR` back
///   verbatim when either was requested"*. Echoing satisfies the second half
///   trivially and answers the first half honestly: a Vulkan optimal-tiled image
///   *is* a driver-chosen unspecified layout, which is what `_UNDEFINED` names.
/// * **The additional-data sizes are zero** because there is none. Their
///   alignments are one, not zero: alignment remains an unconditional input to
///   D3D12Core's packing arithmetic even for an empty block, and one is the
///   identity alignment that adds no padding.
/// * **The alignment is clamped.** `ResourceHeaps.md:1350`: it must be a power of
///   two and must never exceed 64 KiB unless `SampleDesc.Count > 1`. vkd3d
///   answers from Vulkan memory requirements and is under no such obligation.
fn allocation_info_from_engine(
    device10: &ID3D12Device10,
    desc1: &D3D12_RESOURCE_DESC1,
    a: &ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    visible_node_mask: u32,
) -> ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022 {
    let desc = resource_desc(desc1);
    // SAFETY: `desc` is a live local of exactly the struct the entry point
    // names, and the slice it is passed in is one element long.
    let info = unsafe { device10.GetResourceAllocationInfo(visible_node_mask, &[desc]) };

    let (size, alignment) = if info.SizeInBytes == ALLOCATION_INFO_FAILURE {
        L4_REFUSALS.resource_alloc_info_engine_failed.bump();
        let n = L4_REFUSALS.resource_alloc_info_engine_failed.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: GetResourceAllocationInfo refused type={} fmt={} {}x{}x{} mips={} \
                 samples={} -- reporting a zero-sized allocation, which the runtime will \
                 reject (x{n})",
                a.ResourceType,
                a.Format,
                a.Width,
                a.Height,
                a.DepthOrArraySize,
                a.MipLevels,
                a.SampleDesc.Count,
            );
        }
        (0, 0)
    } else {
        (info.SizeInBytes, info.Alignment)
    };

    let clamped = if a.SampleDesc.Count <= 1 && alignment > MAX_NON_MSAA_ALIGNMENT {
        L4_REFUSALS.resource_alignment_clamped.bump();
        let n = L4_REFUSALS.resource_alignment_clamped.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: engine alignment {alignment} exceeds the 64 KiB ceiling a non-multisampled \
                 resource may report -- clamped (x{n})"
            );
        }
        MAX_NON_MSAA_ALIGNMENT
    } else {
        alignment
    };

    if a.Layout == v::TL_UNDEFINED {
        L4_REFUSALS.resource_alloc_info_layout_chosen.bump();
    }

    ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022 {
        ResourceDataSize: size,
        AdditionalDataHeaderSize: 0,
        AdditionalDataSize: 0,
        // The clamp above bounds this at 64 KiB for every non-MSAA resource and
        // at the 4 MB MSAA tier otherwise, so the narrowing cannot lose bits;
        // `unwrap_or(0)` is the compile-time-total form of that, not a guess.
        ResourceDataAlignment: u32::try_from(clamped).unwrap_or(0),
        AdditionalDataHeaderAlignment: EMPTY_ADDITIONAL_DATA_ALIGNMENT,
        AdditionalDataAlignment: EMPTY_ADDITIONAL_DATA_ALIGNMENT,
        Layout: a.Layout,
        MipLevelSwizzleTransition: [0; 5],
        PlaneSliceSwizzleTransition: [0; 2],
    }
}

/// `pfnCheckResourceAllocationInfo`.
///
/// ⭐ **`AlignmentRestriction` is the API's `D3D12_RESOURCE_DESC::Alignment`.**
/// The DDI moved the field out of the struct — `D3D12DDIARG_CREATERESOURCE_0111`
/// has no `Alignment` while `D3D12_RESOURCE_DESC` does — and put it here as a
/// parameter, which is also why `pfnCreateHeapAndResource` has no such argument.
///
/// # ⛔ It is COUNTED AND IGNORED, so that there is exactly ONE derivation
///
/// Forwarding it would make this slot answer a different question from the one
/// the create arms ask. vkd3d honours `desc->Alignment` —
/// `d3d12_device_GetResourceAllocationInfo3` does
/// `requested = desc->Alignment; if (!desc->Alignment) requested =
/// d3d12_resource_desc_default_alignment(desc); Alignment = max(vk, requested)`
/// (`libs/vkd3d/device.c:9036-9041`) — while [`create_fused_heap_and_resource`] and
/// [`create_placed_or_reserved`] must pass 0, because the create DDI carries no
/// alignment for them to pass. Honouring it here therefore produces the exact
/// failure `check_existing_resource_allocation_info`'s doc says caching prevents:
/// an app asks with `D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT` (4 KiB), is told
/// 4 KiB, places the resource at a 4 KiB-aligned heap offset, and the create then
/// substitutes vkd3d's 64 KiB default and refuses the placement.
///
/// Reporting the driver's natural alignment instead is not merely
/// self-consistent, it is the API's own documented behaviour for a small-resource
/// request: a driver that cannot place at the smaller alignment answers with the
/// default, and the application is required to use the number it was given. The
/// ignoring is a number — `ResourceAlignmentRestrictionIgnored` — rather than
/// silence, so "we did not honour the app's alignment" is visible.
///
/// ⚠ `D3D12DDI_RESOURCE_OPTIMIZATION_FLAGS` has no counterpart on
/// `GetResourceAllocationInfo` and is a hint, not a requirement: an ignored hint
/// yields a correct answer that may be larger than an optimal one. Non-zero
/// arrivals are counted so "we ignored it" is a number.
///
/// # Safety
/// `p_resource` must be a live `D3D12DDIARG_CREATERESOURCE_0111`, and `out` must
/// address one writable `D3D12DDI_RESOURCE_ALLOCATION_INFO_0022`.
unsafe extern "C" fn check_resource_allocation_info(
    h_device: ddi12::D3D12DDI_HDEVICE,
    p_resource: *const ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    optimization_flags: ddi12::D3D12DDI_RESOURCE_OPTIMIZATION_FLAGS,
    alignment_restriction: ddi12::UINT32,
    visible_node_mask: ddi12::UINT,
    out: *mut ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022,
) {
    if out.is_null() {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        return;
    }
    // ⛔ A defined answer on every path, before anything can fail.
    // SAFETY: non-null per the check; the DDI declares it an out-parameter.
    unsafe {
        core::ptr::write_unaligned(
            out,
            ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022::default(),
        )
    };

    if p_resource.is_null() {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        return;
    }
    if optimization_flags != 0 {
        L4_REFUSALS.resource_optimization_flags_ignored.bump();
    }
    // ⭐ UP-4 splits `PRIMARY` out of that aggregate, the same move the file
    // already makes for `HeapPrimaryFlagDropped` against
    // `HeapFlagUnrepresentable` and for the same reason: the aggregate is graded
    // "non-zero OK" because ignoring an optimisation *hint* is benign, and the
    // PRIMARY bit is not a hint about layout, it is a declaration that the
    // swapchain primary is being sized. `KMD_IMPACT.md` §14a.3 names it as the
    // present path's second trigger, and this counter is the only instrument that
    // can say whether it arrives -- the measured logs show the aggregate at 1..3
    // in 101 of 150 runs, so *something* sets these bits and the aggregate cannot
    // say which.
    //
    // ⛔ Counted and NOT acted on. This slot has no `D3D12DDI_HRESOURCE`: it is a
    // sizing query about a resource that does not exist yet, so there is nothing
    // to record an identity against. See `create_committed_allocation` for why that
    // makes §14a.3's "use both signals" unimplementable as written.
    if optimization_flags & v::RESOURCE_OPT_PRIMARY != 0 {
        L4_REFUSALS.resource_optimization_primary.bump();
        let n = L4_REFUSALS.resource_optimization_primary.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: D3D12DDI_RESOURCE_OPTIMIZATION_FLAG_PRIMARY on CheckResourceAllocationInfo -- \
                 the swapchain primary is being SIZED here, and this slot has no resource handle to \
                 attach an identity to; {}x{} fmt={} (x{n})",
                // SAFETY: `p_resource` was null-checked above and is `_In_ CONST`.
                unsafe { (*p_resource).Width },
                unsafe { (*p_resource).Height },
                unsafe { (*p_resource).Format },
            );
        }
    }
    if alignment_restriction != 0 {
        L4_REFUSALS.resource_alignment_restriction_ignored.bump();
        let n = L4_REFUSALS.resource_alignment_restriction_ignored.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: AlignmentRestriction={alignment_restriction} ignored -- the create DDI \
                 carries no alignment, so honouring it here would answer a different question \
                 from the one the create asks (x{n})"
            );
        }
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_ CONST`.
    let a = unsafe { &*p_resource };

    // SAFETY: device-scope DDI; the borrow ends with this call.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L4_REFUSALS.resource_no_device);
        return;
    };
    let Some(device10) = engine_device10(dev) else {
        return;
    };
    // ⛔ `0`, exactly as both create arms pass -- see this function's doc. The
    // two Check slots and the create must call `resource_desc1` with the same
    // alignment argument or they are two derivations of one answer.
    // SAFETY: `a` is live per the check above.
    let Some(desc1) = (unsafe { resource_desc1(a) }) else {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        return;
    };

    let info = allocation_info_from_engine(&device10, &desc1, a, visible_node_mask);
    // SAFETY: as the first write above.
    unsafe { core::ptr::write_unaligned(out, info) };
}

/// `pfnGetTextureLayouts` for the unadvertised GUID-layout tier.
///
/// Helios reports `GUID_TEXTURE_LAYOUT_TIER_NOT_SUPPORTED`, so the only
/// truthful finite answer is an empty set. A non-null count is always written
/// before returning; no caller-provided GUID storage is touched.
unsafe extern "C" fn get_texture_layouts(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    _resource: *const ddi12::D3D12DDIARG_CREATERESOURCE_0111,
    _allowed_access: ddi12::D3D12DDI_BARRIER_ACCESS,
    _optimized_access: ddi12::D3D12DDI_BARRIER_ACCESS,
    num_guids: *mut ddi12::UINT,
    _guids: *mut ddi12::GUID,
) {
    if num_guids.is_null() {
        note_refusal(&L4_REFUSALS.texture_layout_query_bad_arg);
        return;
    }
    // SAFETY: the DDI declares `pNumGuids` writable; non-null was checked.
    unsafe { num_guids.write(0) };
    note_refusal(&L4_REFUSALS.texture_layout_query_empty);
}

/// `pfnCheckExistingResourceAllocationInfo`.
///
/// Answers from the resource's own state, which is the allocation info this
/// driver reported when it created the resource. Recomputing would be a second
/// derivation of one answer and the only way the two could disagree.
///
/// ⚠ The typedef's name carries an SDK typo that SDK 26100 still ships —
/// `PFND3D12DDI_CHECKEXISITINGRESOURCEALLOCATIONINFO_0022`, with `EXISITING`
/// transposed (`SPECS.md` §9.7, from `ResourceHeaps.md:2968`). The corrected
/// spelling does not compile.
///
/// # Safety
/// `h_resource` must be a live resource handle from
/// [`create_heap_and_resource`], and `out` must address one writable
/// `D3D12DDI_RESOURCE_ALLOCATION_INFO_0022`.
unsafe extern "C" fn check_existing_resource_allocation_info(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_resource: ddi12::D3D12DDI_HRESOURCE,
    out: *mut ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022,
) {
    if out.is_null() {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        return;
    }
    // SAFETY: non-null per the check; the DDI declares it an out-parameter.
    unsafe {
        core::ptr::write_unaligned(
            out,
            ddi12::D3D12DDI_RESOURCE_ALLOCATION_INFO_0022::default(),
        )
    };

    // SAFETY: the runtime passes a resource handle this driver wrote; the borrow
    // ends with this call.
    let Some(state) = (unsafe { resource_state(h_resource) }) else {
        note_refusal(&L4_REFUSALS.resource_handle_unresolved);
        return;
    };
    // SAFETY: as the first write above.
    unsafe { core::ptr::write_unaligned(out, state.alloc_info) };
}

/// `pfnCheckSubresourceInfo`.
///
/// ⭐ **This is where the runtime learns a subresource's offset and strides**,
/// and it exists precisely because Map is heap-scoped: *"Because Map is on the
/// heap, per-subresource offset and strides come from a separate DDI,
/// `pfnCheckSubresourceInfo`, which must be thread-safe and deterministic for a
/// given resource and subresource"* (`ResourceHeaps.md:1437`). Determinism is
/// why the description comes from the cached [`ResourceState::desc`] rather than
/// from a fresh `GetDesc`, and thread-safety is free: the state is immutable.
///
/// # ⛔ Two SCALAR queries, and no array sized from runtime data
///
/// `GetCopyableFootprints(desc, Subresource, 1, 0, ...)` answers `Offset = 0`,
/// because the footprint it computes is packed from the *first requested*
/// subresource — so the naive one-subresource query cannot give an offset. The
/// first draft therefore asked for `[0, Subresource]` and read the last entry,
/// which is correct but sizes an array from a runtime `UINT`. Bounding that
/// array turned a **legal** resource into a wrong answer: a `Texture2DArray`
/// with `DepthOrArraySize = 2048, MipLevels = 15` has 30 720 subresources,
/// every one of them legal and every query above the bound answered
/// `Offset = 0, RowStride = 0`, which the runtime would then act on.
///
/// The array is not needed. `pTotalBytes` is a **scalar**, and vkd3d's loop
/// (`libs/vkd3d/device.c:9270-9314`) ends each iteration with
/// `total = offset + size; offset = align(total,
/// D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT)`, writing `layouts[i].Offset =
/// base_offset + offset` at the top. So the offset of subresource *n* is exactly
/// `align(pTotalBytes over [0, n), D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT)` —
/// one scalar query and one round-up, with no allocation, no bound and no cliff.
/// The pitch and row count come from a second query of exactly ONE subresource.
///
/// ⛔ Both calls still obey the rule that cost this file a stack overflow in
/// draft: **every out-parameter of `GetCopyableFootprints` is an ARRAY of
/// `NumSubresources` entries** — `pLayouts`, `pNumRows`, `pRowSizeInBytes` all
/// are — so a scalar may only be passed where `NumSubresources` is 1. The range
/// query asks for none of them; the single query asks with a count of 1.
///
/// ⚠ **An out-of-range subresource is detected, not answered.** vkd3d pre-fills
/// every out-parameter with `0xff` and sets `total = ~0ull` before validating,
/// then `goto end`s on a bad range (`device.c:9234-9268`), so an untouched or
/// refused answer is `UINT_MAX` / `UINT64_MAX`. Both locals are seeded with the
/// same sentinel, so "the engine declined" reads the same whether the engine
/// wrote the sentinel or wrote nothing at all. That is the only case
/// `SubresourceInfoOutOfRange` now counts, and it is a case with no right answer
/// rather than a legal query this driver refused to compute.
///
/// ⚠ `DepthStride` is `RowPitch * NumRows` — the byte distance between depth
/// slices of one subresource, which is what the packed footprint defines and
/// what the DDI's field name asks for.
///
/// The swizzle offsets are zero: this driver reports no standard-swizzle layout,
/// so there is no swizzle pattern for a transition to be described against.
///
/// # Safety
/// `h_resource` must be a live resource handle from
/// [`create_heap_and_resource`], and `out` must address one writable
/// `D3D12DDI_SUBRESOURCE_INFO`.
unsafe extern "C" fn check_subresource_info(
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_resource: ddi12::D3D12DDI_HRESOURCE,
    subresource: ddi12::UINT,
    out: *mut ddi12::D3D12DDI_SUBRESOURCE_INFO,
) {
    if out.is_null() {
        note_refusal(&L4_REFUSALS.heap_resource_create_bad_arg);
        return;
    }
    // SAFETY: non-null per the check; the DDI declares it an out-parameter.
    unsafe { core::ptr::write_unaligned(out, ddi12::D3D12DDI_SUBRESOURCE_INFO::default()) };

    // SAFETY: the runtime passes a resource handle this driver wrote; the borrow
    // ends with this call.
    let Some(state) = (unsafe { resource_state(h_resource) }) else {
        note_refusal(&L4_REFUSALS.resource_handle_unresolved);
        return;
    };
    // SAFETY: device-scope DDI; the borrow ends with this call.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L4_REFUSALS.resource_no_device);
        return;
    };
    let Some(device10) = engine_device10(dev) else {
        return;
    };

    let desc = resource_desc(&state.desc);

    // Query 1 -- this subresource's own pitch and row count, and the range
    // check. Exactly ONE subresource, so each out-parameter is an array of one
    // and a single live local is the correct storage.
    let mut layout = D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
        Offset: 0,
        Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
            Format: DXGI_FORMAT(0),
            Width: 0,
            Height: 0,
            Depth: 0,
            RowPitch: FOOTPRINT_UNANSWERED_U32,
        },
    };
    let mut num_rows: u32 = FOOTPRINT_UNANSWERED_U32;
    // SAFETY: `desc`, `layout` and `num_rows` are live locals; `NumSubresources`
    // is 1, so each array the call writes has exactly one element. The two
    // outputs this call does not want are declined with `None`.
    unsafe {
        device10.GetCopyableFootprints(
            &desc,
            subresource,
            1,
            0,
            Some(core::ptr::from_mut(&mut layout)),
            Some(core::ptr::from_mut(&mut num_rows)),
            None,
            None,
        );
    }
    if num_rows == FOOTPRINT_UNANSWERED_U32 || layout.Footprint.RowPitch == FOOTPRINT_UNANSWERED_U32
    {
        L4_REFUSALS.subresource_info_out_of_range.bump();
        let n = L4_REFUSALS.subresource_info_out_of_range.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L4: CheckSubresourceInfo -- the engine did not describe subresource \
                 {subresource} of a {}x{} resource with {} mips and DepthOrArraySize {}, so the \
                 index is outside it; the zeroed answer stands because there is none (x{n})",
                state.desc.Width,
                state.desc.Height,
                state.desc.MipLevels,
                state.desc.DepthOrArraySize,
            );
        }
        return;
    }

    // Query 2 -- the byte extent of everything before it, which is where it
    // starts once rounded up to the texture-data placement alignment. Subresource
    // 0 starts at 0 and needs no query at all.
    let offset = if subresource == 0 {
        0
    } else {
        let mut total: u64 = FOOTPRINT_UNANSWERED_U64;
        // SAFETY: `desc` and `total` are live locals; `pTotalBytes` is a scalar
        // for any `NumSubresources`, and every array output is declined with
        // `None`, so nothing is written through a pointer this call does not
        // size.
        unsafe {
            device10.GetCopyableFootprints(
                &desc,
                0,
                subresource,
                0,
                None,
                None,
                None,
                Some(core::ptr::from_mut(&mut total)),
            );
        }
        // `checked_next_multiple_of` and not `next_multiple_of`: the latter
        // panics on overflow, and a panic in a DDI is a silent graphics
        // deadlock.
        let aligned = if total == FOOTPRINT_UNANSWERED_U64 {
            None
        } else {
            total.checked_next_multiple_of(u64::from(D3D12_TEXTURE_DATA_PLACEMENT_ALIGNMENT))
        };
        let Some(aligned) = aligned else {
            L4_REFUSALS.subresource_info_out_of_range.bump();
            let n = L4_REFUSALS.subresource_info_out_of_range.get();
            if n <= LOG_BUDGET {
                log_error!(
                    "L4: CheckSubresourceInfo -- the engine did not give a byte extent for \
                     subresources 0..{subresource}, so this one has no offset (x{n})"
                );
            }
            return;
        };
        aligned
    };

    let row_stride = u64::from(layout.Footprint.RowPitch);
    let info = ddi12::D3D12DDI_SUBRESOURCE_INFO {
        Offset: offset,
        RowStride: row_stride,
        DepthStride: row_stride.saturating_mul(u64::from(num_rows)),
        RowBytePreSwizzleOffset: 0,
        ColumnPreSwizzleOffset: 0,
        DepthPreSwizzleOffset: 0,
    };
    // SAFETY: as the first write above.
    unsafe { core::ptr::write_unaligned(out, info) };
}

/// `pfnCheckResourceVirtualAddress`.
///
/// ⭐ **This is where `ARCHITECTURE.md` §13's open question about GPU virtual
/// addresses becomes code.** `DDI_REFERENCE.md` §9.7 states the position: a
/// forwarding UMD returns vkd3d's `ID3D12Resource::GetGPUVirtualAddress`, which
/// is a Vulkan **buffer device address** in the *host* GPU's space obtained
/// through venus (`libs/vkd3d/resource.c:2656-2663`), and never calls
/// `pfnReserveGpuVirtualAddressCb` / `pfnMapGpuVirtualAddressCb`. Whether the
/// D3D12 runtime and its debug layer accept a VA space the driver never obtained
/// from the kernel is recorded there as **UNVERIFIED**; this is the site the
/// experiment settles.
///
/// ⚠ Zero is a correct answer for a texture: only buffers have a GPU VA in
/// D3D12, and vkd3d returns 0 for images. It is therefore *not* counted, and
/// only an unresolvable handle is.
///
/// # Safety
/// `h_resource` must be a live resource handle from
/// [`create_heap_and_resource`].
unsafe extern "C" fn check_resource_virtual_address(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_resource: ddi12::D3D12DDI_HRESOURCE,
) -> ddi12::D3D12DDI_GPU_VIRTUAL_ADDRESS {
    // SAFETY: the runtime passes a resource handle this driver wrote; the borrow
    // ends with this call.
    let Some(resource) = (unsafe { engine_resource(h_resource) }) else {
        note_refusal(&L4_REFUSALS.resource_handle_unresolved);
        return 0;
    };
    // SAFETY: `resource` is the engine resource this driver holds a reference to
    // for the object's whole life.
    unsafe { resource.GetGPUVirtualAddress() }
}

/// `pfnCheckResourceAllocationHandle` — the real `D3DKMT_HANDLE`, since UP-5.
///
/// The DDI asks for the kernel allocation behind a resource. ⭐ Since UP-5 this
/// driver has one for every committed resource: the `pfnAllocateCb` handle
/// [`create_committed_allocation`] minted, kept in the [`identity12`] registry
/// and looked up here by the engine resource behind the handle.
///
/// ⚠ **0 remains the correct answer for non-committed arms**, and it is not a
/// refusal: a placed resource is represented by its explicit heap, while reserved
/// resources are not supported. `DDI_REFERENCE.md` §9.7's *"kernel identity is
/// mandatory in at least three places, so 'pure passthrough with no
/// `pfnAllocateCb`' is not viable"* is discharged for committed resources.
///
/// ⚠⚠ **`ResourceAllocationHandleUnavailable` is RE-GRADED by this commit**, and
/// the old grading is written out so the change is not silent. It used to mean
/// *"this driver has no handles at all; expected non-zero if this slot is ever
/// called, and a reading above zero says the passthrough model has been reached"*.
/// It now means *"this slot was asked about a resource with no kernel allocation"*,
/// which is:
///
/// * **expected non-zero and benign** if the runtime asks about ordinary resources;
/// * **a real finding** only when read against `IdentityRecorded` — the interesting
///   quantity is now the *answered* case, and there is no counter for it because
///   `IdentityRecorded` already bounds it. ⛔ So this counter can no longer be read
///   as *"the model has been reached"*; that reading belonged to a driver with no
///   handles and it is now false.
///
/// ⚠ The parameter is spelled `D3D10DDI_HRESOURCE` — this is the one slot in the
/// header that uses the D3D10 name — and no conversion is needed or written:
/// `d3d12umddi.rs:47367` makes `D3D12DDI_HRESOURCE` a bindgen **alias** of it, so
/// they are the same type and `engine_resource` takes it directly.
///
/// # Safety
/// `h_resource`, when its `pDrvPrivate` is non-null, must be a resource handle
/// [`create_heap_and_resource`] returned `S_OK` for.
unsafe extern "C" fn check_resource_allocation_handle(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_resource: ddi12::D3D10DDI_HRESOURCE,
) -> ddi12::D3DKMT_HANDLE {
    // SAFETY: the runtime passes a resource handle this driver wrote; the borrow
    // ends with this call.
    match unsafe { allocation_identity(h_resource) } {
        Some(identity) => identity.h_allocation,
        None => {
            note_refusal(&L4_REFUSALS.resource_allocation_handle_unavailable);
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Install L4's 16 device-core slots.
///
/// Chain position: `QueueSlots` -> `ResourceSlots` on the device-core table.
///
/// Every assignment below is checked by the compiler against the bindgen
/// `Option<unsafe extern "C" fn(...)>` signature for that field, which is the
/// whole premise of the fan-out (`PARALLEL.md` §7).
pub(crate) fn install(
    mut filling: Filling<'_, DeviceCoreTable, stage::QueueSlots>,
) -> Filling<'_, DeviceCoreTable, stage::ResourceSlots> {
    let table = filling.table();

    // (g) heaps, resources, residency -- 11
    table.pfnMapHeap = Some(map_heap);
    table.pfnUnmapHeap = Some(unmap_heap);
    table.pfnCalcPrivateHeapAndResourceSizes = Some(calc_private_heap_and_resource_sizes);
    table.pfnCreateHeapAndResource = Some(create_heap_and_resource);
    table.pfnDestroyHeapAndResource = Some(destroy_heap_and_resource);
    table.pfnCalcPrivateOpenedHeapAndResourceSizes =
        Some(calc_private_opened_heap_and_resource_sizes);
    table.pfnOpenHeapAndResource = Some(open_heap_and_resource);
    table.pfnMakeResident = Some(make_resident);
    table.pfnEvict = Some(evict);
    table.pfnOfferResources = Some(offer_resources);
    table.pfnReclaimResources = Some(reclaim_resources);

    // (h) resource introspection -- 5
    table.pfnCheckResourceVirtualAddress = Some(check_resource_virtual_address);
    table.pfnCheckResourceAllocationInfo = Some(check_resource_allocation_info);
    table.pfnCheckSubresourceInfo = Some(check_subresource_info);
    table.pfnCheckExistingResourceAllocationInfo = Some(check_existing_resource_allocation_info);
    table.pfnCheckResourceAllocationHandle = Some(check_resource_allocation_handle);
    table.pfnGetTextureLayouts = Some(get_texture_layouts);

    filling.advance()
}

// ---------------------------------------------------------------------------
// Refusal counters
// ---------------------------------------------------------------------------

/// L4's refusal counters.
///
/// ⛔ **Append only.** Counter order inside a set, and set order in
/// `lib.rs`'s `UMD12_REFUSAL_SETS`, are both the evidence contract:
/// `D3D12 DDI refusals:` lines get diffed across builds.
struct L4Refusals {
    /// `pfnCalcPrivateHeapAndResourceSizes` with neither a heap nor a resource
    /// description — the illegal fourth row of the arm table. Expected 0. A size
    /// call cannot refuse (it returns a struct), so this counter is that site's
    /// only channel.
    heap_resource_calc_bad_arg: RefusalCounter,
    /// A create or introspection slot was handed a null out-pointer, a null
    /// argument struct, an unknown `D3D12DDI_RESOURCE_TYPE`, or a private block
    /// the paired sizing call said would exist and did not. Expected 0.
    heap_resource_create_bad_arg: RefusalCounter,
    /// `ID3D12Device::CreateHeap` refused. Expected 0; a hit is an
    /// out-of-memory or an unsupported heap shape and the line above it says
    /// which.
    heap_create_engine_failed: RefusalCounter,
    /// A `D3D12DDI_MEMORY_POOL` or `D3D12DDI_CPU_PAGE_PROPERTY` this driver does
    /// not recognise. ⛔ Expected 0 — both enums are closed and two-or-three
    /// valued — and a hit means the header moved under the cached bindings.
    heap_property_unrepresentable: RefusalCounter,
    /// `D3D12DDI_HEAP_FLAGS` bits with no `D3D12_HEAP_FLAGS` counterpart, so
    /// dropped: `COHERENT_SYSTEMWIDE`, `_0041_DENY_L0_DEMOTION`.
    /// ⚠ May legitimately be non-zero; it reads as a work list, not a fault —
    /// which is only true because `PRIMARY` is counted separately by
    /// `HeapPrimaryFlagDropped` and is not in this bucket.
    heap_flag_unrepresentable: RefusalCounter,
    /// A heap was created with `CreationNodeMask` or `VisibleNodeMask` other
    /// than 1 and the value was forwarded unchanged. ⛔ Expected 0 on this
    /// single-node guest; a hit is the same multi-adapter assumption
    /// `ARCHITECTURE.md` §13 UNVERIFIED-11 records, reached for real.
    heap_node_mask_unexpected: RefusalCounter,
    /// A standalone heap got no whole-heap span buffer, so it can be neither
    /// mapped nor asked for a GPU virtual address. ⚠ **Expected non-zero**: a
    /// heap whose flags deny buffers cannot have one by construction, and those
    /// heaps are not CPU-mappable anyway.
    heap_span_buffer_unavailable: RefusalCounter,
    /// `CreateCommittedResource3` or `CreatePlacedResource2` refused. Expected
    /// 0 outside genuine out-of-memory; the line above each hit carries the full
    /// description so the refusal can be attributed to a field.
    resource_create_engine_failed: RefusalCounter,
    /// A reserved (tiled) resource was refused — the resource-args-only arm with
    /// **neither** a `ReuseBufferGPUVA` parent nor a usable `hHeap`, which is
    /// `ResourceHeaps.md:1204`'s definition of reserved. ⛔ Expected 0: `caps12`
    /// reports `TiledResourcesTier = NOT_SUPPORTED`, so the runtime should never
    /// ask. A hit is a caps inconsistency, not something this slot can fix.
    resource_reserved_refused: RefusalCounter,
    /// A placed resource named a `ReuseBufferGPUVA` parent this driver could not
    /// resolve to an engine heap — most likely a committed resource, whose heap
    /// is implicit and has no `ID3D12Heap`. Expected 0. ⚠ It is a *census of the
    /// fallback*, not a failure: the `hHeap` path runs next, so non-zero **with
    /// placed creates still succeeding** says the runtime does supply `hHeap` on
    /// the placed arm and the `ReuseBufferGPUVA` model is not how it places;
    /// non-zero **with creates failing** says neither channel works.
    resource_placement_unresolved: RefusalCounter,
    /// A non-zero `CreateAtVirtualAddress` arrived and was ignored. ⛔ Expected
    /// 0, and a hit is a real finding: `DDI_REFERENCE.md` §9.7 records that
    /// `RecreateAt` is gated behind DDI 0111, which SDK 10.0.26100.0 does not
    /// define, so arrival means that build assumption is wrong.
    resource_create_at_virtual_address: RefusalCounter,
    /// A non-null `pRowMajorLayout` arrived. The API has no way to dictate a
    /// row pitch to `CreateCommittedResource3`, so the engine's own layout is
    /// used and the requested one is dropped. Expected 0 while nothing asks for
    /// a dictated row-major layout.
    resource_row_major_layout_ignored: RefusalCounter,
    /// A `D3D12DDI_TEXTURE_LAYOUT` with no API counterpart —
    /// `DEVICE_DEPENDENT_SWIZZLE_0`. Expected 0.
    resource_layout_unrepresentable: RefusalCounter,
    /// `D3D12DDI_RESOURCE_FLAGS_0003` bits with no `D3D12_RESOURCE_FLAGS`
    /// counterpart, so dropped: `CONTENT_PROTECTION`, the two
    /// `ONLY_*_TEXTURE_PLACEMENT` hints, `4MB_ALIGNED`, `SAMPLER_FEEDBACK`.
    /// ⚠ May legitimately be non-zero.
    resource_flag_unrepresentable: RefusalCounter,
    /// An initial barrier layout was changed: a buffer's forced to `UNDEFINED`
    /// (the only layout a buffer may carry), or one of the DDI-only `LEGACY_*`
    /// values mapped onto the nearest API layout. ⚠ Expected non-zero as soon as
    /// any application creates a buffer through the legacy create path.
    resource_barrier_layout_coerced: RefusalCounter,
    /// A castable-format list longer than this driver's bound was dropped rather
    /// than read. Expected 0 — DXGI defines fewer than 200 formats.
    castable_formats_dropped: RefusalCounter,
    /// A protected-resource session handle arrived on a create or sizing call
    /// and was not forwarded. ⛔ Expected 0: this driver reports no
    /// protected-resource support.
    protected_session_ignored: RefusalCounter,
    /// The engine device does not expose `ID3D12Device10`, so no resource can be
    /// created: the DDI carries a barrier layout, a sampler-feedback mip region
    /// and a castable-format list that only that revision's entry points accept.
    /// ⛔ Expected 0 — vkd3d's `QueryInterface` accepts `IID_ID3D12Device10`
    /// (`libs/vkd3d/device.c:4647`) — and a hit means the engine was swapped.
    resource_device10_unavailable: RefusalCounter,
    /// A device-scope slot could not reach the engine: the `hDevice` did not
    /// resolve, or the bridge carries no `ID3D12Device`. Expected 0 — these are
    /// device-scope DDIs and a device exists by construction.
    resource_no_device: RefusalCounter,
    /// A DDI arrived with a heap or resource handle this driver never wrote, or
    /// whose object had already been destroyed. Expected 0.
    resource_handle_unresolved: RefusalCounter,
    /// `pfnMapHeap` on a heap with no mappable anchor. Paired with
    /// `HeapSpanBufferUnavailable`: that counter says a heap has none, this one
    /// says something tried to map it anyway.
    map_heap_no_anchor: RefusalCounter,
    /// `ID3D12Resource::Map` on a heap's anchor refused. ⚠ Expected non-zero for
    /// committed textures whose layout is not `ROW_MAJOR`: D3D12 does not allow
    /// mapping those, and the heap's own base address is not obtainable through
    /// them.
    map_heap_engine_failed: RefusalCounter,
    /// How many heaps were mapped. ⚠ Not a refusal — it is the evidence that the
    /// heap-scoped map path is being exercised at all, which the per-slot noop
    /// counter used to carry and which implementing the slot would otherwise
    /// delete.
    map_heap_calls: RefusalCounter,
    /// `ID3D12Device::GetResourceAllocationInfo` answered its `UINT64_MAX`
    /// failure sentinel, so a zero-sized allocation was reported. ⛔ Expected 0;
    /// the runtime rejects a zero `ResourceDataSize` for a resource whose layout
    /// it knows (strings:82), so a hit turns into a failed create with a
    /// diagnosable cause.
    resource_alloc_info_engine_failed: RefusalCounter,
    /// The engine's alignment exceeded the 64 KiB ceiling a non-multisampled
    /// resource may report (`ResourceHeaps.md:1350`) and was clamped. Expected 0
    /// while vkd3d answers from ordinary Vulkan memory requirements.
    resource_alignment_clamped: RefusalCounter,
    /// The runtime asked for allocation info with `Layout = _UNDEFINED`, leaving
    /// the choice to the driver, and this driver echoed `_UNDEFINED` back — the
    /// honest name for the Vulkan optimal-tiled layout the engine will pick.
    /// ⚠ **Expected non-zero**, and it is a census rather than a fault.
    resource_alloc_info_layout_chosen: RefusalCounter,
    /// A non-zero `D3D12DDI_RESOURCE_OPTIMIZATION_FLAGS` was ignored.
    /// `GetResourceAllocationInfo` has no counterpart parameter, and the flags
    /// are a hint: ignoring one yields a correct answer that may be larger than
    /// an optimal one. ⚠ May legitimately be non-zero — measured at 1..3 in 101
    /// of the 150 logged runs under `tmp/`.
    ///
    /// ⚠ **RE-GRADED at UP-4.** "Non-zero OK" is only defensible now that
    /// `PRIMARY` is counted separately by `ResourceOptimizationPrimary`: this
    /// aggregate absorbs `SHADER_RESOURCE`, `UNORDERED_ACCESS` and
    /// `DETERMINISTIC`, which are layout hints, and the PRIMARY bit is a
    /// declaration about what the resource IS. Both counters still move for a
    /// PRIMARY arrival — this one is deliberately the total, so its measured
    /// history stays comparable across builds.
    resource_optimization_flags_ignored: RefusalCounter,
    /// `pfnCheckSubresourceInfo` was asked about a subresource the engine would
    /// not describe — i.e. an index outside the resource, which
    /// `GetCopyableFootprints` reports by leaving its `0xff` pre-fill in place.
    /// Expected 0. ⚠ It is **not** a driver-imposed bound: every legal
    /// subresource of a legal resource is answered, including the 30 720 of a
    /// 2 048-slice 15-mip Texture2DArray. A hit is a question with no right
    /// answer, and the zeroed out-struct stands because there is none.
    subresource_info_out_of_range: RefusalCounter,
    /// `pfnCheckResourceAllocationHandle` answered 0 for a resource with **no kernel
    /// allocation**.
    ///
    /// ⚠⚠ **RE-GRADED at UP-5, and the old grading is now FALSE.** It read: *"answered
    /// 0 because this driver mints no kernel allocations. Expected non-zero if the
    /// slot is called at all, and driving it to zero is what closing
    /// `DDI_REFERENCE.md` §9.7's kernel-identity gap would mean."* This driver does
    /// mint kernel allocations now, for every **committed** resource whose HWA2
    /// create succeeded, and the slot answers with the real handle for those.
    ///
    /// ⇒ It is **expected non-zero and benign**: the runtime is free to ask about any
    /// resource, and an ordinary D3D12 texture has no kernel allocation because its
    /// memory is the venus ICD's. ⛔ Driving it to zero is no longer a goal and would
    /// mean the runtime stopped asking. The quantity that matters is the *answered*
    /// case, which `IdentityRecorded` already bounds — there is deliberately no second
    /// counter for it.
    resource_allocation_handle_unavailable: RefusalCounter,
    /// `pfnCalcPrivateOpenedHeapAndResourceSizes` answered {0, 0} because the
    /// paired open refuses. Expected 0 until something opens a shared resource.
    open_heap_calc_refused: RefusalCounter,
    /// `pfnOpenHeapAndResource` refused. ⛔ This is `DECISIONS.md` D3c's counter:
    /// non-zero means DWM, or an application, tried to open a resource across
    /// the D3D11/D3D12 DDI boundary and this driver could not. See the handler's
    /// doc comment for the three things that stand in the way.
    open_heap_and_resource_refused: RefusalCounter,
    /// `pfnMakeResident` calls served. ⚠ Not a refusal. `D12-G7` measured
    /// **two** inside one `D3D12CreateDevice`, so a reading near 2 per device is
    /// the expected shape and a reading of 0 means this slot is not being
    /// reached at all.
    make_resident_calls: RefusalCounter,
    /// `pfnEvict` calls served. ⚠ Not a refusal, same reason.
    evict_calls: RefusalCounter,
    /// Offers accepted with the contents retained. ⚠ Not a refusal: keeping the
    /// contents is a legal answer to every offer priority, and
    /// `pfnReclaimResources` then truthfully reports nothing was discarded.
    offer_resources_accepted: RefusalCounter,
    /// `pfnReclaimResources` calls served, each of which reported every object
    /// as not discarded. ⚠ Not a refusal.
    reclaim_resources_calls: RefusalCounter,
    /// A residency DDI arrived with a null argument struct. Expected 0 — all
    /// four declare their argument non-optional.
    residency_bad_arg: RefusalCounter,
    /// ⛔ **`D3D12DDI_HEAP_FLAG_PRIMARY` arrived and was dropped from the
    /// `D3D12_HEAP_FLAGS` word this driver hands the engine**, because the API
    /// has no counterpart bit. Split out of `HeapFlagUnrepresentable`
    /// deliberately: that counter is graded "non-zero OK" because the bits it
    /// absorbs (`COHERENT_SYSTEMWIDE`, `_0041_DENY_L0_DEMOTION`) are benign, and
    /// this one is not. It is the only channel the DDI has for declaring a
    /// primary (`SPECS.md` §9.7, `ResourceHeaps.md:897`).
    ///
    /// ⚠⚠ **RE-GRADED TWICE, and this is the second time.** At UP-4 it meant *"a
    /// primary reached this driver and was demoted"* on every arm. At UP-5 the
    /// committed arm stopped dropping the flag — it translates it into
    /// `HELIOS_HEAP_FLAG_VENUS_EXPORT` and counts `HeapPrimaryVenusExport` — so this
    /// counter is now reachable **only from the heap-only arm**, where the
    /// declaration cannot be honoured because there is no resource to give an
    /// allocation to.
    ///
    /// ⇒ It is therefore no longer the census of primaries; `HeapPrimaryVenusExport`
    /// is. It is expected **0**, and non-zero means the same thing
    /// `HeapPrimaryWithoutResource` means — the two now fire together and either one
    /// alone would be a finding about this file rather than about the runtime.
    heap_primary_flag_dropped: RefusalCounter,
    /// A non-zero `AlignmentRestriction` arrived on
    /// `pfnCheckResourceAllocationInfo` and was NOT forwarded to the engine, so
    /// the alignment reported is the driver's natural one. ⚠ **Expected non-zero
    /// as soon as anything asks for `D3D12_SMALL_RESOURCE_PLACEMENT_ALIGNMENT`**;
    /// it is a census, not a fault. See the handler for why honouring it would
    /// make the two derivations of one answer disagree.
    resource_alignment_restriction_ignored: RefusalCounter,
    /// `pfnDestroyHeapAndResource` was told by the resource it just destroyed
    /// that the paired create HAD asked for a heap block, and then found no
    /// `Box<HeapState>` behind `hHeap` to reclaim.
    ///
    /// ⛔ **Expected 0.** A hit means an `ID3D12Heap` and its map anchor are now
    /// unreachable — a leak of engine memory, not a crash, which is why the
    /// destroy is allowed to reach it at all.
    ///
    /// ⚠ It is a separate counter from `ResourceHandleUnresolved` and not an
    /// extra bump of it, because on this path `seen` is already `true`: the
    /// resource resolved. The aggregate could never have shown a heap that went
    /// unreclaimed, which is exactly the shape the review pass found in the arm
    /// this counter was added with.
    heap_block_unreclaimed: RefusalCounter,
    // -- UP-4, the resource -> kernel-allocation-identity table -------------
    /// Entries made in the `forward12::identity12` registry, i.e. committed
    /// resources whose **HWA2 create succeeded end to end**. ⚠ **Not a refusal — the
    /// census**, and since UP-5 it is also the count of live-or-once-live WDDM
    /// allocations this driver owns.
    ///
    /// ⚠ **Re-graded twice.** Its partner is `CommittedVenusExport` (every committed
    /// create this driver *attempted*), and the two are deliberately **not**
    /// equal-by-construction: every refusal in `create_committed_allocation` widens
    /// the gap, and the gap is the interesting quantity.
    /// `CommittedVenusExport - IdentityRecorded` is the number of committed resources
    /// that got no kernel allocation, and the counter that says why is one of
    /// `IdentityVkMemoryUnresolved`, `IdentityOffsetNonZero`,
    /// `Hwa2GeometryUnrepresentable`, `Hwa2CreateInputInvalid`, `AllocateCb*`,
    /// `Hwa2WriteBackAbsent`, `Hwa2CreateOutputInvalid`, `Hwa2EchoMismatch` or
    /// `IdentityRegistryAllocFailed`.
    ///
    /// ⚠ The word "adopted" and the counters `IdentityResIdShared` and
    /// `OwnershipTransferFailed` are struck from this list rather than reworded.
    /// Both counters survive as retired append-only slots that are **permanently 0**
    /// — the first had no id left to collide, the second's bridge call is deleted —
    /// because there is no adoption any more (K4-CONTRACT §5), so neither can widen
    /// this gap and reading them as a cause would send a reader looking for a
    /// mechanism that is gone.
    identity_recorded: RefusalCounter,
    /// ⛔ Retired append-only telemetry slot. The fixed 64-entry identity table no
    /// longer exists; the dynamic registry reports allocation pressure through
    /// `IdentityRegistryAllocFailed`. This counter remains in its historical
    /// position so refusal-summary field order does not change; expected 0.
    identity_table_full: RefusalCounter,
    /// A recorded identity **overwrote** one already held for the same engine
    /// resource address. ⛔ Expected 0, and non-zero is a lifetime defect, not a
    /// benign duplicate: it means a `pfnDestroyHeapAndResource` did not retire
    /// its entry and a recycled COM address then collided. The overwrite is the
    /// safe direction (the live resource wins); this counter is how the missed
    /// removal stops being silent.
    identity_replaced: RefusalCounter,
    /// Identities retired by `pfnDestroyHeapAndResource`. ⚠ Not a refusal.
    /// `IdentityRecorded - IdentityRemoved` is the live-entry count, so a
    /// difference that grows across a run is a leak with no extra instrument
    /// needed. It counts only removals that found an entry, so non-committed
    /// resource destroys do not move it.
    identity_removed: RefusalCounter,
    /// ⚠⚠ **RE-GRADED at UP-5, and its bump site moved.** It used to count *entries
    /// recorded with `vk_memory == 0`*, and was expected to equal `IdentityRecorded`
    /// because no bridge accessor existed. Both halves are now false: the accessor
    /// exists, and an entry with a zero half is no longer representable — the table's
    /// invariant is that an entry exists **iff** a WDDM allocation exists.
    ///
    /// It now counts **a committed create refused because the ENGINE could not name
    /// the memory the resource is bound to** — a zero `vk_memory` or a zero
    /// `memory_size`, i.e. `IdentityStatus::BadArg`, `NoInterop` or `EngineRefused`.
    /// ⛔ Expected 0, and non-zero points at the vkd3d fork or the interop interface,
    /// not at the ICD. ⚠ **This is now the ONLY identity failure that refuses a
    /// create**: HWA2's `byte_size` is the bound memory's extent and only the engine
    /// knows it, whereas nothing in the record needs the ICD at all.
    identity_vk_memory_unresolved: RefusalCounter,
    /// ⚠⚠ **RE-GRADED AGAIN by the HPS2 retirement (K4), from a refusal to a
    /// census**, and both gradings are written out so neither change is silent. At
    /// UP-4 it counted entries recorded with `vk_memory == 0`; at UP-5 it counted **a
    /// committed create refused because the memory had no venus resource**. It now
    /// counts a create that simply *proceeded* without one.
    ///
    /// ⛔ Nothing in HWA2 names a host resource (§10.3), so a memory the ICD cannot
    /// export no longer blocks anything at create time — it blocks the *present*,
    /// where `PresentIdentityNoResourceId` names mesa unit A3. This counter still says
    /// the export chain did not engage, which stays worth knowing while
    /// `HELIOS_HEAP_FLAG_VENUS_EXPORT` is still in the tree; read it against
    /// `CommittedVenusExport` and `Hwa2VenusResIdDropped`, which is its complement —
    /// the two partition every committed create.
    identity_venus_unresolved: RefusalCounter,
    /// `D3D12DDI_HEAP_FLAG_PRIMARY` arrived on the **heap-only** arm, with no
    /// resource description. ⛔ Expected 0: `ResourceHeaps.md:897` says the flag
    /// obliges the driver to create a resource simultaneously with the heap, and
    /// the committed-resource HWA2 create path depends on that
    /// obligation holding. A hit means a primary reached this driver with no
    /// `ID3D12Resource` to key an identity on, so it went unrecorded.
    heap_primary_without_resource: RefusalCounter,
    /// `D3D12DDI_RESOURCE_OPTIMIZATION_FLAG_PRIMARY` arrived on
    /// `pfnCheckResourceAllocationInfo`. ⚠ Not a refusal — it is
    /// `KMD_IMPACT.md` §14a.3's *"second signal"*, measured at the only slot that
    /// carries it, and split out of `ResourceOptimizationFlagsIgnored` for the
    /// same reason `HeapPrimaryFlagDropped` is split out of
    /// `HeapFlagUnrepresentable`: the aggregate is graded "non-zero OK" because
    /// ignoring a layout hint is benign, and this bit is not a layout hint.
    ///
    /// ⚠ Historical primary-signal telemetry. Create-time allocation identity no
    /// longer depends on this bit: the deployed runtime sends no primary signal on
    /// the committed create, and this query occurs too early to associate the
    /// eventual `hRTResource` without heuristic description matching.
    resource_optimization_primary: RefusalCounter,
    // -- UP-5, the kernel allocation -----------------------------------------
    /// `D3D12DDI_HEAP_FLAG_PRIMARY` arrived on the **committed** arm and was
    /// translated into [`HELIOS_HEAP_FLAG_VENUS_EXPORT`]. ⚠ Not a refusal — the
    /// historical census of committed resources the runtime additionally declares
    /// primary. `CommittedVenusExport`, not this counter, is the adoption
    /// denominator now.
    heap_primary_venus_export: RefusalCounter,
    /// A committed resource's memory came back with a non-zero **offset**, i.e. the engine
    /// suballocated it. ⛔ Expected 0, and non-zero means the fork's dedicated
    /// arm did not engage — the create is refused.
    ///
    /// ⚠ **The reason changed with the record and is restated rather than left
    /// stale.** It used to be that one venus resource id shared between D3D12
    /// resources breaks the *adopt* model. There is no adoption
    /// (`docs/retirement/K4-CONTRACT.md` §5). The reason now is the descriptor's own
    /// shape: `HeliosWddmAllocationDescV2::byte_size` is the whole bound
    /// `VkDeviceMemory` and every plane record is bounded against it, so a resource
    /// that does not own its extent cannot be described at all.
    identity_offset_nonzero: RefusalCounter,
    /// ⛔ **Retired append-only telemetry slot: PERMANENTLY 0 as of the HPS2
    /// retirement.** It was the rotation-collapse detector — two live resources
    /// claiming one `venus_res_id` — and it died with the id: `identity12` holds no
    /// host resource id and §10.3 forbids it keeping one, so there is nothing left to
    /// collide. It remains in its historical position so refusal-summary field order
    /// does not change.
    ///
    /// ⚠⚠ **THE SUCCESSOR DOES NOT EXIST YET, and it is recorded here rather than
    /// asserted away.** `identity12`'s module doc says the property "is now settled in
    /// the kernel's own allocation objects plus mesa unit **A3**". Measured at HEAD,
    /// the A3 half is future work and the kernel half is not there at all:
    ///
    /// * `kmd_render`'s `AllocationContext::resource_id` is the resid's sole legal
    ///   home, but those contexts are per-`hAllocation` boxes with no adapter-wide
    ///   index, and the allocation generation is explicitly *"never an identity lookup
    ///   key"* (`adapter/allocation_object.rs`). Nothing scans for a duplicate.
    /// * The one adapter-wide resid index that does exist — `create_allocation.rs`'s
    ///   `SCANOUT_ALLOCS` — does **not** detect duplicates.
    ///   `register_scanout_allocation` CASes the first *free* slot without checking
    ///   whether the resid is already registered, so two allocations sharing one resid
    ///   take two slots;
    ///   `scanout_allocation_for_resource` returns whichever comes first in array
    ///   order; and `unregister_scanout_allocation` clears the first match, which can
    ///   be the *other* allocation's slot.
    ///
    /// ⭐ **Unreachable today**, which is why this is a recorded gap and not a live
    /// defect: the KMD mints one venus resource per allocation (`blob.res_id`, from
    /// its own create), so resids are unique per allocation by construction, and this
    /// driver's `IdentityOffsetNonZero` refuses the suballocated committed resource
    /// that is the usual route to a shared id. ⛔ **It becomes reachable exactly when
    /// mesa unit A3 lands** and the resid starts arriving from
    /// `HeliosNativeRenderPatch` instead of a KMD-local create. At that point the
    /// failure is worse than the lost surface the deleted detector caught: a destroy
    /// can clear the *surviving* allocation's slot and leave a freed
    /// `AllocationContext*` published under the id for `DxgkDdiPresent` to read.
    /// `destroy_allocation_ctx`'s comment — *"after this no Present can resolve this
    /// resource id to a handle whose Box is about to be dropped"* — is a claim that
    /// holds only while resids are unique.
    ///
    /// ⇒ **CROSS-LANE**, against the A3 unit and `ddi/create_allocation.rs`: A3 must
    /// not land without a duplicate-resid check in `register_scanout_allocation` and a
    /// named counter for it. This slot cannot be that counter — it is a UMD counter
    /// and the collision is no longer visible from user mode at all.
    identity_res_id_shared: RefusalCounter,
    /// A committed allocation was recorded with `ctx_id == 0`, because the
    /// instance-scoped venus context id was unavailable. ⚠ **Not a refusal and not a
    /// defect.** The value used to travel into `HeliosWddmOpenIdentity::ctx_id`
    /// (*"diagnostic only"* by that retired record's own doc); HWA2 has no context
    /// field at all, so it now stays inside this process. Non-zero means the ICD is
    /// absent or predates `helios_venus_instance_ctx_id`.
    identity_ctx_id_unavailable: RefusalCounter,
    /// vkd3d's `memory_size` and the ICD's `venus_alloc_size` **disagreed** for one
    /// `VkDeviceMemory`. ⛔ Expected 0: they are two readings of one
    /// `VkMemoryAllocateInfo::allocationSize`. Non-zero means one of them describes a
    /// different object, and a cross-process import is exact-size — so an opener will
    /// reject the surface.
    identity_alloc_size_disagreement: RefusalCounter,
    /// `pfnAllocateCb` was absent from the corelayer table, or the table itself was
    /// null. ⛔ Expected 0 — `create_device` refuses a null `p12UMCallbacks` — and a
    /// hit means no committed resource can get a kernel allocation at all.
    allocate_cb_missing: RefusalCounter,
    /// `pfnAllocateCb` returned a failure HRESULT. ⛔ Expected 0. The HRESULT is
    /// dxgkrnl's own and is logged; it is the channel that would report an HWA2
    /// descriptor the KMD's `validate_create_input` rejected, a rejected flag
    /// combination, or a kernel that refused the create. ⚠ Read it against
    /// `Hwa2CreateInputInvalid`: a hit *there* means this driver caught its own
    /// malformed descriptor first, and a hit here with that at 0 means the KMD applies
    /// a rule this driver's copy of the validator does not.
    allocate_cb_failed: RefusalCounter,
    /// `pfnAllocateCb` succeeded and left `hAllocation == 0`. ⛔ Expected 0: a
    /// runtime contract violation, refused here rather than allowed to become a 0
    /// in `pfnPresent`'s `BroadcastSrcAllocation[0]`.
    allocate_cb_no_handle: RefusalCounter,
    /// ⛔ **Retired append-only telemetry slot: PERMANENTLY 0 as of the HPS2
    /// retirement.** It counted the venus ICD refusing to hand a host resource's
    /// ownership to the WDDM allocation that had just adopted it. There is no
    /// adoption: the KMD creates the backing, the ICD keeps its own resource, and
    /// neither can double-unref the other's. Kept in position so refusal-summary field
    /// order does not change.
    ownership_transfer_failed: RefusalCounter,
    /// `pfnDeallocateCb` was unreachable at destroy — absent from the table, or the
    /// device did not resolve. ⛔ Expected 0, and every hit is **one leaked WDDM
    /// allocation**, unreachable until process exit.
    deallocate_cb_missing: RefusalCounter,
    /// `pfnDeallocateCb` returned a failure HRESULT. ⛔ Expected 0; same leak.
    deallocate_cb_failed: RefusalCounter,
    /// ⚠⚠ **RE-GRADED by the HPS2 retirement, and its expected value flipped.** It
    /// used to count the kernel writing back over the retired trailer's `meta.pitch` /
    /// `venus_alloc_size` / `memory_type_index`, with an expected value of **UNKNOWN**
    /// — whether dxgkrnl propagated the KMD's create-time private write to the UMD's
    /// buffer at all was established nowhere in the doc set.
    ///
    /// It now counts a create whose HWA2 write-back **arrived and validated**: a
    /// nonzero KMD-assigned `allocation_generation` in a descriptor that passed
    /// `validate_create_output`. ⛔ **Expected to equal `IdentityRecorded`**, because
    /// K4-CONTRACT §1.1 makes the write-back the output half of the create contract
    /// rather than an optimisation. Its complement is `Hwa2WriteBackAbsent`, and the
    /// two answer the old open question with a number.
    alloc_private_written_back: RefusalCounter,
    /// A `D3D12DDI_RESOURCE_FLAGS` word carried a bit
    /// [`hwa2_bind_flags`]' translation table does not know. ⚠ Its wire name still
    /// says *"Meta"* after the record it was named for was retired, and that is
    /// deliberate: `D3D12 DDI refusals:` lines are diffed across builds, so a counter
    /// name is part of the evidence contract and renaming it would break every
    /// cross-build comparison.
    ///
    /// ⛔ **Expected 0, and the only way it can move is a header revision adding an
    /// enumerator** — the mask it is checked against is built out of the `v::RES_*`
    /// aliases, so it cannot go stale silently. A hit means the allocation's HWA2
    /// `bind_flags` may be missing a usage an opener needs, which surfaces as an
    /// import failure or a surface DWM cannot sample, never as an error at create.
    meta_bind_flag_unknown: RefusalCounter,
    /// Committed resources made venus-exportable and dedicated so their implicit
    /// heaps can receive WDDM allocation identities. Not a refusal; this is the
    /// create-time census for the class the DDI may later ask dxgkrnl to share.
    committed_venus_export: RefusalCounter,
    /// The dynamic allocation-identity registry could not reserve another entry.
    /// Expected 0; the just-created WDDM allocation is rolled back.
    identity_registry_alloc_failed: RefusalCounter,
    // -- K4, the HPS2 retirement: HWA2 replaces the AllocPrivate+Meta pair --------
    /// A committed create whose shape has **no HWA2 spelling**, refused rather than
    /// approximated: a `TL_64KB_TILE_STANDARD_SWIZZLE` layout (§10.3's swizzle enum
    /// has `LINEAR` and `OPAQUE_OPTIMAL` and nothing else), an engine that declined a
    /// row pitch, a dimension above `u32::MAX`, or a subresource extent that does not
    /// fit inside the bound memory.
    ///
    /// ⛔ Expected 0 on the workloads this driver has seen, and it is a **refusal**
    /// because the KMD echoes every field it validates: a field this driver rounded
    /// off would make the descriptor disagree with the resource vkd3d built.
    hwa2_geometry_unrepresentable: RefusalCounter,
    /// The HWA2 create-input descriptor this driver built failed
    /// `validate_create_input` **before** the kernel saw it. ⛔ Expected 0, and a hit
    /// is a bug in `hwa2_create_input`, not in the runtime or the kernel — the
    /// rejection variant is logged and names the exact rule.
    hwa2_create_input_invalid: RefusalCounter,
    /// `pfnAllocateCb` succeeded and the `[in/out]` private buffer came back with
    /// `allocation_generation == 0`, i.e. **the KMD's create-time write did not reach
    /// this UMD's buffer**.
    ///
    /// ⛔ Expected 0, and it is the counter that answers an open question rather than
    /// merely reporting a fault: whether dxgkrnl propagates a create-time kernel write
    /// of the allocation private data back to the creating UMD was established nowhere
    /// in the doc set. Non-zero says it does not, and that the whole descriptor has to
    /// reach this process another way. The create is refused and rolled back, because
    /// an allocation whose descriptor never arrived is one no opener can describe.
    hwa2_write_back_absent: RefusalCounter,
    /// The KMD wrote a descriptor that failed `validate_create_output`, or the buffer
    /// could not be parsed back at all. ⛔ Expected 0; the rejection variant is logged
    /// and the allocation is rolled back.
    hwa2_create_output_invalid: RefusalCounter,
    /// The KMD's output descriptor **did not echo** the create input: some field other
    /// than `allocation_generation` and the two KMD-owned flag bits came back changed.
    ///
    /// ⛔ Expected 0. K4-CONTRACT §1.1: *"the KMD refuses the create rather than
    /// correcting a field"*, because a silent correction would make the descriptor
    /// disagree with the resource this driver believes it made. A hit is therefore a
    /// finding about the kernel's write site, not a value to adopt — the create is
    /// refused and rolled back.
    hwa2_echo_mismatch: RefusalCounter,
    /// A committed create for which the engine and the ICD **could** name the host
    /// venus resource id, and this driver deliberately did **not** send it.
    ///
    /// ⚠ **Not a refusal — the census of the §5 gap**, at the exact site where the
    /// value is dropped. HWA2 carries no host resource token (§10.3) and
    /// `docs/retirement/K4-CONTRACT.md` §5 forbids inventing a replacement field,
    /// stashing it elsewhere, or keeping the legacy record alive as a side channel.
    /// ⇒ the mechanism that replaces it is **mesa lane unit A3**: the ICD stops naming
    /// host resources at all and the KMD patches the resid in from
    /// `HeliosNativeRenderPatch`. Read it against `IdentityVenusUnresolved`, which is
    /// its complement, and against `PresentIdentityNoResourceId`, which is where the
    /// gap is actually paid for.
    hwa2_venus_res_id_dropped: RefusalCounter,
    /// A Core-0111 resource named a nonzero GUID layout while the GUID layout
    /// tier is unadvertised. The resource or allocation-info query is refused.
    resource_layout_guid_refused: RefusalCounter,
    /// `pfnGetTextureLayouts` lacked its mandatory writable count pointer.
    texture_layout_query_bad_arg: RefusalCounter,
    /// The runtime queried GUID layouts and received the truthful empty set.
    texture_layout_query_empty: RefusalCounter,
}

static L4_REFUSALS: L4Refusals = L4Refusals {
    heap_resource_calc_bad_arg: RefusalCounter::new("HeapResourceCalcBadArg"),
    heap_resource_create_bad_arg: RefusalCounter::new("HeapResourceCreateBadArg"),
    heap_create_engine_failed: RefusalCounter::new("HeapCreateEngineFailed"),
    heap_property_unrepresentable: RefusalCounter::new("HeapPropertyUnrepresentable"),
    heap_flag_unrepresentable: RefusalCounter::new("HeapFlagUnrepresentable"),
    heap_node_mask_unexpected: RefusalCounter::new("HeapNodeMaskUnexpected"),
    heap_span_buffer_unavailable: RefusalCounter::new("HeapSpanBufferUnavailable"),
    resource_create_engine_failed: RefusalCounter::new("ResourceCreateEngineFailed"),
    resource_reserved_refused: RefusalCounter::new("ResourceReservedRefused"),
    resource_placement_unresolved: RefusalCounter::new("ResourcePlacementUnresolved"),
    resource_create_at_virtual_address: RefusalCounter::new("ResourceCreateAtVirtualAddress"),
    resource_row_major_layout_ignored: RefusalCounter::new("ResourceRowMajorLayoutIgnored"),
    resource_layout_unrepresentable: RefusalCounter::new("ResourceLayoutUnrepresentable"),
    resource_flag_unrepresentable: RefusalCounter::new("ResourceFlagUnrepresentable"),
    resource_barrier_layout_coerced: RefusalCounter::new("ResourceBarrierLayoutCoerced"),
    castable_formats_dropped: RefusalCounter::new("CastableFormatsDropped"),
    protected_session_ignored: RefusalCounter::new("ProtectedSessionIgnored"),
    resource_device10_unavailable: RefusalCounter::new("ResourceDevice10Unavailable"),
    resource_no_device: RefusalCounter::new("ResourceNoDevice"),
    resource_handle_unresolved: RefusalCounter::new("ResourceHandleUnresolved"),
    map_heap_no_anchor: RefusalCounter::new("MapHeapNoAnchor"),
    map_heap_engine_failed: RefusalCounter::new("MapHeapEngineFailed"),
    map_heap_calls: RefusalCounter::new("MapHeapCalls"),
    resource_alloc_info_engine_failed: RefusalCounter::new("ResourceAllocInfoEngineFailed"),
    resource_alignment_clamped: RefusalCounter::new("ResourceAlignmentClamped"),
    resource_alloc_info_layout_chosen: RefusalCounter::new("ResourceAllocInfoLayoutChosen"),
    resource_optimization_flags_ignored: RefusalCounter::new("ResourceOptimizationFlagsIgnored"),
    subresource_info_out_of_range: RefusalCounter::new("SubresourceInfoOutOfRange"),
    resource_allocation_handle_unavailable: RefusalCounter::new(
        "ResourceAllocationHandleUnavailable",
    ),
    open_heap_calc_refused: RefusalCounter::new("OpenHeapCalcRefused"),
    open_heap_and_resource_refused: RefusalCounter::new("OpenHeapAndResourceRefused"),
    make_resident_calls: RefusalCounter::new("MakeResidentCalls"),
    evict_calls: RefusalCounter::new("EvictCalls"),
    offer_resources_accepted: RefusalCounter::new("OfferResourcesAccepted"),
    reclaim_resources_calls: RefusalCounter::new("ReclaimResourcesCalls"),
    residency_bad_arg: RefusalCounter::new("ResidencyBadArg"),
    heap_primary_flag_dropped: RefusalCounter::new("HeapPrimaryFlagDropped"),
    resource_alignment_restriction_ignored: RefusalCounter::new(
        "ResourceAlignmentRestrictionIgnored",
    ),
    heap_block_unreclaimed: RefusalCounter::new("HeapBlockUnreclaimed"),
    identity_recorded: RefusalCounter::new("IdentityRecorded"),
    identity_table_full: RefusalCounter::new("IdentityTableFull"),
    identity_replaced: RefusalCounter::new("IdentityReplaced"),
    identity_removed: RefusalCounter::new("IdentityRemoved"),
    identity_vk_memory_unresolved: RefusalCounter::new("IdentityVkMemoryUnresolved"),
    identity_venus_unresolved: RefusalCounter::new("IdentityVenusUnresolved"),
    heap_primary_without_resource: RefusalCounter::new("HeapPrimaryWithoutResource"),
    resource_optimization_primary: RefusalCounter::new("ResourceOptimizationPrimary"),
    heap_primary_venus_export: RefusalCounter::new("HeapPrimaryVenusExport"),
    identity_offset_nonzero: RefusalCounter::new("IdentityOffsetNonZero"),
    identity_res_id_shared: RefusalCounter::new("IdentityResIdShared"),
    identity_ctx_id_unavailable: RefusalCounter::new("IdentityCtxIdUnavailable"),
    identity_alloc_size_disagreement: RefusalCounter::new("IdentityAllocSizeDisagreement"),
    allocate_cb_missing: RefusalCounter::new("AllocateCbMissing"),
    allocate_cb_failed: RefusalCounter::new("AllocateCbFailed"),
    allocate_cb_no_handle: RefusalCounter::new("AllocateCbNoHandle"),
    ownership_transfer_failed: RefusalCounter::new("OwnershipTransferFailed"),
    deallocate_cb_missing: RefusalCounter::new("DeallocateCbMissing"),
    deallocate_cb_failed: RefusalCounter::new("DeallocateCbFailed"),
    alloc_private_written_back: RefusalCounter::new("AllocPrivateWrittenBack"),
    meta_bind_flag_unknown: RefusalCounter::new("MetaBindFlagUnknown"),
    committed_venus_export: RefusalCounter::new("CommittedVenusExport"),
    identity_registry_alloc_failed: RefusalCounter::new("IdentityRegistryAllocFailed"),
    hwa2_geometry_unrepresentable: RefusalCounter::new("Hwa2GeometryUnrepresentable"),
    hwa2_create_input_invalid: RefusalCounter::new("Hwa2CreateInputInvalid"),
    hwa2_write_back_absent: RefusalCounter::new("Hwa2WriteBackAbsent"),
    hwa2_create_output_invalid: RefusalCounter::new("Hwa2CreateOutputInvalid"),
    hwa2_echo_mismatch: RefusalCounter::new("Hwa2EchoMismatch"),
    hwa2_venus_res_id_dropped: RefusalCounter::new("Hwa2VenusResIdDropped"),
    resource_layout_guid_refused: RefusalCounter::new("ResourceLayoutGuidRefused"),
    texture_layout_query_bad_arg: RefusalCounter::new("TextureLayoutQueryBadArg"),
    texture_layout_query_empty: RefusalCounter::new("TextureLayoutQueryEmpty"),
};

/// L4's refusal set, printed by `crate::log_refusal_summary` at this lane's
/// position in `lib.rs`'s `UMD12_REFUSAL_SETS`.
///
/// ⭐ **Declared here rather than in `lib.rs` so this lane's diff against the
/// crate root is empty.** Every one of the eleven S6 lanes needs counters
/// (`PARALLEL.md` §9.1: *every skipped or refused path gets a named counter*),
/// and one flat array in `lib.rs` would have been the split's hottest merge
/// point — §5's shared-file table does not even list `lib.rs`. Same move
/// `forward12::tables12` makes for the 206 slots: name all eleven up front and
/// the lanes become substitutive instead of additive.
///
/// ⛔ **Append only**, in declaration order.
pub(crate) static REFUSALS: &[&RefusalCounter] = &[
    &L4_REFUSALS.heap_resource_calc_bad_arg,
    &L4_REFUSALS.heap_resource_create_bad_arg,
    &L4_REFUSALS.heap_create_engine_failed,
    &L4_REFUSALS.heap_property_unrepresentable,
    &L4_REFUSALS.heap_flag_unrepresentable,
    &L4_REFUSALS.heap_node_mask_unexpected,
    &L4_REFUSALS.heap_span_buffer_unavailable,
    &L4_REFUSALS.resource_create_engine_failed,
    &L4_REFUSALS.resource_reserved_refused,
    &L4_REFUSALS.resource_placement_unresolved,
    &L4_REFUSALS.resource_create_at_virtual_address,
    &L4_REFUSALS.resource_row_major_layout_ignored,
    &L4_REFUSALS.resource_layout_unrepresentable,
    &L4_REFUSALS.resource_flag_unrepresentable,
    &L4_REFUSALS.resource_barrier_layout_coerced,
    &L4_REFUSALS.castable_formats_dropped,
    &L4_REFUSALS.protected_session_ignored,
    &L4_REFUSALS.resource_device10_unavailable,
    &L4_REFUSALS.resource_no_device,
    &L4_REFUSALS.resource_handle_unresolved,
    &L4_REFUSALS.map_heap_no_anchor,
    &L4_REFUSALS.map_heap_engine_failed,
    &L4_REFUSALS.map_heap_calls,
    &L4_REFUSALS.resource_alloc_info_engine_failed,
    &L4_REFUSALS.resource_alignment_clamped,
    &L4_REFUSALS.resource_alloc_info_layout_chosen,
    &L4_REFUSALS.resource_optimization_flags_ignored,
    &L4_REFUSALS.subresource_info_out_of_range,
    &L4_REFUSALS.resource_allocation_handle_unavailable,
    &L4_REFUSALS.open_heap_calc_refused,
    &L4_REFUSALS.open_heap_and_resource_refused,
    &L4_REFUSALS.make_resident_calls,
    &L4_REFUSALS.evict_calls,
    &L4_REFUSALS.offer_resources_accepted,
    &L4_REFUSALS.reclaim_resources_calls,
    &L4_REFUSALS.residency_bad_arg,
    &L4_REFUSALS.heap_primary_flag_dropped,
    &L4_REFUSALS.resource_alignment_restriction_ignored,
    &L4_REFUSALS.heap_block_unreclaimed,
    &L4_REFUSALS.identity_recorded,
    &L4_REFUSALS.identity_table_full,
    &L4_REFUSALS.identity_replaced,
    &L4_REFUSALS.identity_removed,
    &L4_REFUSALS.identity_vk_memory_unresolved,
    &L4_REFUSALS.identity_venus_unresolved,
    &L4_REFUSALS.heap_primary_without_resource,
    &L4_REFUSALS.resource_optimization_primary,
    &L4_REFUSALS.heap_primary_venus_export,
    &L4_REFUSALS.identity_offset_nonzero,
    &L4_REFUSALS.identity_res_id_shared,
    &L4_REFUSALS.identity_ctx_id_unavailable,
    &L4_REFUSALS.identity_alloc_size_disagreement,
    &L4_REFUSALS.allocate_cb_missing,
    &L4_REFUSALS.allocate_cb_failed,
    &L4_REFUSALS.allocate_cb_no_handle,
    &L4_REFUSALS.ownership_transfer_failed,
    &L4_REFUSALS.deallocate_cb_missing,
    &L4_REFUSALS.deallocate_cb_failed,
    &L4_REFUSALS.alloc_private_written_back,
    // ⛔ APPENDED, the bind-flag translation fix. At the END for the reason the
    // block comments above give: `D3D12 DDI refusals:` lines are diffed across builds.
    &L4_REFUSALS.meta_bind_flag_unknown,
    &L4_REFUSALS.committed_venus_export,
    &L4_REFUSALS.identity_registry_alloc_failed,
    // ⛔ APPENDED, K4 / the HPS2 retirement: the HWA2 create-input + write-back
    // contract and the §5 host-resource-id gap. At the END for the reason the block
    // comments above give -- `D3D12 DDI refusals:` lines are diffed across builds.
    &L4_REFUSALS.hwa2_geometry_unrepresentable,
    &L4_REFUSALS.hwa2_create_input_invalid,
    &L4_REFUSALS.hwa2_write_back_absent,
    &L4_REFUSALS.hwa2_create_output_invalid,
    &L4_REFUSALS.hwa2_echo_mismatch,
    &L4_REFUSALS.hwa2_venus_res_id_dropped,
    // APPENDED with the Core-0111 descriptor/table transition.
    &L4_REFUSALS.resource_layout_guid_refused,
    &L4_REFUSALS.texture_layout_query_bad_arg,
    &L4_REFUSALS.texture_layout_query_empty,
];
