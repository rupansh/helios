//! Helios D3D-side outer-batch and allocation ABI — the bytes that cross the
//! D3D UMD ↔ dxgkrnl ↔ `kmd_render` ↔ QEMU boundary after the HPS2 retirement.
//!
//! Four records live here, all pointer-free, little-endian, padding-free, and
//! immutable once written:
//!
//!   - [`HeliosWddmAllocationDescV2`] (`HWA2`, 168 B) — the create-time
//!     allocation descriptor the KMD writes into the `[in/out]` allocation
//!     private buffer. Every opener treats it as `const`
//!     (`HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.3).
//!   - [`HeliosOuterBatchV1`] (`HOB1`, 112 B header) plus its 40-byte
//!     [`HeliosOuterBatchUseV1`] table, 16-byte
//!     [`HeliosOuterBatchOperandV1`] table, and the sealed Venus payload — the
//!     one complete contiguous record of a translated outer command (§10.4).
//!   - [`HeliosOuterSubmitV1`] (`HOS1`, 64 B) — the D3D12 `pPrivateDriverData`
//!     descriptor. Metadata only: no command bytes, no resource identity, no
//!     pointer, handle, GPUVA, or host token (§10.4).
//!   - [`HeliosOuterCommandAllocationV1`] (`HOC1`, 64 B) — the per-allocation
//!     record for the C65 64-MiB D3D12 command-buffer pool (§10.6).
//!
//! # What this module supersedes
//!
//! This file replaces the HPS2 D3D wire surface: the `HeliosWddmAllocPrivate` /
//! `HeliosWddmAllocMeta` create-time pair, the mutable open-time
//! `HeliosWddmOpenIdentity` restamp, and the `HeliosPresent*` /
//! `HeliosD3D12SubmitCmd` present-ticket and raw-`resid` command identities.
//! None of those constructs survives: identity is the exact WDDM allocation
//! object plus a nonzero KMD-assigned allocation *generation*, never a renderer
//! resource id, ticket, PID, handle, or timing/dimension inference.
//!
//! It also supersedes the deleted `escape.rs` D3DKMTEscape verb ABI and the
//! deleted System-class `ioctl.rs` carrier. No transport, discovery, metadata,
//! synchronization, completion, lifetime, diagnostic, or fallback path may be
//! routed through either retired mechanism.
//!
//! # Reading the invariants that shaped these bytes (§10.1)
//!
//! * No adapter/process-global resource or synchronization discovery structure
//!   exists (inv. 10) — so no record here carries a lookup key.
//! * A command carrying `WrittenPrimaries` contains the batch that actually
//!   performs those writes (inv. 2) — so a HOB1 always carries a nonzero
//!   payload and `HOS1` is never itself work.
//! * No HOB1 record orders another WDDM context: every generation field is an
//!   anti-stale cross-check against the live KMD context object, which remains
//!   the identity (§10.4).
//!
//! All structs are `#[repr(C)]` and padding-free (explicit reserved fields,
//! never implicit padding), so they derive `Pod`/`Zeroable`, the C mirror in
//! Mesa/QEMU (`protocol/include/helios_wddm.h`) is a byte-for-byte
//! transcription, and every field offset in the doc's tables is pinned by a
//! `const` assertion that fails the *build*.
//!
//! # Where the validation lives
//!
//! A HOB1 record has three properties no single struct can carry, so they are
//! enforced by [`validate_batch_record`] and by nothing else:
//!
//!   * every `identityKind=1` use record is bounded by the caller's
//!     `D3DDDI_ALLOCATIONLIST` length ([`HeliosOuterBatchExpectation::allocation_list_count`])
//!     — the KMD indexes a kernel array with a user-mode number;
//!   * the use table is the **unique**-allocation closure on the D3D11 arm; and
//!   * the redundant use↔operand pair agrees in both directions.
//!
//! Each of the three is a §10.4 sentence rather than a table row, and each of
//! them is silent if only the per-record validators run.
//!
//! ⛔ Every parser here borrows out of the caller's buffer. The bytes must
//! already be somewhere the producer cannot still write — the KMD's copied
//! command slot, or QEMU's own snapshot of the C65 extent. §10.6's "once
//! submitted, the extent is immutable" is a guest-side promise and is not a
//! host-side guarantee.
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

// ─────────────────────────────────────────────────────────────────────────────
// Shared ABI anchors
// ─────────────────────────────────────────────────────────────────────────────

/// `D3DDDI_ID_UNINITIALIZED` — the WDDM sentinel a C44 D3D12 runtime primary
/// must preserve in its VidPn-source field, and the value a non-primary
/// allocation carries (§10.3 offset 80, C44).
///
/// Duplicated from the WDK header on purpose: this crate has no bindgen edge
/// and the value is a frozen Windows ABI constant.
pub const D3DDDI_ID_UNINITIALIZED: u32 = 0xFFFF_FFFF;

/// `DXGI_FORMAT_UNKNOWN`.
pub const DXGI_FORMAT_UNKNOWN: u32 = 0;
/// `DXGI_FORMAT_B8G8R8A8_UNORM` — the sole SDR presentable format in this
/// protocol generation (§10.7 profile, C38).
pub const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;
/// `DXGI_FORMAT_B8G8R8X8_UNORM` — the format the KMD's own standard-primary
/// author deliberately writes (XR24 is the measured egl-headless scanout
/// format, 39th session), so scanout admission accepts it alongside 87.
pub const DXGI_FORMAT_B8G8R8X8_UNORM: u32 = 88;

/// `D3DDDIFMT_A8R8G8B8`, the BGRA `D3DDDIFORMAT` the display path reports.
///
/// Retained from the pre-retirement module: it is a frozen Windows ABI value,
/// not a Helios identity construct, and [`HeliosWddmAllocationDescV2`] offset
/// 52 carries exactly this vocabulary.
pub const D3DDDIFMT_A8R8G8B8: u32 = 21;
/// `D3DDDIFMT_X8R8G8B8`.
pub const D3DDDIFMT_X8R8G8B8: u32 = 22;

// ─────────────────────────────────────────────────────────────────────────────
// §10.3 — HWA2: the immutable create-time allocation descriptor
// ─────────────────────────────────────────────────────────────────────────────

/// `HWA2` — magic of [`HeliosWddmAllocationDescV2`].
/// HELIOS_PRESENT_SYNC_RETIREMENT.md §10.3, offset 0.
pub const HELIOS_HWA2_MAGIC: u32 = 0x3241_5748;
/// HWA2 ABI version (§10.3, offset 4). Not a negotiation: a mismatch is a hard
/// reject and never selects a legacy parser.
pub const HELIOS_HWA2_ABI_VERSION: u16 = 2;
/// HWA2 structure size in bytes (§10.3, offset 6).
pub const HELIOS_HWA2_BYTES: u16 = 168;

/// Required alignment of [`HeliosCpuBackingV1::va`]. One page: the KMD builds
/// an MDL over the range and takes its page frames, so a sub-page start would
/// put bytes the creator does not own in the first entry.
pub const HELIOS_CPU_BACKING_ALIGN: u64 = 4096;

/// `"HCB1"`, the CPU-backing side record's magic.
pub const HELIOS_CPU_BACKING_MAGIC: u32 = u32::from_le_bytes(*b"HCB1");
/// Exact wire length of [`HeliosCpuBackingV1`].
pub const HELIOS_CPU_BACKING_BYTES: u16 = 24;

/// The creator's own CPU buffer for the allocation being created, carried in
/// the RESOURCE-level private data rather than in the allocation descriptor.
///
/// ⛔ IT MAY NOT LIVE IN [`HeliosWddmAllocationDescV2`], and this is measured,
/// not stylistic. That record is echoed: dxgkrnl copies the KMD's create-time
/// output back over the creator's buffer, and the UMD validates it. With ANY
/// nonzero tail byte in the create input, that copy-back stops happening —
/// isolated on 22.22.411.0 with a control that wrote a FAKE page-aligned value
/// (`0x1000`) into the field and changed nothing else: the write-back vanished
/// exactly as it did with a real address, and the allocation then failed the
/// UMD's own `AllocationGenerationZero` check. Input-only data belongs in an
/// input-only channel.
///
/// The resource-level buffer is that channel: `DxgkDdiCreateAllocation` gets it
/// as `DXGKARG_CREATEALLOCATION::pPrivateDriverData`, nothing echoes it, and
/// the allocation descriptor stays byte-identical to the 168-byte record that
/// has always round-tripped.
///
/// ⚠ No C mirror, deliberately (CLAUDE.md rule 10): only the Rust UMD and KMD
/// read it, so a hand-written twin would be drift with no reader.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosCpuBackingV1 {
    /// `== HELIOS_CPU_BACKING_MAGIC` (offset 0).
    pub magic: u32,
    /// `== HELIOS_CPU_BACKING_BYTES` (offset 4).
    pub struct_size: u16,
    /// Zero (offset 6).
    pub reserved: u16,
    /// Page-aligned user VA in the CREATING process (offset 8), nonzero.
    pub va: u64,
    /// Page-rounded byte length of the buffer at [`Self::va`] (offset 16).
    pub bytes: u64,
}

const _: () = {
    assert!(core::mem::size_of::<HeliosCpuBackingV1>() == HELIOS_CPU_BACKING_BYTES as usize);
    assert!(core::mem::align_of::<HeliosCpuBackingV1>() == 8);
    assert!(core::mem::offset_of!(HeliosCpuBackingV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosCpuBackingV1, struct_size) == 4);
    assert!(core::mem::offset_of!(HeliosCpuBackingV1, reserved) == 6);
    assert!(core::mem::offset_of!(HeliosCpuBackingV1, va) == 8);
    assert!(core::mem::offset_of!(HeliosCpuBackingV1, bytes) == 16);
};

impl HeliosCpuBackingV1 {
    /// A complete record for `va`/`bytes`.
    pub const fn new(va: u64, bytes: u64) -> Self {
        Self {
            magic: HELIOS_CPU_BACKING_MAGIC,
            struct_size: HELIOS_CPU_BACKING_BYTES,
            reserved: 0,
            va,
            bytes,
        }
    }

    /// Read one out of a `(pPrivateDriverData, PrivateDriverDataSize)` pair, or
    /// `None` for anything that is not exactly this record. Total: a resource
    /// private buffer that is absent, another size, or another producer's is a
    /// "no pages offered", never a partial read.
    pub fn from_private_data(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != HELIOS_CPU_BACKING_BYTES as usize {
            return None;
        }
        let record = bytemuck::try_pod_read_unaligned::<Self>(bytes).ok()?;
        record.validate().then_some(record)
    }

    /// Every field a consumer relies on, checked together.
    pub const fn validate(&self) -> bool {
        self.magic == HELIOS_CPU_BACKING_MAGIC
            && self.struct_size == HELIOS_CPU_BACKING_BYTES
            && self.reserved == 0
            && self.va != 0
            && self.va % HELIOS_CPU_BACKING_ALIGN == 0
            && self.bytes != 0
            && self.bytes % HELIOS_CPU_BACKING_ALIGN == 0
    }
}

// ── allocation kind (§10.3, offset 64) ──────────────────────────────────────
//
// "versioned protocol enum: buffer, image, standard primary/shadow/staging, or
// paging object". Zero is not a member: an all-zero descriptor must fail, and
// it does — on the magic first and the kind second.

/// Not a member. Present so a zeroed buffer is refused by name.
pub const HELIOS_HWA2_KIND_INVALID: u32 = 0;
/// A linear byte range with no DXGI image interpretation.
pub const HELIOS_HWA2_KIND_BUFFER: u32 = 1;
/// An ordinary UMD-created image.
pub const HELIOS_HWA2_KIND_IMAGE: u32 = 2;
/// `D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE`.
pub const HELIOS_HWA2_KIND_STANDARD_PRIMARY: u32 = 3;
/// `D3DKMDT_STANDARDALLOCATION_SHADOWSURFACE`.
pub const HELIOS_HWA2_KIND_STANDARD_SHADOW: u32 = 4;
/// `D3DKMDT_STANDARDALLOCATION_STAGINGSURFACE`.
pub const HELIOS_HWA2_KIND_STANDARD_STAGING: u32 = 5;
/// A KMD/VidMm paging object: no DXGI interpretation, no texel geometry. The
/// C65 HOB1 command pool is one of these.
pub const HELIOS_HWA2_KIND_PAGING_OBJECT: u32 = 6;
/// Highest defined kind; anything above it is rejected by name.
pub const HELIOS_HWA2_KIND_MAX: u32 = HELIOS_HWA2_KIND_PAGING_OBJECT;

/// Does this kind have texel geometry and a DXGI interpretation?
///
/// ⚠ Load-bearing in both directions. An image kind must carry nonzero
/// width/height/depth/mip/sample-count and a known `DXGI_FORMAT`; a non-image
/// kind must carry zero in every one of those fields and `DXGI_FORMAT_UNKNOWN`.
/// A `D3DKMDT_STANDARDALLOCATION_GDISURFACE` has no kind of its own in the
/// §10.3 enum: it is [`HELIOS_HWA2_KIND_IMAGE`] plus the
/// [`HELIOS_HWA2_FLAG_STANDARD`] bit and the exact OS enum in
/// [`HeliosWddmAllocationDescV2::standard_allocation_type`].
#[inline]
pub const fn helios_hwa2_kind_is_image(kind: u32) -> bool {
    matches!(
        kind,
        HELIOS_HWA2_KIND_IMAGE
            | HELIOS_HWA2_KIND_STANDARD_PRIMARY
            | HELIOS_HWA2_KIND_STANDARD_SHADOW
            | HELIOS_HWA2_KIND_STANDARD_STAGING
    )
}

/// Is this kind one of the three `STANDARD_*` members, which may appear only
/// with [`HELIOS_HWA2_FLAG_STANDARD`] set?
#[inline]
pub const fn helios_hwa2_kind_is_standard(kind: u32) -> bool {
    matches!(
        kind,
        HELIOS_HWA2_KIND_STANDARD_PRIMARY
            | HELIOS_HWA2_KIND_STANDARD_SHADOW
            | HELIOS_HWA2_KIND_STANDARD_STAGING
    )
}

// ── flags (§10.3, offset 68) ────────────────────────────────────────────────
//
// The doc lists exactly eleven bits, in this order, and says "every other bit
// zero". The order below is the doc's order; do not renumber.

/// The allocation is a primary (scanout-eligible) surface.
pub const HELIOS_HWA2_FLAG_PRIMARY: u32 = 1 << 0;
/// Stereo primary. Refused for Direct Flip in this generation.
pub const HELIOS_HWA2_FLAG_STEREO: u32 = 1 << 1;
/// The allocation is shareable across D3D devices/processes.
pub const HELIOS_HWA2_FLAG_SHARED: u32 = 1 << 2;
/// The allocation may be bound to a display plane.
pub const HELIOS_HWA2_FLAG_DISPLAYABLE: u32 = 1 << 3;
/// KMD-asserted Direct Flip eligibility. Necessary but never sufficient: the
/// D3D11 UMD must still compare both live wrappers and every C43 field, and KMD
/// `CheckMPO3` must still accept the actual display attributes (§10.3).
pub const HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE: u32 = 1 << 4;
/// C44: the primary was created through the D3D12 Resource-Heaps path, whose
/// runtime overwrites `VidPnSourceId` with [`D3DDDI_ID_UNINITIALIZED`].
///
/// ⛔ The bit and the sentinel are **cross-validated**; neither is inferred by
/// an opener. KMD sets it only when the D3D12 create record, the runtime
/// `PRIMARY` flag, and the required sentinel value all agree (§10.3, C44).
pub const HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY: u32 = 1 << 5;
/// Protected/DRM content. Refused for Direct Flip in this generation.
pub const HELIOS_HWA2_FLAG_PROTECTED: u32 = 1 << 6;
/// Cross-adapter allocation. Refused for Direct Flip in this generation; §3
/// forbids cross-adapter inference outright.
pub const HELIOS_HWA2_FLAG_CROSS_ADAPTER: u32 = 1 << 7;
/// The backing is CPU-visible.
pub const HELIOS_HWA2_FLAG_CPU_VISIBLE: u32 = 1 << 8;
/// Dxgkrnl asked KMD to create a resource object for this allocation
/// (`DXGK_CREATEALLOCATIONFLAGS::Resource`), preserved verbatim so later
/// diagnostics never have to infer it from dimensions, process, or order.
pub const HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED: u32 = 1 << 9;
/// The allocation is an OS "standard allocation";
/// [`HeliosWddmAllocationDescV2::standard_allocation_type`] then carries the
/// exact `D3DKMDT_STANDARDALLOCATION_TYPE`.
pub const HELIOS_HWA2_FLAG_STANDARD: u32 = 1 << 10;
/// Union of every defined flag. Any bit outside this mask is a hard reject.
pub const HELIOS_HWA2_FLAG_MASK: u32 = 0x0000_07FF;
/// The flag bits **only the KMD may set** — and therefore the exact set a
/// create-*input* descriptor must leave clear.
///
/// §10.3 states both as assertions the kernel makes about a finished
/// allocation: "KMD sets `D3D12_RUNTIME_PRIMARY` only when the D3D12 create
/// record, runtime `PRIMARY` flag, and required `D3DDDI_ID_UNINITIALIZED` value
/// agree … KMD sets `DIRECT_FLIP_COMPATIBLE` only when the exact allocation is
/// a non-protected, non-cross-adapter managed primary in a swizzle/layout class
/// the selected display backend implements", and of both: "neither is inferred
/// by an opener".
///
/// A UMD that pre-set either would be asserting a property of an allocation
/// that does not exist yet, and once the KMD echoes the input word back there
/// is nothing in the finished record to distinguish a UMD's guess from the
/// kernel's finding. So [`HeliosWddmAllocationDescV2::validate_create_input`]
/// refuses the bit by name rather than silently clearing it — a silent
/// correction would make the descriptor disagree with the resource the UMD
/// believes it asked for. Named once here so the input rule, the refusal
/// payload, and the C mirror cannot drift apart.
pub const HELIOS_HWA2_FLAG_KMD_OWNED_MASK: u32 =
    HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE | HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;

// ── bind flags (§10.3, offset 72) ───────────────────────────────────────────
//
// ⛔⛔ "shared protocol vocabulary, never raw D3D11/D3D12 bit reinterpretation".
//
// The retired `HeliosWddmAllocMeta::bind_flags` was the raw `D3D10DDI_BIND_*`
// word, and a second producer (`helios_umd12.dll`) wrote a
// `D3D12DDI_RESOURCE_FLAGS_0003` word into the same field — a different
// vocabulary at overlapping bit positions, which is not a mismatch any reader
// can detect. The bit assignment below is therefore chosen to be
// NON-COINCIDENT with both D3D11 and D3D12: `SHADER_RESOURCE` is 0x1 here and
// 0x8 at the D3D11 DDI, `RENDER_TARGET` is 0x2 here and 0x1 in the D3D12 DDI.
// An untranslated word from either API lands on a visibly wrong bind or trips
// the reserved-bit reject; it can no longer be silently plausible.

/// Sampleable. What a compositor needs.
pub const HELIOS_HWA2_BIND_SHADER_RESOURCE: u32 = 1 << 0;
/// Render-target bindable.
pub const HELIOS_HWA2_BIND_RENDER_TARGET: u32 = 1 << 1;
/// Depth/stencil bindable.
pub const HELIOS_HWA2_BIND_DEPTH_STENCIL: u32 = 1 << 2;
/// Unordered-access bindable.
pub const HELIOS_HWA2_BIND_UNORDERED_ACCESS: u32 = 1 << 3;
/// Vertex-buffer bindable.
pub const HELIOS_HWA2_BIND_VERTEX_BUFFER: u32 = 1 << 4;
/// Index-buffer bindable.
pub const HELIOS_HWA2_BIND_INDEX_BUFFER: u32 = 1 << 5;
/// Constant-buffer bindable.
pub const HELIOS_HWA2_BIND_CONSTANT_BUFFER: u32 = 1 << 6;
/// Stream-output bindable.
pub const HELIOS_HWA2_BIND_STREAM_OUTPUT: u32 = 1 << 7;
/// Presentable.
pub const HELIOS_HWA2_BIND_PRESENT: u32 = 1 << 8;
/// Video-decoder output.
pub const HELIOS_HWA2_BIND_VIDEO_DECODER: u32 = 1 << 9;
/// Video-encoder input.
pub const HELIOS_HWA2_BIND_VIDEO_ENCODER: u32 = 1 << 10;
/// Union of every defined bind bit. Any bit outside this mask is a hard reject.
pub const HELIOS_HWA2_BIND_MASK: u32 = 0x0000_07FF;

// ── misc flags (§10.3, offset 76) ───────────────────────────────────────────
//
// "versioned protocol vocabulary; reserved bits zero". Protocol-owned, same
// non-coincidence rule as the bind vocabulary above.

/// The surface is GDI-compatible (`GetDC` capable).
pub const HELIOS_HWA2_MISC_GDI_COMPATIBLE: u32 = 1 << 0;
/// The image is a cube map.
pub const HELIOS_HWA2_MISC_TEXTURE_CUBE: u32 = 1 << 1;
/// The resource requests LOD clamping.
pub const HELIOS_HWA2_MISC_RESOURCE_CLAMP: u32 = 1 << 2;
/// The share is an NT handle share rather than a legacy global share.
pub const HELIOS_HWA2_MISC_SHARED_NT_HANDLE: u32 = 1 << 3;
/// Union of every defined misc bit. Any bit outside this mask is a hard reject.
pub const HELIOS_HWA2_MISC_MASK: u32 = 0x0000_000F;

// ── swizzle/layout class (§10.3, offset 88) ─────────────────────────────────

/// Not a member; a zeroed or unset class is refused by name.
pub const HELIOS_HWA2_SWIZZLE_INVALID: u32 = 0;
/// Row-major linear, `rowPitch`-addressable. The only class the selected
/// display backend implements, because scanout binds a LINEAR image through
/// `SET_SCANOUT_BLOB`.
pub const HELIOS_HWA2_SWIZZLE_LINEAR: u32 = 1;
/// Implementation-opaque optimal tiling. Legal for ordinary rendering; never
/// Direct-Flip eligible in this generation.
pub const HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL: u32 = 2;
/// Highest defined class; anything above it is rejected by name.
pub const HELIOS_HWA2_SWIZZLE_MAX: u32 = HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL;

/// Is this a swizzle/layout class the selected display backend implements?
/// §10.3: KMD sets `DIRECT_FLIP_COMPATIBLE` only for such a class, and C43
/// additionally requires the two descriptors' classes to be *equal*.
#[inline]
pub const fn helios_hwa2_swizzle_is_direct_flip_capable(class: u32) -> bool {
    class == HELIOS_HWA2_SWIZZLE_LINEAR
}

/// Is this a class `SetVidPnSourceAddress` can bind the allocation's OWN host
/// resource for, instead of copying it into the adapter's LINEAR target?
///
/// ⛔ Deliberately WIDER than [`helios_hwa2_swizzle_is_direct_flip_capable`],
/// and the two must not be merged. That one answers a WIRE question an opener
/// reads ("dxgkrnl and DWM may Direct-Flip this"), which §10.3 rules out for
/// `OPAQUE_OPTIMAL` outright. This one answers a KMD-PRIVATE routing question,
/// and `OPAQUE_OPTIMAL` passes it because the QEMU fork reconstructs that
/// native layout (`qemu-helios`, native OPTIMAL readback). The same argument is
/// spelled out at `kmd_render/src/ddi/create_allocation.rs`'s
/// `DIRECT_FLIP_COMPATIBLE` stamp.
#[inline]
pub const fn helios_hwa2_swizzle_is_scanout_bindable(class: u32) -> bool {
    class == HELIOS_HWA2_SWIZZLE_LINEAR || class == HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL
}

// ── memory class (§10.3, offset 92) ─────────────────────────────────────────
//
// "device-local/shared/CPU-visible protocol enum; no Vulkan memory-type index".
// ⛔ The retired trailer carried `memory_type_index`; it is gone. A Vulkan
// memory-type index is a host-side detail the KMD's own allocation object owns.

/// Not a member; refused by name.
pub const HELIOS_HWA2_MEMORY_INVALID: u32 = 0;
/// Device-local backing, not CPU mappable.
pub const HELIOS_HWA2_MEMORY_DEVICE_LOCAL: u32 = 1;
/// Shared/system backing reachable by both the device and other adapters'
/// aperture path.
pub const HELIOS_HWA2_MEMORY_SHARED: u32 = 2;
/// CPU-visible backing; implies [`HELIOS_HWA2_FLAG_CPU_VISIBLE`].
pub const HELIOS_HWA2_MEMORY_CPU_VISIBLE: u32 = 3;
/// Highest defined class; anything above it is rejected by name.
pub const HELIOS_HWA2_MEMORY_MAX: u32 = HELIOS_HWA2_MEMORY_CPU_VISIBLE;

/// Maximum plane records in an HWA2 descriptor (§10.3, offset 96/104).
pub const HELIOS_HWA2_MAX_PLANES: u32 = 4;

/// One plane of an HWA2 allocation: `{u64 offset, u32 rowPitch, u32 slicePitch}`
/// (§10.3, offset 104 — four consecutive records of 16 bytes).
///
/// Unused records (index `>= plane_count`) are zero. Every used record's range
/// is overflow-checked to lie inside
/// [`HeliosWddmAllocationDescV2::byte_size`].
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosWddmPlaneRecordV2 {
    /// Byte offset of the plane inside the backing extent.
    pub offset: u64,
    /// Bytes between two consecutive rows of this plane.
    pub row_pitch: u32,
    /// Bytes covered by this plane, i.e. the extent bounded against
    /// `byte_size`. For a chroma plane this is smaller than the luma plane's,
    /// which is why the bound is `offset + slice_pitch`, never
    /// `row_pitch * height`.
    pub slice_pitch: u32,
}

/// The versioned, immutable create-time allocation descriptor
/// (`HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.3, 168 bytes, align 8).
///
/// # The contract, in one paragraph
///
/// The creator allocates the complete buffer; **KMD writes it only at create**;
/// every opener treats it as `const`. Dxgkrnl associates it with the allocation
/// and hands it to a receiving UMD on `OpenResource` (C11-C12). KMD only
/// validates and reads the bytes on an ordinary open — its exact kernel
/// allocation object remains authoritative. `DxgkDdiOpenAllocation` never
/// writes it, which is the whole point: the retired `HeliosWddmOpenIdentity`
/// restamped the first 48 bytes at open time, so two openers of one allocation
/// could disagree about what they had.
///
/// # The two create stages
///
/// The buffer is `[in/out]` and the kernel cannot invent texel dimensions, so
/// the create is a request and a total acceptance: the UMD fills a complete
/// [`Hwa2Stage::CreateInput`] record, the KMD validates it with
/// [`Self::validate_create_input`], and the KMD then writes all 168 bytes back,
/// echoing every field it validated and stamping
/// [`Self::allocation_generation`] plus any bit of
/// [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] it has established. "KMD writes it only
/// on create" stays literally true — the kernel performs the write and no
/// opener writes any of it — and a refused input creates nothing.
///
/// # What it deliberately does not contain
///
/// No host resource token, `resid`, PID, process handle, synchronization
/// object, mutable value, or independently usable identity (§10.3). A host
/// `resid` may live **solely** inside the KMD allocation object; no UMD, ICD,
/// batch, or private descriptor can name or supply one.
/// [`Self::allocation_generation`] is a stale-validation/diagnostic value and
/// is **never an identity lookup key**.
///
/// Any malformed, unknown, truncated, mismatched-generation, or
/// reserved-nonzero descriptor makes create/open fail; it never selects a
/// legacy parser.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosWddmAllocationDescV2 {
    /// `== HELIOS_HWA2_MAGIC` (offset 0).
    pub magic: u32,
    /// `== HELIOS_HWA2_ABI_VERSION` (offset 4).
    pub abi_version: u16,
    /// `== HELIOS_HWA2_BYTES` (offset 6).
    pub struct_size: u16,
    /// Exact atomic-package generation (offset 8). A mismatch is fatal: §17.1
    /// requires one generation shared by protocol, Mesa, both UMDs, KMD, QEMU,
    /// and the installer.
    pub package_generation: u64,
    /// Nonzero KMD-assigned stale-validation/diagnostic generation (offset 16).
    /// **Zero on create-input**; the kernel alone assigns it.
    ///
    /// ⛔ **Never an identity lookup key.** HOB1 use records repeat it as
    /// `expected_allocation_generation` so a stale batch is refused; nothing
    /// resolves an allocation *from* it.
    pub allocation_generation: u64,
    /// Exact backing extent (offset 24): nonzero, and bounds every plane.
    pub byte_size: u64,
    /// Exact texel width (offset 32); zero only for a non-image kind.
    pub width: u32,
    /// Exact texel height (offset 36); zero only for a non-image kind.
    pub height: u32,
    /// Exact depth/array size (offset 40); nonzero for images.
    pub depth_or_array_size: u32,
    /// Exact mip level count (offset 44); nonzero for images.
    pub mip_levels: u32,
    /// Exact `DXGI_FORMAT` (offset 48); `UNKNOWN` only for a kind with no DXGI
    /// image interpretation.
    ///
    /// ⚠ Carried verbatim because [`Self::d3d_ddi_format`] is lossy: the
    /// D3DDDIFORMAT↔DXGI translation collapses non-BGRA surfaces to BGRA, which
    /// historically made openers rebuild A8 masks as 4 bpp BGRA.
    pub dxgi_format: u32,
    /// Exact `D3DDDIFORMAT` supplied to / reported to the runtime (offset 52).
    pub d3d_ddi_format: u32,
    /// Exact sample count (offset 56); nonzero for images.
    pub sample_count: u32,
    /// Exact sample quality (offset 60).
    pub sample_quality: u32,
    /// `HELIOS_HWA2_KIND_*` (offset 64).
    pub allocation_kind: u32,
    /// `HELIOS_HWA2_FLAG_*` (offset 68); every other bit zero. On create-input
    /// every bit of [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] is zero too.
    pub flags: u32,
    /// `HELIOS_HWA2_BIND_*` (offset 72) — the shared protocol vocabulary, never
    /// a raw D3D11/D3D12 bit reinterpretation.
    pub bind_flags: u32,
    /// `HELIOS_HWA2_MISC_*` (offset 76); reserved bits zero.
    pub misc_flags: u32,
    /// Exact create-time `VidPnSourceId` (offset 80).
    ///
    /// A conventional D3D11 primary carries its concrete source; a C44 D3D12
    /// runtime primary must preserve [`D3DDDI_ID_UNINITIALIZED`]; a non-primary
    /// uses that same sentinel. The sentinel means *any source on this exact
    /// adapter* and is never replaced or compared as a concrete identity.
    ///
    /// On create-input a primary carrying the sentinel is admitted — it is the
    /// D3D12 request whose `D3D12_RUNTIME_PRIMARY` bit the kernel has not stamped
    /// yet, and the creator is forbidden from stamping it itself
    /// ([`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`]).
    pub vidpn_source: u32,
    /// Exact `D3DKMDT_STANDARDALLOCATION_TYPE` when
    /// [`HELIOS_HWA2_FLAG_STANDARD`] is set (offset 84), otherwise zero. The OS
    /// enum has no zero member, so zero-with-`STANDARD` is a hard reject.
    pub standard_allocation_type: u32,
    /// `HELIOS_HWA2_SWIZZLE_*` (offset 88). Direct Flip requires equality
    /// between the pair and an explicitly supported class.
    pub swizzle_class: u32,
    /// `HELIOS_HWA2_MEMORY_*` (offset 92). No Vulkan memory-type index.
    pub memory_class: u32,
    /// Plane record count, `0..=4` (offset 96). Direct Flip requires exactly
    /// one.
    pub plane_count: u32,
    /// Reserved; zero (offset 100).
    pub reserved: u32,
    /// Four plane records (offset 104, 64 bytes). Records at or above
    /// [`Self::plane_count`] are zero.
    pub planes: [HeliosWddmPlaneRecordV2; 4],
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md §10.3 — every offset in the 168-byte table.
// A wrong offset breaks the build, not a test.
const _: () = {
    assert!(core::mem::size_of::<HeliosWddmPlaneRecordV2>() == 16);
    assert!(core::mem::align_of::<HeliosWddmPlaneRecordV2>() == 8);
    assert!(core::mem::offset_of!(HeliosWddmPlaneRecordV2, offset) == 0);
    assert!(core::mem::offset_of!(HeliosWddmPlaneRecordV2, row_pitch) == 8);
    assert!(core::mem::offset_of!(HeliosWddmPlaneRecordV2, slice_pitch) == 12);

    assert!(core::mem::size_of::<HeliosWddmAllocationDescV2>() == 168);
    assert!(core::mem::size_of::<HeliosWddmAllocationDescV2>() == HELIOS_HWA2_BYTES as usize);
    assert!(core::mem::align_of::<HeliosWddmAllocationDescV2>() == 8);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, magic) == 0);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, allocation_generation) == 16);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, byte_size) == 24);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, width) == 32);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, height) == 36);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, depth_or_array_size) == 40);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, mip_levels) == 44);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, dxgi_format) == 48);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, d3d_ddi_format) == 52);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, sample_count) == 56);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, sample_quality) == 60);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, allocation_kind) == 64);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, flags) == 68);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, bind_flags) == 72);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, misc_flags) == 76);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, vidpn_source) == 80);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, standard_allocation_type) == 84);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, swizzle_class) == 88);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, memory_class) == 92);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, plane_count) == 96);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, reserved) == 100);
    assert!(core::mem::offset_of!(HeliosWddmAllocationDescV2, planes) == 104);
    // The eleven flag bits of §10.3 offset 68, and nothing else.
    assert!(
        HELIOS_HWA2_FLAG_MASK
            == HELIOS_HWA2_FLAG_PRIMARY
                | HELIOS_HWA2_FLAG_STEREO
                | HELIOS_HWA2_FLAG_SHARED
                | HELIOS_HWA2_FLAG_DISPLAYABLE
                | HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE
                | HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY
                | HELIOS_HWA2_FLAG_PROTECTED
                | HELIOS_HWA2_FLAG_CROSS_ADAPTER
                | HELIOS_HWA2_FLAG_CPU_VISIBLE
                | HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED
                | HELIOS_HWA2_FLAG_STANDARD
    );
    // The KMD-owned pair is a subset of the eleven, and is exactly the two bits
    // §10.3 says the kernel sets and no opener infers. The C mirror pins the
    // same literal, because there the union is spelled out by hand.
    assert!(HELIOS_HWA2_FLAG_KMD_OWNED_MASK & !HELIOS_HWA2_FLAG_MASK == 0);
    assert!(HELIOS_HWA2_FLAG_KMD_OWNED_MASK == 0x0000_0030);
    assert!(HELIOS_HWA2_MAX_PLANES == 4);

    // Four ASCII bytes read little-endian; see the same block under HOB1.
    assert!(HELIOS_HWA2_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HWA2_MAGIC.to_le_bytes()[1] == b'W');
    assert!(HELIOS_HWA2_MAGIC.to_le_bytes()[2] == b'A');
    assert!(HELIOS_HWA2_MAGIC.to_le_bytes()[3] == b'2');
};

/// Which side of the `DxgkDdiCreateAllocation` descriptor write is being
/// validated.
///
/// The allocation-private buffer is `[in/out]` and the KMD cannot invent texel
/// dimensions, so §10.3's "KMD writes it only on create" completes exactly one
/// way: the UMD supplies a complete create-input record, the KMD validates it
/// in full, and then the kernel writes all 168 bytes back — echoing every field
/// it validated and stamping the ones only it can know
/// (`docs/retirement/K4-CONTRACT.md` §1). The input is a *request* whose
/// acceptance is total; a rejected request creates nothing, and no opener ever
/// writes a byte.
///
/// The difference between the two sides is two bits, one field, and the one
/// cross-field rule those bits make stage-dependent:
///
/// 1. [`HeliosWddmAllocationDescV2::allocation_generation`] — zero in, nonzero
///    out.
/// 2. The two flags in [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] — clear in,
///    KMD-decided out.
/// 3. C44's primary/VidPn rule. Because `D3D12_RUNTIME_PRIMARY` is one of those
///    KMD-owned bits, a D3D12 runtime primary's **input** is PRIMARY + the
///    [`D3DDDI_ID_UNINITIALIZED`] sentinel + the bit clear — that shape is the
///    request, and refusing it (as this crate did until the seam review) makes
///    the kernel's stamp unreachable, since no legal input could ever reach it.
///    On the **output** side the pair is cross-validated in both directions,
///    unchanged: see [`HeliosAllocDescRejection::PrimaryVidPnSourceNotConcrete`].
///
/// Everything else is identical, which is
/// why both stages run the same core rather than two hand-kept copies. Same
/// shape, and for the same reason, as [`crate::native_render::Hvm1Stage`] and
/// [`HeliosOuterCommandAllocationV1::validate_create_input`] /
/// [`HeliosOuterCommandAllocationV1::validate_create_output`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hwa2Stage {
    /// The bytes user mode supplies to `pfnAllocateCb`: the allocation
    /// generation is zero and neither KMD-owned flag bit is set.
    CreateInput,
    /// The bytes the KMD wrote back — and, byte for byte, what every later
    /// opener reads, because nothing writes the record again.
    CreateOutput,
}

/// Which scalar geometry field a [`HeliosAllocDescRejection`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosAllocDescField {
    Width,
    Height,
    DepthOrArraySize,
    MipLevels,
    SampleCount,
    SampleQuality,
}

/// Why [`HeliosWddmAllocationDescV2::validate`],
/// [`HeliosWddmAllocationDescV2::validate_create_input`] or
/// [`HeliosWddmAllocationDescV2::validate_create_output`] refused.
///
/// One named variant per rejection: a caller can raise a distinct counter for
/// every reason without re-deriving the check, and no refusal is a bare
/// `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosAllocDescRejection {
    /// The runtime/dxgkrnl private-data buffer is not exactly
    /// [`HELIOS_HWA2_BYTES`]. §10.3: "any malformed, unknown, **truncated**,
    /// mismatched-generation, or reserved-nonzero descriptor makes create/open
    /// fail" — this is the truncation arm, and it exists so no consumer
    /// hand-rolls the length check on a `(pPrivateDriverData, size)` pair.
    PrivateDataSize {
        found: usize,
        expected: usize,
    },
    /// Not an HWA2 record. Never fall back to a legacy parser.
    Magic {
        found: u32,
    },
    AbiVersion {
        found: u16,
    },
    StructSize {
        found: u16,
    },
    PackageGeneration {
        found: u64,
        expected: u64,
    },
    /// The KMD-assigned allocation generation is zero.
    AllocationGenerationZero,
    ByteSizeZero,
    ReservedNonZero {
        found: u32,
    },
    UnknownAllocationKind {
        found: u32,
    },
    /// A flag bit outside [`HELIOS_HWA2_FLAG_MASK`] is set.
    UnknownFlagBits {
        found: u32,
    },
    UnknownBindBits {
        found: u32,
    },
    UnknownMiscBits {
        found: u32,
    },
    UnknownSwizzleClass {
        found: u32,
    },
    UnknownMemoryClass {
        found: u32,
    },
    /// An image kind left a required geometry field zero.
    ImageGeometryZero {
        field: HeliosAllocDescField,
    },
    /// A non-image kind carried texel geometry it cannot have.
    NonImageGeometryNonZero {
        field: HeliosAllocDescField,
        found: u32,
    },
    /// An image kind carried `DXGI_FORMAT_UNKNOWN`.
    ImageDxgiFormatUnknown,
    /// A non-image kind carried a DXGI format.
    NonImageDxgiFormatSet {
        found: u32,
    },
    /// A non-image kind carried a D3DDDIFORMAT.
    NonImageD3dDdiFormatSet {
        found: u32,
    },
    PlaneCountOutOfRange {
        found: u32,
    },
    ImagePlaneCountZero,
    NonImagePlaneCountNonZero {
        found: u32,
    },
    /// A record at or above `plane_count` was not zero.
    UnusedPlaneNonZero {
        index: u32,
    },
    PlaneRowPitchZero {
        index: u32,
    },
    PlaneSlicePitchZero {
        index: u32,
    },
    /// `row_pitch > slice_pitch`: the plane cannot hold even one row.
    PlaneRowPitchExceedsSlicePitch {
        index: u32,
    },
    /// `offset + slice_pitch` overflowed `u64`.
    PlaneRangeOverflow {
        index: u32,
    },
    /// `offset + slice_pitch > byte_size`.
    PlaneRangeExceedsByteSize {
        index: u32,
    },
    /// `D3D12_RUNTIME_PRIMARY` without `PRIMARY` (C44 cross-validation).
    D3D12RuntimePrimaryWithoutPrimary,
    /// `D3D12_RUNTIME_PRIMARY` whose VidPn source is not
    /// [`D3DDDI_ID_UNINITIALIZED`] (C44 cross-validation).
    D3D12RuntimePrimaryNotSentinel {
        found: u32,
    },
    /// A conventional D3D11 primary carrying the sentinel instead of a concrete
    /// source.
    ///
    /// ⚠ [`Hwa2Stage::CreateOutput`] **only**. On the input stage the identical
    /// bytes — PRIMARY, the sentinel, and `D3D12_RUNTIME_PRIMARY` clear because
    /// the creator may not set a KMD-owned bit — are the legal C44 D3D12 request
    /// awaiting the kernel's stamp, and there is nothing in the record that could
    /// distinguish the two before the KMD reads the D3D12 create record. On the
    /// output side the bit has been decided, so a primary that still carries the
    /// sentinel without it is a stamp the KMD failed to write.
    PrimaryVidPnSourceNotConcrete,
    /// A non-primary carrying anything other than the sentinel.
    NonPrimaryVidPnSourceNotSentinel {
        found: u32,
    },
    /// `STANDARD` set but the OS standard-allocation type is zero.
    StandardAllocationTypeZero,
    /// `STANDARD` clear but a standard-allocation type is present.
    StandardAllocationTypeWithoutStandardFlag {
        found: u32,
    },
    /// A `STANDARD_*` kind without the `STANDARD` flag.
    StandardKindWithoutStandardFlag {
        kind: u32,
    },
    /// The `STANDARD` flag on a kind that has no texel geometry.
    StandardFlagOnNonImageKind {
        kind: u32,
    },
    DirectFlipWithoutPrimary,
    DirectFlipProtected,
    DirectFlipCrossAdapter,
    DirectFlipPlaneCountNotOne {
        found: u32,
    },
    DirectFlipUnsupportedSwizzleClass {
        found: u32,
    },
    /// `memory_class == CPU_VISIBLE` without the `CPU_VISIBLE` flag, or
    /// `memory_class == DEVICE_LOCAL` with it.
    MemoryClassCpuVisibilityMismatch {
        memory_class: u32,
        flags: u32,
    },
    // ⚠ Variants below are append-only additions made when HWA2 gained its
    // create-input stage. The order of this enum is its stable numbering for
    // KMD counters and ETW fields; never insert into the middle.
    /// [`Hwa2Stage::CreateInput`] carried a nonzero allocation generation. The
    /// KMD assigns it, so an input that already has one is a UMD inventing an
    /// identity — never an adoption. Identical rule, and identical name, to
    /// [`HeliosOuterCommandAllocRejection::AllocationGenerationNonZeroOnInput`].
    AllocationGenerationNonZeroOnInput {
        found: u64,
    },
    /// [`Hwa2Stage::CreateInput`] set a bit in
    /// [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`]. `bits` is the offending subset, so
    /// a counter can name which of `DIRECT_FLIP_COMPATIBLE` /
    /// `D3D12_RUNTIME_PRIMARY` the creator tried to assert for itself.
    KmdOwnedFlagSetOnInput {
        bits: u32,
    },
}

impl HeliosWddmAllocationDescV2 {
    /// A zeroed descriptor with only the identity header filled in.
    ///
    /// Deliberately **not** `Default`: an HWA2 record is only ever complete or
    /// refused, and a partially filled record must not look constructible.
    #[inline]
    pub const fn header(package_generation: u64, allocation_generation: u64) -> Self {
        Self {
            magic: HELIOS_HWA2_MAGIC,
            abi_version: HELIOS_HWA2_ABI_VERSION,
            struct_size: HELIOS_HWA2_BYTES,
            package_generation,
            allocation_generation,
            byte_size: 0,
            width: 0,
            height: 0,
            depth_or_array_size: 0,
            mip_levels: 0,
            dxgi_format: DXGI_FORMAT_UNKNOWN,
            d3d_ddi_format: 0,
            sample_count: 0,
            sample_quality: 0,
            allocation_kind: HELIOS_HWA2_KIND_INVALID,
            flags: 0,
            bind_flags: 0,
            misc_flags: 0,
            vidpn_source: D3DDDI_ID_UNINITIALIZED,
            standard_allocation_type: 0,
            swizzle_class: HELIOS_HWA2_SWIZZLE_INVALID,
            memory_class: HELIOS_HWA2_MEMORY_INVALID,
            plane_count: 0,
            reserved: 0,
            planes: [HeliosWddmPlaneRecordV2 {
                offset: 0,
                row_pitch: 0,
                slice_pitch: 0,
            }; 4],
        }
    }

    /// Read one descriptor out of a `(pPrivateDriverData, PrivateDriverDataSize)`
    /// pair that must be exactly [`HELIOS_HWA2_BYTES`] long.
    ///
    /// Owned and unaligned for the same reason as
    /// [`HeliosOuterSubmitV1::from_private_data`]: the runtime's buffer carries
    /// no alignment promise, and a KMD that read 168 bytes out of a shorter one
    /// would take an out-of-bounds kernel read this crate could not catch.
    /// Every consumer must enter through here rather than casting the pointer.
    pub fn from_private_data(bytes: &[u8]) -> Result<Self, HeliosAllocDescRejection> {
        let expected = core::mem::size_of::<Self>();
        if bytes.len() != expected {
            return Err(HeliosAllocDescRejection::PrivateDataSize {
                found: bytes.len(),
                expected,
            });
        }
        // With the length already exact, `try_pod_read_unaligned` cannot fail;
        // the arm is kept because a total function may not `unwrap`.
        bytemuck::try_pod_read_unaligned::<Self>(bytes).map_err(|_| {
            HeliosAllocDescRejection::PrivateDataSize {
                found: bytes.len(),
                expected,
            }
        })
    }

    /// Is a flag bit set? Convenience so callers never re-spell the mask.
    #[inline]
    pub const fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// Does this descriptor describe an image (texel geometry + DXGI format)?
    #[inline]
    pub const fn is_image(&self) -> bool {
        helios_hwa2_kind_is_image(self.allocation_kind)
    }

    /// Total validation of one descriptor at one create stage, in the order a
    /// reader must apply it: identity → generations → vocabulary → geometry →
    /// planes → cross-field rules (§10.3).
    ///
    /// This is the single cross-field core behind all three entry points
    /// ([`Self::validate`], [`Self::validate_create_input`],
    /// [`Self::validate_create_output`]), for the same reason
    /// `HeliosOuterCommandAllocationV1::validate_fixed` is HOC1's: three
    /// copies of a rule set is three chances to enforce a rule on one path and
    /// not another, and the path it would go missing from is the create path
    /// that produces the bytes every later opener trusts as `const`. Exactly
    /// two `match stage` arms below differ between the stages; everything else
    /// is common by construction rather than by review.
    ///
    /// Total function, no panic, no allocation: every arithmetic step is
    /// checked and every refusal is named.
    fn validate_stage(
        &self,
        package_generation: u64,
        stage: Hwa2Stage,
    ) -> Result<(), HeliosAllocDescRejection> {
        use HeliosAllocDescField as F;
        use HeliosAllocDescRejection as R;

        // ── identity ────────────────────────────────────────────────────────
        if self.magic != HELIOS_HWA2_MAGIC {
            return Err(R::Magic { found: self.magic });
        }
        if self.abi_version != HELIOS_HWA2_ABI_VERSION {
            return Err(R::AbiVersion {
                found: self.abi_version,
            });
        }
        if self.struct_size != HELIOS_HWA2_BYTES {
            return Err(R::StructSize {
                found: self.struct_size,
            });
        }
        // Zero is never a live package generation on *either* side: a zeroed
        // buffer must not be admitted just because the caller has not yet
        // established the package (crate-level rule, `crate::HELIOS_PACKAGE_GENERATION`).
        if self.package_generation != package_generation || self.package_generation == 0 {
            return Err(R::PackageGeneration {
                found: self.package_generation,
                expected: package_generation,
            });
        }
        // ── the one field the KMD alone writes ──────────────────────────────
        //
        // §10.3 gives `allocation_generation` no create-input meaning at all:
        // it is "nonzero KMD-assigned", so an input carrying one is a UMD that
        // invented an identity rather than requesting an allocation. HOC1
        // states the same rule at the same position for the same reason
        // (`AllocationGenerationNonZeroOnInput`), and the check stays here —
        // in the "generations" step, before any vocabulary check — so the
        // documented refusal order is one order for all three entry points.
        match stage {
            Hwa2Stage::CreateInput => {
                if self.allocation_generation != 0 {
                    return Err(R::AllocationGenerationNonZeroOnInput {
                        found: self.allocation_generation,
                    });
                }
            }
            Hwa2Stage::CreateOutput => {
                if self.allocation_generation == 0 {
                    return Err(R::AllocationGenerationZero);
                }
            }
        }
        if self.reserved != 0 {
            return Err(R::ReservedNonZero {
                found: self.reserved,
            });
        }
        if self.byte_size == 0 {
            return Err(R::ByteSizeZero);
        }

        // ── vocabulary ──────────────────────────────────────────────────────
        if self.allocation_kind == HELIOS_HWA2_KIND_INVALID
            || self.allocation_kind > HELIOS_HWA2_KIND_MAX
        {
            return Err(R::UnknownAllocationKind {
                found: self.allocation_kind,
            });
        }
        if self.flags & !HELIOS_HWA2_FLAG_MASK != 0 {
            return Err(R::UnknownFlagBits {
                found: self.flags & !HELIOS_HWA2_FLAG_MASK,
            });
        }
        // The KMD-owned pair, checked immediately after the mask because both
        // are facts about the same word. Unknown bits are refused first: a
        // garbage `flags` word is the worse failure and its reason carries more
        // information than "you set a bit you do not own".
        if matches!(stage, Hwa2Stage::CreateInput) {
            let kmd_owned = self.flags & HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
            if kmd_owned != 0 {
                return Err(R::KmdOwnedFlagSetOnInput { bits: kmd_owned });
            }
        }
        if self.bind_flags & !HELIOS_HWA2_BIND_MASK != 0 {
            return Err(R::UnknownBindBits {
                found: self.bind_flags & !HELIOS_HWA2_BIND_MASK,
            });
        }
        if self.misc_flags & !HELIOS_HWA2_MISC_MASK != 0 {
            return Err(R::UnknownMiscBits {
                found: self.misc_flags & !HELIOS_HWA2_MISC_MASK,
            });
        }
        if self.swizzle_class == HELIOS_HWA2_SWIZZLE_INVALID
            || self.swizzle_class > HELIOS_HWA2_SWIZZLE_MAX
        {
            return Err(R::UnknownSwizzleClass {
                found: self.swizzle_class,
            });
        }
        if self.memory_class == HELIOS_HWA2_MEMORY_INVALID
            || self.memory_class > HELIOS_HWA2_MEMORY_MAX
        {
            return Err(R::UnknownMemoryClass {
                found: self.memory_class,
            });
        }

        // ── geometry, per kind class ────────────────────────────────────────
        //
        // "zero only for a non-image allocation kind" read fail-closed in both
        // directions: an image must carry it, a non-image must not.
        let image = self.is_image();
        let geometry = [
            (F::Width, self.width),
            (F::Height, self.height),
            (F::DepthOrArraySize, self.depth_or_array_size),
            (F::MipLevels, self.mip_levels),
            (F::SampleCount, self.sample_count),
        ];
        let mut i = 0;
        while i < geometry.len() {
            let (field, value) = geometry[i];
            if image {
                if value == 0 {
                    return Err(R::ImageGeometryZero { field });
                }
            } else if value != 0 {
                return Err(R::NonImageGeometryNonZero {
                    field,
                    found: value,
                });
            }
            i += 1;
        }
        if !image && self.sample_quality != 0 {
            // Sample quality is meaningless without a sample count, and the
            // sample count is already required zero here.
            return Err(R::NonImageGeometryNonZero {
                field: F::SampleQuality,
                found: self.sample_quality,
            });
        }
        if image {
            if self.dxgi_format == DXGI_FORMAT_UNKNOWN {
                return Err(R::ImageDxgiFormatUnknown);
            }
        } else {
            if self.dxgi_format != DXGI_FORMAT_UNKNOWN {
                return Err(R::NonImageDxgiFormatSet {
                    found: self.dxgi_format,
                });
            }
            if self.d3d_ddi_format != 0 {
                return Err(R::NonImageD3dDdiFormatSet {
                    found: self.d3d_ddi_format,
                });
            }
        }

        // ── planes ──────────────────────────────────────────────────────────
        if self.plane_count > HELIOS_HWA2_MAX_PLANES {
            return Err(R::PlaneCountOutOfRange {
                found: self.plane_count,
            });
        }
        if image && self.plane_count == 0 {
            return Err(R::ImagePlaneCountZero);
        }
        if !image && self.plane_count != 0 {
            return Err(R::NonImagePlaneCountNonZero {
                found: self.plane_count,
            });
        }
        let mut p: u32 = 0;
        while p < HELIOS_HWA2_MAX_PLANES {
            let plane = self.planes[p as usize];
            if p >= self.plane_count {
                if plane.offset != 0 || plane.row_pitch != 0 || plane.slice_pitch != 0 {
                    return Err(R::UnusedPlaneNonZero { index: p });
                }
                p += 1;
                continue;
            }
            if plane.row_pitch == 0 {
                return Err(R::PlaneRowPitchZero { index: p });
            }
            if plane.slice_pitch == 0 {
                return Err(R::PlaneSlicePitchZero { index: p });
            }
            if plane.row_pitch > plane.slice_pitch {
                return Err(R::PlaneRowPitchExceedsSlicePitch { index: p });
            }
            let end = match plane.offset.checked_add(plane.slice_pitch as u64) {
                Some(end) => end,
                None => return Err(R::PlaneRangeOverflow { index: p }),
            };
            if end > self.byte_size {
                return Err(R::PlaneRangeExceedsByteSize { index: p });
            }
            p += 1;
        }

        // ── C44: the runtime-primary bit and the sentinel, cross-validated ──
        //
        // ⛔ Neither is inferred by an opener. Both are checked here, together,
        // so a reader can never accept one without the other.
        //
        // ⚠ One arm of this block — and only one — is genuinely stage-dependent,
        // and it is the third difference between the two stages (see
        // [`Hwa2Stage`]). `D3D12_RUNTIME_PRIMARY` is KMD-owned, so the create
        // *input* for a D3D12 runtime primary is necessarily PRIMARY + the
        // sentinel + the bit CLEAR: the input rule above refuses the bit by name,
        // so a D3D12 UMD has no other shape available to it. Requiring a concrete
        // VidPn source of every bit-less primary on the input stage therefore
        // refuses the only legal D3D12 request, and — worse than a refusal —
        // makes the kernel's own stamp unreachable by construction, because the
        // create never survives long enough for the KMD to decide the bit.
        //
        // That is not hypothetical; it is what this crate did until the seam
        // review enumerated the four reachable shapes against it: a D3D12 primary
        // input with the bit clear returned `PrimaryVidPnSourceNotConcrete`, the
        // same input with the bit set returned `KmdOwnedFlagSetOnInput`, and
        // `Ok` was reachable *only* for a record that had already been stamped —
        // i.e. only ever from the KMD's own output, never from a UMD.
        //
        // Nothing is weakened on the side every opener reads. On `CreateOutput`
        // the pair is still cross-validated in both directions: PRIMARY + the
        // sentinel still requires the bit (a stamped-looking record that lost the
        // bit is refused here), and the bit still requires PRIMARY + the
        // sentinel. An opener still infers neither from the other.
        let primary = self.has_flag(HELIOS_HWA2_FLAG_PRIMARY);
        if self.has_flag(HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY) {
            if !primary {
                return Err(R::D3D12RuntimePrimaryWithoutPrimary);
            }
            if self.vidpn_source != D3DDDI_ID_UNINITIALIZED {
                return Err(R::D3D12RuntimePrimaryNotSentinel {
                    found: self.vidpn_source,
                });
            }
        } else if primary {
            // Output: a primary without the bit is a D3D11 primary and must name
            // its concrete source — the sentinel here would mean the KMD echoed a
            // D3D12 primary back without stamping it, and every opener would then
            // read a primary belonging to no source at all.
            // Input: the same bytes are the D3D12 request awaiting that stamp.
            if matches!(stage, Hwa2Stage::CreateOutput)
                && self.vidpn_source == D3DDDI_ID_UNINITIALIZED
            {
                return Err(R::PrimaryVidPnSourceNotConcrete);
            }
        } else if self.vidpn_source != D3DDDI_ID_UNINITIALIZED {
            return Err(R::NonPrimaryVidPnSourceNotSentinel {
                found: self.vidpn_source,
            });
        }

        // ── standard-allocation coupling ────────────────────────────────────
        let standard = self.has_flag(HELIOS_HWA2_FLAG_STANDARD);
        if standard {
            if self.standard_allocation_type == 0 {
                return Err(R::StandardAllocationTypeZero);
            }
            if !image {
                return Err(R::StandardFlagOnNonImageKind {
                    kind: self.allocation_kind,
                });
            }
        } else {
            if self.standard_allocation_type != 0 {
                return Err(R::StandardAllocationTypeWithoutStandardFlag {
                    found: self.standard_allocation_type,
                });
            }
            if helios_hwa2_kind_is_standard(self.allocation_kind) {
                return Err(R::StandardKindWithoutStandardFlag {
                    kind: self.allocation_kind,
                });
            }
        }

        // ── Direct Flip eligibility (§10.3 paragraph after the table) ───────
        if self.has_flag(HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE) {
            if !primary {
                return Err(R::DirectFlipWithoutPrimary);
            }
            if self.has_flag(HELIOS_HWA2_FLAG_PROTECTED) {
                return Err(R::DirectFlipProtected);
            }
            if self.has_flag(HELIOS_HWA2_FLAG_CROSS_ADAPTER) {
                return Err(R::DirectFlipCrossAdapter);
            }
            if self.plane_count != 1 {
                return Err(R::DirectFlipPlaneCountNotOne {
                    found: self.plane_count,
                });
            }
            if !helios_hwa2_swizzle_is_direct_flip_capable(self.swizzle_class) {
                return Err(R::DirectFlipUnsupportedSwizzleClass {
                    found: self.swizzle_class,
                });
            }
        }

        // ── memory class vs CPU visibility ──────────────────────────────────
        let cpu_visible = self.has_flag(HELIOS_HWA2_FLAG_CPU_VISIBLE);
        let mismatch = match self.memory_class {
            HELIOS_HWA2_MEMORY_CPU_VISIBLE => !cpu_visible,
            HELIOS_HWA2_MEMORY_DEVICE_LOCAL => cpu_visible,
            _ => false,
        };
        if mismatch {
            return Err(R::MemoryClassCpuVisibilityMismatch {
                memory_class: self.memory_class,
                flags: self.flags,
            });
        }

        Ok(())
    }

    /// Open-time validation of the descriptor as the KMD wrote it.
    ///
    /// Unchanged in meaning and signature from before HWA2 had a create-input
    /// stage, and deliberately so: an opener and a create-output reader are
    /// asking the same question because they are reading the same bytes.
    /// `DxgkDdiOpenAllocation` never writes the record (§10.3), so what an
    /// opener sees is exactly what create produced — this is
    /// [`Self::validate_create_output`] under the name every open-path caller
    /// already uses.
    pub fn validate(&self, package_generation: u64) -> Result<(), HeliosAllocDescRejection> {
        self.validate_stage(package_generation, Hwa2Stage::CreateOutput)
    }

    /// KMD-side validation of the record the UMD supplied to `pfnAllocateCb`.
    ///
    /// The same total check as [`Self::validate`] with the two stage rules
    /// inverted: the allocation generation must be zero, and neither bit of
    /// [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] may be set. Every other field is
    /// UMD-supplied, validated here, and echoed verbatim into the output
    /// record — the KMD refuses the create rather than correcting a field
    /// (`docs/retirement/K4-CONTRACT.md` §1.1).
    ///
    /// Mirrors [`HeliosOuterCommandAllocationV1::validate_create_input`]; a
    /// reader who knows one knows this one.
    pub fn validate_create_input(
        &self,
        package_generation: u64,
    ) -> Result<(), HeliosAllocDescRejection> {
        self.validate_stage(package_generation, Hwa2Stage::CreateInput)
    }

    /// Validation of the complete record the KMD wrote back at create: the same
    /// total check plus the nonzero create-time generation stamp.
    ///
    /// The KMD calls it on its own output — the cheapest possible self-check
    /// that the 168 bytes about to become immutable are the ones every opener
    /// will accept — and a UMD calls it on the buffer that came back. Mirrors
    /// [`HeliosOuterCommandAllocationV1::validate_create_output`].
    pub fn validate_create_output(
        &self,
        package_generation: u64,
    ) -> Result<(), HeliosAllocDescRejection> {
        self.validate_stage(package_generation, Hwa2Stage::CreateOutput)
    }
}

/// Whether the caller has already proven both resources live on the exact same
/// one-node adapter/LUID. Not a `bool`: §3 forbids cross-adapter inference, so
/// the caller must state the finding rather than pass an unlabelled flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosAdapterMatch {
    /// Both wrappers resolve to the identical adapter LUID.
    SameAdapter,
    /// They do not, or the caller could not prove it.
    DifferentOrUnknown,
}

/// Why [`direct_flip_pair_admissible`] refused (C43/C44).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosDirectFlipRefusal {
    /// One of the two descriptors is not a valid HWA2 record.
    AppDescriptorInvalid(HeliosAllocDescRejection),
    DwmDescriptorInvalid(HeliosAllocDescRejection),
    /// `CheckDirectFlipFlags` was nonzero: the first generation refuses
    /// `IMMEDIATE`.
    ImmediateFlipRequested {
        flags: u32,
    },
    NotSameAdapter,
    /// A descriptor lacks `PRIMARY|DISPLAYABLE|DIRECT_FLIP_COMPATIBLE`.
    MissingRequiredFlags {
        flags: u32,
    },
    /// `STEREO`, `PROTECTED`, or `CROSS_ADAPTER` present on either descriptor.
    ForbiddenFlags {
        flags: u32,
    },
    NotBgra8 {
        found: u32,
    },
    ExtentMismatchOrZero,
    PlaneCountNotOne {
        found: u32,
    },
    MipOrLayerNotOne,
    SampleProfileNotOneZero,
    SwizzleClassMismatch {
        app: u32,
        dwm: u32,
    },
    UnsupportedSwizzleClass {
        found: u32,
    },
    /// Two conventional D3D11 primaries whose concrete sources differ.
    ConcreteVidPnSourceMismatch {
        app: u32,
        dwm: u32,
    },
}

/// C43/C44 static resource-pair admission for D3D11 `pfnCheckDirectFlipSupport`
/// (§10.5 promotion paragraph, doc lines 2726-2742).
///
/// This is *only* the static pair check. The later classic
/// `DxgkDdiSetVidPnSourceAddress` / `CheckMPO3` / `SetMPO3` path independently
/// validates the actual OS-supplied source, mode, and plane attributes. The UMD
/// never invokes an Escape callback to ask KMD.
///
/// The C44 branch: if either resource is a `D3D12_RUNTIME_PRIMARY`, its VidPn
/// field must be [`D3DDDI_ID_UNINITIALIZED`] — already enforced by
/// [`HeliosWddmAllocationDescV2::validate`] — and that sentinel means *any*
/// source on this exact adapter, so it is never compared as a concrete
/// identity. Two conventional D3D11 primaries must have equal, non-sentinel
/// sources.
pub fn direct_flip_pair_admissible(
    app: &HeliosWddmAllocationDescV2,
    dwm: &HeliosWddmAllocationDescV2,
    package_generation: u64,
    adapter_match: HeliosAdapterMatch,
    check_direct_flip_flags: u32,
) -> Result<(), HeliosDirectFlipRefusal> {
    use HeliosDirectFlipRefusal as R;

    if let Err(e) = app.validate(package_generation) {
        return Err(R::AppDescriptorInvalid(e));
    }
    if let Err(e) = dwm.validate(package_generation) {
        return Err(R::DwmDescriptorInvalid(e));
    }
    if check_direct_flip_flags != 0 {
        return Err(R::ImmediateFlipRequested {
            flags: check_direct_flip_flags,
        });
    }
    if adapter_match != HeliosAdapterMatch::SameAdapter {
        return Err(R::NotSameAdapter);
    }

    const REQUIRED: u32 = HELIOS_HWA2_FLAG_PRIMARY
        | HELIOS_HWA2_FLAG_DISPLAYABLE
        | HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
    const FORBIDDEN: u32 =
        HELIOS_HWA2_FLAG_STEREO | HELIOS_HWA2_FLAG_PROTECTED | HELIOS_HWA2_FLAG_CROSS_ADAPTER;

    for d in [app, dwm] {
        if d.flags & REQUIRED != REQUIRED {
            return Err(R::MissingRequiredFlags { flags: d.flags });
        }
        if d.flags & FORBIDDEN != 0 {
            return Err(R::ForbiddenFlags {
                flags: d.flags & FORBIDDEN,
            });
        }
        if d.dxgi_format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(R::NotBgra8 {
                found: d.dxgi_format,
            });
        }
        if d.plane_count != 1 {
            return Err(R::PlaneCountNotOne {
                found: d.plane_count,
            });
        }
        if d.mip_levels != 1 || d.depth_or_array_size != 1 {
            return Err(R::MipOrLayerNotOne);
        }
        if d.sample_count != 1 || d.sample_quality != 0 {
            return Err(R::SampleProfileNotOneZero);
        }
        if !helios_hwa2_swizzle_is_direct_flip_capable(d.swizzle_class) {
            return Err(R::UnsupportedSwizzleClass {
                found: d.swizzle_class,
            });
        }
    }

    if app.width == 0 || app.height == 0 || app.width != dwm.width || app.height != dwm.height {
        return Err(R::ExtentMismatchOrZero);
    }
    if app.swizzle_class != dwm.swizzle_class {
        return Err(R::SwizzleClassMismatch {
            app: app.swizzle_class,
            dwm: dwm.swizzle_class,
        });
    }

    // C44: the sentinel is never compared as a concrete identity; only two
    // concrete D3D11 sources are compared, and they must be equal.
    let app_runtime = app.has_flag(HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY);
    let dwm_runtime = dwm.has_flag(HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY);
    if !app_runtime && !dwm_runtime && app.vidpn_source != dwm.vidpn_source {
        return Err(R::ConcreteVidPnSourceMismatch {
            app: app.vidpn_source,
            dwm: dwm.vidpn_source,
        });
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// CRC64-ECMA — the HOB1/HOS1 corruption check
// ─────────────────────────────────────────────────────────────────────────────
//
// HELIOS_PRESENT_SYNC_RETIREMENT.md §10.4 names "CRC64-ECMA" for the HOB1
// header field at offset 80 and its HOS1 echo at offset 48. The definition used
// here is **CRC-64/ECMA-182** exactly as published in ECMA-182 Annex B:
//
//     width=64  poly=0x42F0E1EBA9EA3693  init=0x0000000000000000
//     refin=false  refout=false  xorout=0x0000000000000000
//     check("123456789") = 0x6C40DF5F0B497347
//
// ⚠ This is NOT the reflected "CRC-64/XZ" (a.k.a. CRC-64/GO-ECMA) variant,
// which shares the polynomial but uses init/xorout = 0xFFFF_FFFF_FFFF_FFFF and
// reflects both directions. The two disagree on every nonempty input, so the
// choice is stated here rather than left to whichever library a mirror picks.
//
// ⚠ Consequence of init=0/xorout=0: the CRC of an all-zero buffer is zero. That
// is harmless for HOB1 — the record can never be all zero because offset 0 must
// hold the magic — but a C mirror must not "sanity check" a nonzero CRC.

/// CRC-64/ECMA-182 generator polynomial (normal, MSB-first form).
pub const HELIOS_CRC64_ECMA182_POLY: u64 = 0x42F0_E1EB_A9EA_3693;
/// CRC-64/ECMA-182 initial register value.
pub const HELIOS_CRC64_ECMA182_INIT: u64 = 0x0000_0000_0000_0000;
/// CRC-64/ECMA-182 final XOR value.
pub const HELIOS_CRC64_ECMA182_XOROUT: u64 = 0x0000_0000_0000_0000;
/// Published check value: the CRC of the ASCII string `123456789`. The unit
/// test derives this independently from a bitwise reference implementation, so
/// a table typo cannot pass.
pub const HELIOS_CRC64_ECMA182_CHECK: u64 = 0x6C40_DF5F_0B49_7347;

/// Byte-at-a-time table, generated at compile time from the polynomial. Built
/// in a `const` block so there is one definition of the algorithm: the table
/// cannot drift from [`HELIOS_CRC64_ECMA182_POLY`].
const CRC64_ECMA182_TABLE: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut n = 0usize;
    while n < 256 {
        let mut c = (n as u64) << 56;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 0x8000_0000_0000_0000 != 0 {
                (c << 1) ^ HELIOS_CRC64_ECMA182_POLY
            } else {
                c << 1
            };
            bit += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// Fold `bytes` into a running CRC-64/ECMA-182 register.
///
/// `const fn`, `no_std`, allocation-free, and panic-free: the loop index is
/// bounded by `bytes.len()` and the table index is a `u8`, so neither can be
/// out of range.
#[inline]
pub const fn crc64_ecma_update(crc: u64, bytes: &[u8]) -> u64 {
    let mut crc = crc;
    let mut i = 0usize;
    while i < bytes.len() {
        let idx = (((crc >> 56) as u8) ^ bytes[i]) as usize;
        crc = CRC64_ECMA182_TABLE[idx] ^ (crc << 8);
        i += 1;
    }
    crc
}

/// CRC-64/ECMA-182 over a complete byte range.
#[inline]
pub const fn crc64_ecma(bytes: &[u8]) -> u64 {
    crc64_ecma_update(HELIOS_CRC64_ECMA182_INIT, bytes) ^ HELIOS_CRC64_ECMA182_XOROUT
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.4 — HOB1: one complete contiguous translated outer command
// ─────────────────────────────────────────────────────────────────────────────

/// `HOB1` — magic of [`HeliosOuterBatchV1`] (§10.4, offset 0).
pub const HELIOS_HOB1_MAGIC: u32 = 0x3142_4F48;
/// HOB1 ABI version (§10.4, offset 4).
pub const HELIOS_HOB1_ABI_VERSION: u16 = 1;
/// HOB1 header size in bytes (§10.4, offset 6).
pub const HELIOS_HOB1_HEADER_BYTES: u16 = 112;
/// Byte offset of the CRC64 field, which is folded in as zero when computing
/// the record's own checksum (§10.4, offset 80).
pub const HELIOS_HOB1_CRC_FIELD_OFFSET: usize = 80;

/// Size of one [`HeliosOuterBatchUseV1`] record (§10.4).
pub const HELIOS_HOB1_USE_RECORD_BYTES: u32 = 40;
/// Size of one [`HeliosOuterBatchOperandV1`] record (§10.4).
pub const HELIOS_HOB1_OPERAND_RECORD_BYTES: u32 = 16;
/// Maximum use records in one HOB1 (§10.4, offset 68). The generated D3D11
/// profile proves one indivisible operation has at most this many unique
/// allocation uses; if that inequality fails, the associated cap is not
/// exposed.
pub const HELIOS_HOB1_MAX_USE_RECORDS: u32 = 4096;
/// Maximum typed operand records in one HOB1 (§10.4, offset 76).
pub const HELIOS_HOB1_MAX_OPERAND_RECORDS: u32 = 8192;
/// Maximum total bytes of one HOB1 record, header through payload (§10.4,
/// offset 48; §10.6 repeats it for the C65 pool).
pub const HELIOS_HOB1_MAX_BYTES: u64 = 15 * 1024 * 1024;
/// Alignment every HOB1 interior offset must satisfy.
///
/// §10.4 says "aligned" without a number. Eight is the conservative reading:
/// it is the natural alignment of the 40-byte use record (which contains
/// `u64`s), it is a multiple of the 16-byte operand record's own requirement,
/// and both record strides preserve it, so a table that starts 8-aligned stays
/// 8-aligned to its last element.
pub const HELIOS_HOB1_OFFSET_ALIGNMENT: u64 = 8;

/// [`HeliosOuterBatchV1::flags`] — a D3D11 batch on a legacy physical Render
/// context; every use record is an allocation-list index (§10.4, offset 44).
pub const HELIOS_HOB1_FLAG_D3D11_PHYSICAL: u32 = 1;
/// [`HeliosOuterBatchV1::flags`] — a D3D12 batch on a virtual context; every
/// use record is a GPUVA (§10.4, offset 44).
pub const HELIOS_HOB1_FLAG_D3D12_VIRTUAL: u32 = 2;

/// [`HeliosOuterBatchUseV1::identity_kind`] — a D3D11 allocation-list index
/// whose upper 32 address bits are zero (§10.4).
pub const HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX: u16 = 1;
/// [`HeliosOuterBatchUseV1::identity_kind`] — a D3D12 GPUVA (§10.4).
pub const HELIOS_HOB1_IDENTITY_D3D12_GPUVA: u16 = 2;

/// [`HeliosOuterBatchUseV1::access_flags`] — the batch reads the allocation.
pub const HELIOS_HOB1_ACCESS_READ: u32 = 1;
/// [`HeliosOuterBatchUseV1::access_flags`] — the batch writes the allocation.
pub const HELIOS_HOB1_ACCESS_WRITE: u32 = 2;
/// [`HeliosOuterBatchUseV1::access_flags`] — the batch writes a primary.
/// ⚠ Primary **implies** write (§10.4); the validator enforces it rather than
/// inferring it.
pub const HELIOS_HOB1_ACCESS_PRIMARY_WRITE: u32 = 4;
/// The only access bits that exist. Any other bit is a hard reject.
pub const HELIOS_HOB1_ACCESS_MASK: u32 =
    HELIOS_HOB1_ACCESS_READ | HELIOS_HOB1_ACCESS_WRITE | HELIOS_HOB1_ACCESS_PRIMARY_WRITE;

/// [`HeliosOuterBatchOperandV1::operand_kind`] — not a member; refused by name.
pub const HELIOS_HOB1_OPERAND_KIND_INVALID: u16 = 0;
/// [`HeliosOuterBatchOperandV1::operand_kind`] — a generated resource operand
/// whose payload bytes are zero and which the consumer rewrites to a
/// device-local capability. ⛔ This is the *only* kind: "arbitrary patches and
/// raw renderer IDs are rejected" (§10.4).
pub const HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE: u16 = 1;
/// Highest defined operand kind.
pub const HELIOS_HOB1_OPERAND_KIND_MAX: u16 = HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE;

/// The two encoded operand widths a generated Venus resource operand can have,
/// in bytes. Anything else is refused: the offset/type/width triple must
/// identify an operand in the generated, fully parsed opcode schema.
pub const HELIOS_HOB1_OPERAND_WIDTH_4: u16 = 4;
/// See [`HELIOS_HOB1_OPERAND_WIDTH_4`].
pub const HELIOS_HOB1_OPERAND_WIDTH_8: u16 = 8;

/// Required alignment of [`HeliosOuterBatchOperandV1::payload_offset`].
///
/// §10.4 does not print a number for the operand offset the way it does for the
/// tables, but the payload it indexes is a Venus command stream, which is
/// encoded in 4-byte units; every generated resource operand — of either width
/// — therefore starts 4-byte aligned. This is the same constant and the same
/// reason as the native lane's
/// [`crate::native_render::HELIOS_HNR2_OPERAND_ALIGN`], and it is what makes
/// "arbitrary patches … are rejected" (§10.4) enforceable: without it an
/// operand at an odd offset would let a consumer rewrite a capability ordinal
/// across two adjacent Venus command words.
pub const HELIOS_HOB1_OPERAND_ALIGN: u32 = 4;

/// The 112-byte header of one complete contiguous translated outer command
/// (`HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.4).
///
/// The header is followed, inside the same record, by the bounded use table,
/// the typed-operand table, and the sealed Venus payload. **All interior
/// offsets are from the HOB1 start** and the three regions are non-overlapping
/// and wholly inside [`Self::total_bytes`].
///
/// # What the generation fields are, and are not
///
/// [`Self::session_generation`], [`Self::context_generation`], and
/// [`Self::endpoint_id`] repeat values the KMD already stored in the live
/// context object at `DxgkDdiCreateContext` time. They exist **only** as an
/// anti-stale cross-check: the live KMD context object remains the identity and
/// no later lookup uses the numeric generation (§10.4). No HOB1 record orders
/// another WDDM context — [`Self::batch_id`] is strictly increasing *only on
/// this context*, and cross-context order exists solely through explicit
/// native-fence dependencies.
///
/// # Fragmentation
///
/// There is none. "Every piece is independently complete, fits its current
/// outer command buffer, repeats its exact use/operand tables, and is submitted
/// in the owning context's order; no HOB1 is fragmented across two outer WDDM
/// submissions."
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosOuterBatchV1 {
    /// `== HELIOS_HOB1_MAGIC` (offset 0).
    pub magic: u32,
    /// `== HELIOS_HOB1_ABI_VERSION` (offset 4).
    pub abi_version: u16,
    /// `== HELIOS_HOB1_HEADER_BYTES` (offset 6).
    pub header_size: u16,
    /// Exact atomic package generation (offset 8).
    pub package_generation: u64,
    /// Exact live HTS1 session generation, nonzero (offset 16).
    pub session_generation: u64,
    /// Exact HQA1-attached outer context generation, nonzero (offset 24).
    pub context_generation: u64,
    /// Context-local batch ID: nonzero, strictly increasing **only on this
    /// context** (offset 32).
    pub batch_id: u64,
    /// Exact direct endpoint reference, nonzero (offset 40).
    pub endpoint_id: u32,
    /// Exactly one of [`HELIOS_HOB1_FLAG_D3D11_PHYSICAL`] /
    /// [`HELIOS_HOB1_FLAG_D3D12_VIRTUAL`] (offset 44).
    pub flags: u32,
    /// Header through payload: nonzero, at most [`HELIOS_HOB1_MAX_BYTES`], and
    /// no larger than the current runtime-approved command buffer (offset 48).
    pub total_bytes: u64,
    /// Byte offset of the Venus payload from the HOB1 start: aligned, and after
    /// both tables (offset 56).
    pub payload_offset: u32,
    /// Exact finite Venus command size (offset 60).
    pub payload_bytes: u32,
    /// Byte offset of the use table from the HOB1 start: aligned, after the
    /// header (offset 64). Zero when [`Self::use_count`] is zero.
    pub use_offset: u32,
    /// Use-record count, at most [`HELIOS_HOB1_MAX_USE_RECORDS`] (offset 68).
    pub use_count: u32,
    /// Byte offset of the typed-operand table from the HOB1 start: aligned,
    /// non-overlapping (offset 72). Zero when [`Self::operand_count`] is zero.
    pub operand_offset: u32,
    /// Operand-record count, at most [`HELIOS_HOB1_MAX_OPERAND_RECORDS`]
    /// (offset 76).
    pub operand_count: u32,
    /// CRC64-ECMA of the whole record with this field zero (offset 80).
    /// ⚠ A corruption check, **not** an identity.
    pub crc64: u64,
    /// Reserved; zero (offset 88, 24 bytes).
    pub reserved: [u8; 24],
}

/// One 40-byte HOB1 use record (§10.4):
/// `{u64 addressOrIndex, u64 byteLength, u64 expectedAllocationGeneration,
///   u32 accessFlags, u16 identityKind, u16 operandCount, u32 firstOperand,
///   u32 reservedZero}`.
///
/// The use table is the complete unique-allocation closure of every operation
/// in the batch. A D3D12 descriptor table does **not** expand into one record
/// per possibly indexed descriptor: the HOB1 names the exact descriptor-heap /
/// root / direct GPUVAs, and the HTS1-local generated Venus object graph
/// retains the allocation generations and object refs.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosOuterBatchUseV1 {
    /// Type 1: a D3D11 allocation-list index, upper 32 bits zero.
    /// Type 2: a D3D12 GPUVA.
    pub address_or_index: u64,
    /// Byte length of the used range.
    pub byte_length: u64,
    /// The [`HeliosWddmAllocationDescV2::allocation_generation`] the encoder saw.
    /// A mismatch at the consumer is a stale batch, not a lookup miss.
    pub expected_allocation_generation: u64,
    /// `HELIOS_HOB1_ACCESS_*`.
    pub access_flags: u32,
    /// `HELIOS_HOB1_IDENTITY_*`.
    pub identity_kind: u16,
    /// Number of typed operands belonging to this use.
    pub operand_count: u16,
    /// Index of this use's first operand in the batch's operand table.
    pub first_operand: u32,
    /// Reserved; zero.
    pub reserved: u32,
}

/// One 16-byte HOB1 typed operand record (§10.4):
/// `{u32 payloadOffset, u32 useIndex, u16 operandKind, u16 encodedWidth,
///   u32 reservedZero}`.
///
/// ⛔ "It identifies only a generated resource operand whose payload bytes are
/// zero; arbitrary patches and raw renderer IDs are rejected." The encoder
/// writes zero into every host-resource-id operand and the consumer rewrites it
/// to a device-local capability ordinal; a host resource ID never travels on
/// the wire.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosOuterBatchOperandV1 {
    /// Byte offset of the operand **from the HOB1 start** (§10.4: "all offsets
    /// are from the HOB1 start"), which must land wholly inside the payload
    /// region.
    pub payload_offset: u32,
    /// Index into the batch's use table.
    pub use_index: u32,
    /// `HELIOS_HOB1_OPERAND_KIND_*`.
    pub operand_kind: u16,
    /// Encoded operand width in bytes: 4 or 8.
    pub encoded_width: u16,
    /// Reserved; zero.
    pub reserved: u32,
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md §10.4 — the 112-byte header, the 40-byte
// use record, and the 16-byte typed operand, offset by offset.
const _: () = {
    assert!(core::mem::size_of::<HeliosOuterBatchV1>() == 112);
    assert!(core::mem::size_of::<HeliosOuterBatchV1>() == HELIOS_HOB1_HEADER_BYTES as usize);
    assert!(core::mem::align_of::<HeliosOuterBatchV1>() == 8);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, header_size) == 6);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, session_generation) == 16);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, context_generation) == 24);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, batch_id) == 32);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, endpoint_id) == 40);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, flags) == 44);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, total_bytes) == 48);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, payload_offset) == 56);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, payload_bytes) == 60);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, use_offset) == 64);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, use_count) == 68);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, operand_offset) == 72);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, operand_count) == 76);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, crc64) == 80);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, crc64) == HELIOS_HOB1_CRC_FIELD_OFFSET);
    assert!(core::mem::offset_of!(HeliosOuterBatchV1, reserved) == 88);

    assert!(core::mem::size_of::<HeliosOuterBatchUseV1>() == 40);
    assert!(core::mem::size_of::<HeliosOuterBatchUseV1>() == HELIOS_HOB1_USE_RECORD_BYTES as usize);
    assert!(core::mem::align_of::<HeliosOuterBatchUseV1>() == 8);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, address_or_index) == 0);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, byte_length) == 8);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, expected_allocation_generation) == 16);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, access_flags) == 24);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, identity_kind) == 28);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, operand_count) == 30);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, first_operand) == 32);
    assert!(core::mem::offset_of!(HeliosOuterBatchUseV1, reserved) == 36);

    assert!(core::mem::size_of::<HeliosOuterBatchOperandV1>() == 16);
    assert!(
        core::mem::size_of::<HeliosOuterBatchOperandV1>()
            == HELIOS_HOB1_OPERAND_RECORD_BYTES as usize
    );
    assert!(core::mem::align_of::<HeliosOuterBatchOperandV1>() == 4);
    assert!(core::mem::offset_of!(HeliosOuterBatchOperandV1, payload_offset) == 0);
    assert!(core::mem::offset_of!(HeliosOuterBatchOperandV1, use_index) == 4);
    assert!(core::mem::offset_of!(HeliosOuterBatchOperandV1, operand_kind) == 8);
    assert!(core::mem::offset_of!(HeliosOuterBatchOperandV1, encoded_width) == 10);
    assert!(core::mem::offset_of!(HeliosOuterBatchOperandV1, reserved) == 12);

    // The bound arithmetic in this module adds/multiplies u32-derived values in
    // u64. That is overflow-free only while the strides stay small; pin it.
    assert!(HELIOS_HOB1_USE_RECORD_BYTES <= 64);
    assert!(HELIOS_HOB1_OPERAND_RECORD_BYTES <= 64);
    assert!(HELIOS_HOB1_MAX_BYTES == 15_728_640);
    // A whole record must fit the C65 pool with room for its 64-KiB alignment.
    assert!(HELIOS_HOB1_MAX_BYTES < HELIOS_HOC1_POOL_BYTES);
    // `validate_batch_record`'s uniqueness bitmap is `[u64; MAX/64]`. If the
    // maximum ever stopped being a multiple of 64 the array would silently
    // become one word short and legal high indices would be misreported as
    // out-of-range, so the sizing is asserted rather than assumed.
    assert!(HELIOS_HOB1_MAX_USE_RECORDS % 64 == 0);
    // 4096 bits is 512 bytes of PASSIVE_LEVEL stack; a KMD kernel stack is
    // 12 KiB, so pin the order of magnitude too.
    assert!((HELIOS_HOB1_MAX_USE_RECORDS / 64) as usize * 8 <= 1024);
    // A generated resource operand is at least as wide as its alignment, so a
    // 4-aligned offset with either legal width stays inside one Venus dword
    // boundary pair.
    assert!(HELIOS_HOB1_OPERAND_WIDTH_4 as u32 % HELIOS_HOB1_OPERAND_ALIGN == 0);
    assert!(HELIOS_HOB1_OPERAND_WIDTH_8 as u32 % HELIOS_HOB1_OPERAND_ALIGN == 0);

    // The magic is four ASCII bytes read little-endian. A transposed hex digit
    // in the literal above would otherwise compile clean, pass every unit test
    // that builds records through the constructors, and fail only against the C
    // mirror or a live host.
    assert!(HELIOS_HOB1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HOB1_MAGIC.to_le_bytes()[1] == b'O');
    assert!(HELIOS_HOB1_MAGIC.to_le_bytes()[2] == b'B');
    assert!(HELIOS_HOB1_MAGIC.to_le_bytes()[3] == b'1');
};

/// The [`HeliosOuterBatchV1`] identity a validated use record carries.
///
/// Returned by [`HeliosOuterBatchUseV1::validate`] so a caller cannot read
/// `address_or_index` without having gone through the type check that says
/// which of the two things it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosUseIdentity {
    /// `identityKind=1`: an index into the D3D11 `D3DDDI_ALLOCATIONLIST` that
    /// dxgkrnl resolves for KMD during Render/Patch.
    D3D11AllocationListIndex(u32),
    /// `identityKind=2`: a D3D12 GPUVA that KMD resolves through the exact live
    /// device-scoped outer allocation/GPUVA association.
    D3D12GpuVirtualAddress(u64),
}

/// Why [`HeliosOuterBatchUseV1::validate`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosUseRecordRejection {
    UnknownIdentityKind {
        found: u16,
    },
    /// A type-2 GPUVA in a D3D11 physical batch, or a type-1 index in a D3D12
    /// virtual batch. ⛔ The batch's arm decides; a record never re-selects it.
    IdentityKindWrongForBatch {
        batch_flags: u32,
        identity_kind: u16,
    },
    /// `identityKind=1` with nonzero upper 32 address bits.
    D3D11IndexUpperBitsSet {
        found: u64,
    },
    /// `identityKind=1` naming an entry past the end of the `D3DDDI_ALLOCATIONLIST`
    /// dxgkrnl supplied for this Render. ⛔ The record is user-mode data and the
    /// consumer indexes a kernel array with it, so this bound is load-bearing,
    /// not a sanity check — it is the D3D11 analogue of
    /// [`crate::native_render::Hnr2TableReject::AllocationIndexOutOfRange`].
    D3D11IndexOutsideAllocationList {
        found: u32,
        allocation_list_count: u32,
    },
    D3D12GpuVaZero,
    /// `identityKind=2` whose `address_or_index + byte_length` wraps `u64`. The
    /// host resolver computes that end from the exact submitted process page
    /// tables (§10.4), so a wrapping range must never reach it.
    D3D12GpuVaRangeOverflow {
        address: u64,
        byte_length: u64,
    },
    ByteLengthZero,
    ExpectedAllocationGenerationZero,
    AccessFlagsZero,
    UnknownAccessBits {
        found: u32,
    },
    /// `PRIMARY_WRITE` without `WRITE`: primary implies write.
    PrimaryWriteWithoutWrite,
    ReservedNonZero {
        found: u32,
    },
    /// `first_operand` is nonzero while `operand_count` is zero. An empty run
    /// names no operand, so its cursor is zero — and [`validate_batch_record`]'s
    /// tiling walk therefore skips it rather than expecting it to continue the
    /// previous run.
    FirstOperandWithoutOperands {
        first_operand: u32,
    },
    /// `first_operand + operand_count` leaves the batch's operand table.
    OperandRangeOutsideTable {
        first_operand: u32,
        operand_count: u16,
        batch_operand_count: u32,
    },
}

impl HeliosOuterBatchUseV1 {
    /// The D3D11-type-1 versus D3D12-type-2 identity validator (§10.4, §17.1).
    ///
    /// `batch_flags` is the owning [`HeliosOuterBatchV1::flags`] arm,
    /// `batch_operand_count` its [`HeliosOuterBatchV1::operand_count`], and
    /// `allocation_list_count` the `D3DDDI_ALLOCATIONLIST` length dxgkrnl
    /// supplied for **this** Render (zero on the D3D12 virtual arm, which has no
    /// allocation list). Total function; every refusal is named; on success the
    /// caller receives the identity already discriminated **and already bounded**
    /// — that is the whole point of returning [`HeliosUseIdentity`] rather than a
    /// raw `u64`, so a consumer can index its allocation list without repeating
    /// the check.
    pub fn validate(
        &self,
        batch_flags: u32,
        batch_operand_count: u32,
        allocation_list_count: u32,
    ) -> Result<HeliosUseIdentity, HeliosUseRecordRejection> {
        use HeliosUseRecordRejection as R;

        if self.reserved != 0 {
            return Err(R::ReservedNonZero {
                found: self.reserved,
            });
        }
        if self.byte_length == 0 {
            return Err(R::ByteLengthZero);
        }
        if self.expected_allocation_generation == 0 {
            return Err(R::ExpectedAllocationGenerationZero);
        }

        // Access bits: only READ/WRITE/PRIMARY_WRITE exist, and primary implies
        // write. A use with no access is not a use.
        if self.access_flags & !HELIOS_HOB1_ACCESS_MASK != 0 {
            return Err(R::UnknownAccessBits {
                found: self.access_flags & !HELIOS_HOB1_ACCESS_MASK,
            });
        }
        if self.access_flags == 0 {
            return Err(R::AccessFlagsZero);
        }
        if self.access_flags & HELIOS_HOB1_ACCESS_PRIMARY_WRITE != 0
            && self.access_flags & HELIOS_HOB1_ACCESS_WRITE == 0
        {
            return Err(R::PrimaryWriteWithoutWrite);
        }

        // Operand slice bounds. Both operands are u32-derived, so the u64 sum
        // cannot overflow.
        if self.operand_count == 0 {
            if self.first_operand != 0 {
                return Err(R::FirstOperandWithoutOperands {
                    first_operand: self.first_operand,
                });
            }
        } else if self.first_operand as u64 + self.operand_count as u64 > batch_operand_count as u64
        {
            return Err(R::OperandRangeOutsideTable {
                first_operand: self.first_operand,
                operand_count: self.operand_count,
                batch_operand_count,
            });
        }

        // Identity: the batch's arm decides which kind is legal here.
        let required = match batch_flags {
            HELIOS_HOB1_FLAG_D3D11_PHYSICAL => HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX,
            HELIOS_HOB1_FLAG_D3D12_VIRTUAL => HELIOS_HOB1_IDENTITY_D3D12_GPUVA,
            _ => {
                return Err(R::IdentityKindWrongForBatch {
                    batch_flags,
                    identity_kind: self.identity_kind,
                })
            }
        };
        match self.identity_kind {
            HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX | HELIOS_HOB1_IDENTITY_D3D12_GPUVA => {}
            found => return Err(R::UnknownIdentityKind { found }),
        }
        if self.identity_kind != required {
            return Err(R::IdentityKindWrongForBatch {
                batch_flags,
                identity_kind: self.identity_kind,
            });
        }

        if required == HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX {
            if self.address_or_index >> 32 != 0 {
                return Err(R::D3D11IndexUpperBitsSet {
                    found: self.address_or_index,
                });
            }
            let index = self.address_or_index as u32;
            // "D3D11 KMD converts type 1 to DMA-local physical capabilities
            // during Render/Patch" (§10.4) — i.e. it indexes the runtime's
            // `D3DDDI_ALLOCATIONLIST` with this value from inside the kernel.
            if index >= allocation_list_count {
                return Err(R::D3D11IndexOutsideAllocationList {
                    found: index,
                    allocation_list_count,
                });
            }
            Ok(HeliosUseIdentity::D3D11AllocationListIndex(index))
        } else {
            if self.address_or_index == 0 {
                return Err(R::D3D12GpuVaZero);
            }
            // KMD resolves `[address, +byte_length)` through the exact live
            // allocation association; a wrapping range cannot name one extent.
            if self.address_or_index.checked_add(self.byte_length).is_none() {
                return Err(R::D3D12GpuVaRangeOverflow {
                    address: self.address_or_index,
                    byte_length: self.byte_length,
                });
            }
            Ok(HeliosUseIdentity::D3D12GpuVirtualAddress(
                self.address_or_index,
            ))
        }
    }
}

/// Why [`HeliosOuterBatchOperandV1::validate`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosOperandRecordRejection {
    UnknownOperandKind {
        found: u16,
    },
    UnsupportedEncodedWidth {
        found: u16,
    },
    UseIndexOutsideTable {
        found: u32,
        use_count: u32,
    },
    /// The operand names a use record outside the run its owning use declared,
    /// so the two directions of the §10.4 redundant pair disagree.
    UseIndexNotOwningUse {
        found: u32,
        owning_use: u32,
    },
    /// `payload_offset` is not [`HELIOS_HOB1_OPERAND_ALIGN`]-aligned.
    OffsetMisaligned {
        payload_offset: u32,
        alignment: u32,
    },
    /// `payload_offset + encoded_width` is not wholly inside the payload region.
    OutsidePayloadRegion {
        payload_offset: u32,
        width: u16,
    },
    /// The operand's payload bytes are not the required zero placeholder.
    /// §10.4: the operand "identifies only a generated resource operand **whose
    /// payload bytes are zero**; arbitrary patches and raw renderer IDs are
    /// rejected". A nonzero placeholder is how a raw host resource/renderer ID
    /// would reach the host at a position the operand table blesses.
    PayloadPlaceholderNonZero {
        payload_offset: u32,
        width: u16,
    },
    ReservedNonZero {
        found: u32,
    },
}

impl HeliosOuterBatchOperandV1 {
    /// Validate one typed operand against its owning, already-validated header.
    ///
    /// The payload region is `[header.payload_offset, +payload_bytes)` measured
    /// from the HOB1 start, so an operand can never name a byte of the header
    /// or of either table.
    pub fn validate(
        &self,
        header: &HeliosOuterBatchV1,
    ) -> Result<(), HeliosOperandRecordRejection> {
        use HeliosOperandRecordRejection as R;

        if self.reserved != 0 {
            return Err(R::ReservedNonZero {
                found: self.reserved,
            });
        }
        if self.operand_kind == HELIOS_HOB1_OPERAND_KIND_INVALID
            || self.operand_kind > HELIOS_HOB1_OPERAND_KIND_MAX
        {
            return Err(R::UnknownOperandKind {
                found: self.operand_kind,
            });
        }
        if self.encoded_width != HELIOS_HOB1_OPERAND_WIDTH_4
            && self.encoded_width != HELIOS_HOB1_OPERAND_WIDTH_8
        {
            return Err(R::UnsupportedEncodedWidth {
                found: self.encoded_width,
            });
        }
        if self.use_index >= header.use_count {
            return Err(R::UseIndexOutsideTable {
                found: self.use_index,
                use_count: header.use_count,
            });
        }
        // `%` rather than `is_multiple_of`: this crate is also compiled by the
        // Windows kernel toolchain, whose pinned nightly predates that method's
        // stabilisation.
        if self.payload_offset % HELIOS_HOB1_OPERAND_ALIGN != 0 {
            return Err(R::OffsetMisaligned {
                payload_offset: self.payload_offset,
                alignment: HELIOS_HOB1_OPERAND_ALIGN,
            });
        }
        // All four terms are u32-derived; the u64 arithmetic cannot overflow.
        let start = self.payload_offset as u64;
        let end = start + self.encoded_width as u64;
        let payload_start = header.payload_offset as u64;
        let payload_end = payload_start + header.payload_bytes as u64;
        if start < payload_start || end > payload_end {
            return Err(R::OutsidePayloadRegion {
                payload_offset: self.payload_offset,
                width: self.encoded_width,
            });
        }
        Ok(())
    }
}

/// What the consumer already knows about the context a HOB1 claims to belong
/// to, taken from the live KMD context object created at HQA1 attach time.
///
/// ⛔ This is the anti-stale cross-check side of §10.4. Nothing here is a
/// lookup key: the caller must already hold the exact context object, and these
/// numbers only decide whether the record it was handed is the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosOuterBatchExpectation {
    /// Exact atomic package generation.
    pub package_generation: u64,
    /// The live HTS1 session generation the context holds a strong reference to.
    pub session_generation: u64,
    /// The context generation stored at `DxgkDdiCreateContext`.
    pub context_generation: u64,
    /// The endpoint the context bound at attach time.
    pub endpoint_id: u32,
    /// The context's HQA1 arm: exactly one of
    /// [`HELIOS_HOB1_FLAG_D3D11_PHYSICAL`] / [`HELIOS_HOB1_FLAG_D3D12_VIRTUAL`].
    pub flags: u32,
    /// The current runtime-approved command-buffer capacity. `total_bytes` may
    /// not exceed it (§10.4, offset 48).
    pub max_command_bytes: u64,
    /// The highest batch ID already accepted on this context, or zero if none.
    /// `batch_id` must be strictly greater.
    pub last_batch_id: u64,
    /// Exact `D3DDDI_ALLOCATIONLIST` length dxgkrnl supplied for **this**
    /// Render, on the D3D11 physical arm.
    ///
    /// ⛔ This is the bound for every `identityKind=1` use record: the KMD
    /// indexes a kernel array with a user-mode-supplied number, so a use table
    /// validated without it is not validated. At most
    /// [`HELIOS_HOB1_MAX_USE_RECORDS`] entries; the D3D12 virtual arm has no
    /// allocation list at all and must pass zero.
    pub allocation_list_count: u32,
}

/// Why a HOB1 record was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosOuterBatchRejection {
    /// The supplied byte range is shorter than the 112-byte header.
    RecordShorterThanHeader {
        found: usize,
    },
    /// The record buffer is not 8-byte aligned, so the header cannot be read as
    /// a `HeliosOuterBatchV1` without an unaligned access.
    RecordMisaligned,
    /// `record.len()` disagrees with `total_bytes`.
    RecordLengthMismatch {
        found: usize,
        total_bytes: u64,
    },
    Magic {
        found: u32,
    },
    AbiVersion {
        found: u16,
    },
    HeaderSize {
        found: u16,
    },
    PackageGeneration {
        found: u64,
        expected: u64,
    },
    SessionGeneration {
        found: u64,
        expected: u64,
    },
    ContextGeneration {
        found: u64,
        expected: u64,
    },
    EndpointId {
        found: u32,
        expected: u32,
    },
    /// Not exactly one of the two arms, or not the arm this context attached
    /// with.
    Flags {
        found: u32,
        expected: u32,
    },
    BatchIdZero,
    /// The batch ID did not strictly increase on this context.
    BatchIdNotIncreasing {
        found: u64,
        last: u64,
    },
    TotalBytesZero,
    TotalBytesAboveLimit {
        found: u64,
        limit: u64,
    },
    TotalBytesAboveCommandBuffer {
        found: u64,
        capacity: u64,
    },
    ReservedNonZero,
    UseCountAboveLimit {
        found: u32,
        limit: u32,
    },
    OperandCountAboveLimit {
        found: u32,
        limit: u32,
    },
    PayloadBytesZero,
    /// A table offset is nonzero while its count is zero, or zero while its
    /// count is not.
    TableOffsetCountMismatch {
        offset: u32,
        count: u32,
    },
    UseTableMisaligned {
        offset: u32,
    },
    OperandTableMisaligned {
        offset: u32,
    },
    PayloadMisaligned {
        offset: u32,
    },
    UseTableBeforeHeaderEnd {
        offset: u32,
    },
    OperandTableBeforeHeaderEnd {
        offset: u32,
    },
    PayloadBeforeHeaderEnd {
        offset: u32,
    },
    UseTableOutsideRecord {
        end: u64,
        total_bytes: u64,
    },
    OperandTableOutsideRecord {
        end: u64,
        total_bytes: u64,
    },
    PayloadOutsideRecord {
        end: u64,
        total_bytes: u64,
    },
    /// The payload does not start at or after the end of both tables.
    PayloadNotAfterTables {
        payload_offset: u32,
    },
    /// The payload does not end exactly at `total_bytes`. §10.4 defines
    /// `total_bytes` as "header through payload", and the payload is required to
    /// be last, so any trailing byte is a region no table describes — sealed
    /// under the CRC and copied into the KMD's command slot or a C65 extent.
    RecordHasTrailingBytes {
        payload_end: u64,
        total_bytes: u64,
    },
    UseAndOperandTablesOverlap,
    UseTableAndPayloadOverlap,
    OperandTableAndPayloadOverlap,
    /// The caller's `allocation_list_count` exceeds
    /// [`HELIOS_HOB1_MAX_USE_RECORDS`], so the uniqueness bitmap in
    /// [`validate_batch_record`] cannot describe it.
    AllocationListTooLarge {
        found: u32,
        limit: u32,
    },
    /// The caller supplied a nonzero allocation-list length for a D3D12 virtual
    /// context, which has no allocation list (§10.4). Refused rather than
    /// ignored: the two arms must never share a bound.
    AllocationListOnVirtualArm {
        found: u32,
    },
    /// Two use records name the same allocation. §10.4: "the use table is the
    /// complete **unique**-allocation closure of every operation in that
    /// physical batch" — a duplicate would let one allocation carry two
    /// contradictory access claims and make the applied one order-dependent.
    AllocationUsedTwice {
        /// The allocation-list entry named twice.
        allocation_index: u32,
        /// The second use record naming it. The first is not reported: keeping
        /// it would cost a 16-KiB index array on a kernel stack that is 12 KiB.
        use_index: u32,
    },
    /// A use record's operand run does not begin where the previous one ended,
    /// so the runs do not tile the operand table.
    OperandRunNotContiguous {
        use_index: u32,
        expected_first_operand: u32,
        found_first_operand: u32,
    },
    /// Operand records at the end of the table belong to no use record.
    OperandRunLeavesGap {
        covered: u32,
        operand_count: u32,
    },
    /// The stored CRC64 does not match the record with that field zeroed.
    Crc64Mismatch {
        found: u64,
        computed: u64,
    },
    /// One use record was refused; the index localises it.
    UseRecord {
        index: u32,
        reason: HeliosUseRecordRejection,
    },
    /// One operand record was refused; the index localises it.
    OperandRecord {
        index: u32,
        reason: HeliosOperandRecordRejection,
    },
}

/// Half-open byte region `[start, end)` inside a HOB1 record, measured from the
/// record start. An empty region has `start == end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosHob1Region {
    pub start: u64,
    pub end: u64,
}

impl HeliosHob1Region {
    #[inline]
    const fn new(offset: u32, count: u32, stride: u32) -> Self {
        // offset, count and stride are all u32-derived, and the strides are
        // pinned <= 64 by the const block above, so neither the product nor the
        // sum can overflow u64.
        let start = offset as u64;
        Self {
            start,
            end: start + (count as u64) * (stride as u64),
        }
    }

    #[inline]
    const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    #[inline]
    const fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

impl HeliosOuterBatchV1 {
    /// Byte region of the use table inside the record.
    #[inline]
    pub const fn use_table_region(&self) -> HeliosHob1Region {
        HeliosHob1Region::new(
            self.use_offset,
            self.use_count,
            HELIOS_HOB1_USE_RECORD_BYTES,
        )
    }

    /// Byte region of the typed-operand table inside the record.
    #[inline]
    pub const fn operand_table_region(&self) -> HeliosHob1Region {
        HeliosHob1Region::new(
            self.operand_offset,
            self.operand_count,
            HELIOS_HOB1_OPERAND_RECORD_BYTES,
        )
    }

    /// Byte region of the Venus payload inside the record.
    #[inline]
    pub const fn payload_region(&self) -> HeliosHob1Region {
        HeliosHob1Region::new(self.payload_offset, self.payload_bytes, 1)
    }

    /// Validate the header alone: identity, generations, bounds, alignment, and
    /// the non-overlapping layout of the three interior regions (§10.4).
    ///
    /// This does **not** check the CRC or the table contents — those need the
    /// whole record; see [`validate_batch_record`].
    pub fn validate(
        &self,
        exp: &HeliosOuterBatchExpectation,
    ) -> Result<(), HeliosOuterBatchRejection> {
        use HeliosOuterBatchRejection as R;

        if self.magic != HELIOS_HOB1_MAGIC {
            return Err(R::Magic { found: self.magic });
        }
        if self.abi_version != HELIOS_HOB1_ABI_VERSION {
            return Err(R::AbiVersion {
                found: self.abi_version,
            });
        }
        if self.header_size != HELIOS_HOB1_HEADER_BYTES {
            return Err(R::HeaderSize {
                found: self.header_size,
            });
        }
        let mut i = 0;
        while i < self.reserved.len() {
            if self.reserved[i] != 0 {
                return Err(R::ReservedNonZero);
            }
            i += 1;
        }

        // ── anti-stale cross-check against the live context object ──────────
        // Zero is never a live package generation on *either* side; see
        // `crate::HELIOS_PACKAGE_GENERATION`.
        if self.package_generation != exp.package_generation || self.package_generation == 0 {
            return Err(R::PackageGeneration {
                found: self.package_generation,
                expected: exp.package_generation,
            });
        }
        if self.session_generation != exp.session_generation || self.session_generation == 0 {
            return Err(R::SessionGeneration {
                found: self.session_generation,
                expected: exp.session_generation,
            });
        }
        if self.context_generation != exp.context_generation || self.context_generation == 0 {
            return Err(R::ContextGeneration {
                found: self.context_generation,
                expected: exp.context_generation,
            });
        }
        if self.endpoint_id != exp.endpoint_id || self.endpoint_id == 0 {
            return Err(R::EndpointId {
                found: self.endpoint_id,
                expected: exp.endpoint_id,
            });
        }
        // Exactly one arm, and it must be the one this context attached with.
        if (self.flags != HELIOS_HOB1_FLAG_D3D11_PHYSICAL
            && self.flags != HELIOS_HOB1_FLAG_D3D12_VIRTUAL)
            || self.flags != exp.flags
        {
            return Err(R::Flags {
                found: self.flags,
                expected: exp.flags,
            });
        }
        if self.batch_id == 0 {
            return Err(R::BatchIdZero);
        }
        if self.batch_id <= exp.last_batch_id {
            return Err(R::BatchIdNotIncreasing {
                found: self.batch_id,
                last: exp.last_batch_id,
            });
        }

        // ── the caller's own allocation-list bound ──────────────────────────
        // Checked here, before any use record is read, because a wrong bound is
        // not a wire error the record can be blamed for: it silently widens or
        // narrows the kernel array every type-1 index will address.
        if exp.allocation_list_count > HELIOS_HOB1_MAX_USE_RECORDS {
            return Err(R::AllocationListTooLarge {
                found: exp.allocation_list_count,
                limit: HELIOS_HOB1_MAX_USE_RECORDS,
            });
        }
        if self.flags == HELIOS_HOB1_FLAG_D3D12_VIRTUAL && exp.allocation_list_count != 0 {
            return Err(R::AllocationListOnVirtualArm {
                found: exp.allocation_list_count,
            });
        }

        // ── size bounds ─────────────────────────────────────────────────────
        if self.total_bytes == 0 {
            return Err(R::TotalBytesZero);
        }
        if self.total_bytes > HELIOS_HOB1_MAX_BYTES {
            return Err(R::TotalBytesAboveLimit {
                found: self.total_bytes,
                limit: HELIOS_HOB1_MAX_BYTES,
            });
        }
        if self.total_bytes > exp.max_command_bytes {
            return Err(R::TotalBytesAboveCommandBuffer {
                found: self.total_bytes,
                capacity: exp.max_command_bytes,
            });
        }
        if self.use_count > HELIOS_HOB1_MAX_USE_RECORDS {
            return Err(R::UseCountAboveLimit {
                found: self.use_count,
                limit: HELIOS_HOB1_MAX_USE_RECORDS,
            });
        }
        if self.operand_count > HELIOS_HOB1_MAX_OPERAND_RECORDS {
            return Err(R::OperandCountAboveLimit {
                found: self.operand_count,
                limit: HELIOS_HOB1_MAX_OPERAND_RECORDS,
            });
        }
        if self.payload_bytes == 0 {
            // §10.1 inv. 2: the command carrying the association must contain
            // the actual translated work. An empty payload is never a batch.
            return Err(R::PayloadBytesZero);
        }

        // ── table/payload placement ─────────────────────────────────────────
        let header_end = HELIOS_HOB1_HEADER_BYTES as u64;
        let align = HELIOS_HOB1_OFFSET_ALIGNMENT;

        if (self.use_count == 0) != (self.use_offset == 0) {
            return Err(R::TableOffsetCountMismatch {
                offset: self.use_offset,
                count: self.use_count,
            });
        }
        if (self.operand_count == 0) != (self.operand_offset == 0) {
            return Err(R::TableOffsetCountMismatch {
                offset: self.operand_offset,
                count: self.operand_count,
            });
        }

        let uses = self.use_table_region();
        if !uses.is_empty() {
            if uses.start % align != 0 {
                return Err(R::UseTableMisaligned {
                    offset: self.use_offset,
                });
            }
            if uses.start < header_end {
                return Err(R::UseTableBeforeHeaderEnd {
                    offset: self.use_offset,
                });
            }
            if uses.end > self.total_bytes {
                return Err(R::UseTableOutsideRecord {
                    end: uses.end,
                    total_bytes: self.total_bytes,
                });
            }
        }

        let operands = self.operand_table_region();
        if !operands.is_empty() {
            if operands.start % align != 0 {
                return Err(R::OperandTableMisaligned {
                    offset: self.operand_offset,
                });
            }
            if operands.start < header_end {
                return Err(R::OperandTableBeforeHeaderEnd {
                    offset: self.operand_offset,
                });
            }
            if operands.end > self.total_bytes {
                return Err(R::OperandTableOutsideRecord {
                    end: operands.end,
                    total_bytes: self.total_bytes,
                });
            }
        }

        let payload = self.payload_region();
        if payload.start % align != 0 {
            return Err(R::PayloadMisaligned {
                offset: self.payload_offset,
            });
        }
        if payload.start < header_end {
            return Err(R::PayloadBeforeHeaderEnd {
                offset: self.payload_offset,
            });
        }
        if payload.end > self.total_bytes {
            return Err(R::PayloadOutsideRecord {
                end: payload.end,
                total_bytes: self.total_bytes,
            });
        }
        // "payload offset | aligned and after both tables".
        if payload.start < uses.end || payload.start < operands.end {
            return Err(R::PayloadNotAfterTables {
                payload_offset: self.payload_offset,
            });
        }
        // "total bytes | header through payload" — exactly, not at least. The
        // payload is last (checked immediately above), so anything after it is a
        // region no table describes.
        if payload.end != self.total_bytes {
            return Err(R::RecordHasTrailingBytes {
                payload_end: payload.end,
                total_bytes: self.total_bytes,
            });
        }

        if uses.overlaps(&operands) {
            return Err(R::UseAndOperandTablesOverlap);
        }
        if uses.overlaps(&payload) {
            return Err(R::UseTableAndPayloadOverlap);
        }
        if operands.overlaps(&payload) {
            return Err(R::OperandTableAndPayloadOverlap);
        }

        Ok(())
    }
}

/// CRC-64/ECMA-182 of a complete HOB1 record with the checksum field at offset
/// 80 folded in as zero (§10.4, offset 80).
///
/// `const fn`, panic-free, and does not copy the record: the eight checksum
/// bytes are substituted during the fold rather than zeroed in place, so a
/// caller may checksum a read-only mapping.
///
/// ⚠ **PASSIVE_LEVEL only.** This is a byte-at-a-time fold over the whole
/// record — up to [`HELIOS_HOB1_MAX_BYTES`] = 15,728,640 iterations, which is
/// the one unbounded-looking cost in this module. The D3D11 Render path that
/// calls it runs at `PASSIVE_LEVEL`; paging `DxgkDdiSubmitCommand` runs at
/// `DISPATCH_LEVEL` (§10.7) and must never reach it. CLAUDE.md: never spin in a
/// DPC/ISR path.
pub const fn hob1_record_crc64(record: &[u8]) -> Result<u64, HeliosOuterBatchRejection> {
    if record.len() < HELIOS_HOB1_HEADER_BYTES as usize {
        return Err(HeliosOuterBatchRejection::RecordShorterThanHeader {
            found: record.len(),
        });
    }
    let mut crc = HELIOS_CRC64_ECMA182_INIT;
    let mut i = 0usize;
    while i < record.len() {
        let byte = if i >= HELIOS_HOB1_CRC_FIELD_OFFSET && i < HELIOS_HOB1_CRC_FIELD_OFFSET + 8 {
            0u8
        } else {
            record[i]
        };
        let idx = (((crc >> 56) as u8) ^ byte) as usize;
        crc = CRC64_ECMA182_TABLE[idx] ^ (crc << 8);
        i += 1;
    }
    Ok(crc ^ HELIOS_CRC64_ECMA182_XOROUT)
}

/// Borrow the 112-byte header out of a complete HOB1 record.
///
/// Fails closed on a short or misaligned buffer rather than performing an
/// unaligned read; the KMD's copied command slot and the D3D12 pool extent are
/// both 8-aligned by construction.
///
/// ⛔ The returned reference aliases `record` for its whole lifetime. See
/// [`validate_batch_record`] for the caller contract: the bytes must not be
/// concurrently writable by the party that produced them, or validation and
/// execution can see two different records.
pub fn hob1_header(record: &[u8]) -> Result<&HeliosOuterBatchV1, HeliosOuterBatchRejection> {
    let head = match record.get(..HELIOS_HOB1_HEADER_BYTES as usize) {
        Some(head) => head,
        None => {
            return Err(HeliosOuterBatchRejection::RecordShorterThanHeader {
                found: record.len(),
            })
        }
    };
    bytemuck::try_from_bytes::<HeliosOuterBatchV1>(head)
        .map_err(|_| HeliosOuterBatchRejection::RecordMisaligned)
}

/// Borrow the use table out of a validated HOB1 record.
pub fn hob1_use_records<'a>(
    record: &'a [u8],
    header: &HeliosOuterBatchV1,
) -> Result<&'a [HeliosOuterBatchUseV1], HeliosOuterBatchRejection> {
    let region = header.use_table_region();
    if region.is_empty() {
        return Ok(&[]);
    }
    let bytes = match record.get(region.start as usize..region.end as usize) {
        Some(bytes) => bytes,
        None => {
            return Err(HeliosOuterBatchRejection::UseTableOutsideRecord {
                end: region.end,
                total_bytes: record.len() as u64,
            })
        }
    };
    bytemuck::try_cast_slice(bytes).map_err(|_| HeliosOuterBatchRejection::UseTableMisaligned {
        offset: header.use_offset,
    })
}

/// Borrow the typed-operand table out of a validated HOB1 record.
pub fn hob1_operand_records<'a>(
    record: &'a [u8],
    header: &HeliosOuterBatchV1,
) -> Result<&'a [HeliosOuterBatchOperandV1], HeliosOuterBatchRejection> {
    let region = header.operand_table_region();
    if region.is_empty() {
        return Ok(&[]);
    }
    let bytes = match record.get(region.start as usize..region.end as usize) {
        Some(bytes) => bytes,
        None => {
            return Err(HeliosOuterBatchRejection::OperandTableOutsideRecord {
                end: region.end,
                total_bytes: record.len() as u64,
            })
        }
    };
    bytemuck::try_cast_slice(bytes).map_err(|_| HeliosOuterBatchRejection::OperandTableMisaligned {
        offset: header.operand_offset,
    })
}

/// Borrow the sealed Venus payload out of a validated HOB1 record.
pub fn hob1_payload<'a>(
    record: &'a [u8],
    header: &HeliosOuterBatchV1,
) -> Result<&'a [u8], HeliosOuterBatchRejection> {
    let region = header.payload_region();
    match record.get(region.start as usize..region.end as usize) {
        Some(bytes) => Ok(bytes),
        None => Err(HeliosOuterBatchRejection::PayloadOutsideRecord {
            end: region.end,
            total_bytes: record.len() as u64,
        }),
    }
}

/// The complete HOB1 validator: header, exact record length, CRC64, then every
/// use record and every operand record, plus the three whole-table properties
/// no single record can carry (§10.4).
///
/// Enforced here, in one pass and with no allocation:
///   * every §10.4 field rule on every use and operand record;
///   * **unique-allocation closure** on the D3D11 physical arm — no two use
///     records may name the same allocation-list entry (a 4096-bit stack
///     bitmap, sized by the `const` assertion below); and
///   * the redundant use↔operand pair agreeing in **both** directions: the
///     nonempty `{first_operand, operand_count}` runs tile `[0, operand_count)`
///     in order, and each operand's `use_index` names the use whose run contains
///     it. A use that owns no typed operand carries `first_operand == 0` and is
///     skipped by the tiling walk; and
///   * each operand's payload bytes being the required **zero placeholder** —
///     "arbitrary patches and raw renderer IDs are rejected", which is the rule
///     that stops a raw host resource ID travelling to the host at a position
///     the operand table blesses.
///
/// ⛔ **Caller contract — the record must not be concurrently writable.** This
/// function reads `record` many times (header fields, then the tables, then the
/// CRC fold), so a caller that validates bytes another party can still store to
/// validates a record that no longer exists. §10.6 makes that immutability a
/// *guest-side* promise ("once submitted, the extent is immutable"), which is
/// not a property the host can rely on: QEMU must snapshot the HOB1 out of the
/// process page tables and validate the snapshot it will execute, and the D3D11
/// KMD must validate its own copied command slot, never the user buffer.
///
/// Total function; no allocation; every refusal is named and, for a table
/// entry, localised by index. Rejection is terminal — §10.6: a mismatched
/// length/checksum/generation record "is never truncated or reclassified".
pub fn validate_batch_record(
    record: &[u8],
    exp: &HeliosOuterBatchExpectation,
) -> Result<(), HeliosOuterBatchRejection> {
    use HeliosOuterBatchRejection as R;

    let header = hob1_header(record)?;
    header.validate(exp)?;

    if record.len() as u64 != header.total_bytes {
        return Err(R::RecordLengthMismatch {
            found: record.len(),
            total_bytes: header.total_bytes,
        });
    }

    let computed = hob1_record_crc64(record)?;
    if computed != header.crc64 {
        return Err(R::Crc64Mismatch {
            found: header.crc64,
            computed,
        });
    }

    let uses = hob1_use_records(record, header)?;
    let operands = hob1_operand_records(record, header)?;

    // One bit per legal allocation-list index: 512 bytes of PASSIVE_LEVEL stack,
    // no allocation. Only the D3D11 arm can populate it — a type-2 GPUVA is a
    // 64-bit address with no bounded index space, and §10.4 states the
    // unique-allocation closure for "that physical batch".
    let mut seen = [0u64; (HELIOS_HOB1_MAX_USE_RECORDS / 64) as usize];
    let mut next_operand: u32 = 0;

    let mut index: u32 = 0;
    while (index as usize) < uses.len() {
        let record_use = &uses[index as usize];
        let identity =
            match record_use.validate(header.flags, header.operand_count, exp.allocation_list_count)
            {
                Ok(identity) => identity,
                Err(reason) => return Err(R::UseRecord { index, reason }),
            };

        if let HeliosUseIdentity::D3D11AllocationListIndex(allocation_index) = identity {
            // `validate` already bounded the index below `allocation_list_count`,
            // which `HeliosOuterBatchV1::validate` bounded below
            // `HELIOS_HOB1_MAX_USE_RECORDS`, so both lookups are in range; the
            // `match` keeps that fact from being an unchecked index anyway.
            let word = match seen.get_mut((allocation_index / 64) as usize) {
                Some(word) => word,
                None => {
                    return Err(R::UseRecord {
                        index,
                        reason: HeliosUseRecordRejection::D3D11IndexOutsideAllocationList {
                            found: allocation_index,
                            allocation_list_count: exp.allocation_list_count,
                        },
                    })
                }
            };
            let bit = 1u64 << (allocation_index % 64);
            if *word & bit != 0 {
                return Err(R::AllocationUsedTwice {
                    allocation_index,
                    use_index: index,
                });
            }
            *word |= bit;
        }

        // The nonempty runs must tile the operand table in order. `validate`
        // already proved `first_operand + operand_count <= header.operand_count`
        // and that an *empty* run carries `first_operand == 0`, so an empty run
        // is skipped rather than advancing (or resetting) the cursor: a use that
        // names no typed operand is legal — an allocation can be used without a
        // generated resource operand naming it — and must not make the batch
        // unencodable.
        if record_use.operand_count != 0 {
            if record_use.first_operand != next_operand {
                return Err(R::OperandRunNotContiguous {
                    use_index: index,
                    expected_first_operand: next_operand,
                    found_first_operand: record_use.first_operand,
                });
            }
            // `validate` already proved this sum is at most `operand_count`
            // (≤ 8192), so the checked form can only be `None` for a record it
            // has already refused; it is spelled checked anyway because an
            // arithmetic overflow panic in a DDI is a silent graphics deadlock.
            let run_end = match record_use
                .first_operand
                .checked_add(record_use.operand_count as u32)
            {
                Some(run_end) => run_end,
                None => {
                    return Err(R::UseRecord {
                        index,
                        reason: HeliosUseRecordRejection::OperandRangeOutsideTable {
                            first_operand: record_use.first_operand,
                            operand_count: record_use.operand_count,
                            batch_operand_count: header.operand_count,
                        },
                    })
                }
            };
            let mut operand_index = record_use.first_operand;
            while operand_index < run_end {
                let operand = match operands.get(operand_index as usize) {
                    Some(operand) => operand,
                    None => {
                        return Err(R::UseRecord {
                            index,
                            reason: HeliosUseRecordRejection::OperandRangeOutsideTable {
                                first_operand: record_use.first_operand,
                                operand_count: record_use.operand_count,
                                batch_operand_count: header.operand_count,
                            },
                        })
                    }
                };
                if let Err(reason) = operand.validate(header) {
                    return Err(R::OperandRecord {
                        index: operand_index,
                        reason,
                    });
                }
                // "It identifies only a generated resource operand whose payload
                // bytes are ZERO; arbitrary patches and raw renderer IDs are
                // rejected" (§10.4). `operand.validate` proved the range lies
                // inside the payload, so the slice exists; the `None` arm cannot
                // be reached and is spelled out because a total function may not
                // index blindly.
                //
                // ⚠ This is only ever true *before* the consumer rewrites the
                // placeholder to a device-local capability ordinal — which is the
                // only time this validator can run at all, because the CRC covers
                // the payload and a rewritten record no longer matches it.
                let start = operand.payload_offset as usize;
                let end = start + operand.encoded_width as usize;
                let placeholder = match record.get(start..end) {
                    Some(placeholder) => placeholder,
                    None => {
                        return Err(R::OperandRecord {
                            index: operand_index,
                            reason: HeliosOperandRecordRejection::OutsidePayloadRegion {
                                payload_offset: operand.payload_offset,
                                width: operand.encoded_width,
                            },
                        })
                    }
                };
                let mut byte = 0usize;
                while byte < placeholder.len() {
                    if placeholder[byte] != 0 {
                        return Err(R::OperandRecord {
                            index: operand_index,
                            reason: HeliosOperandRecordRejection::PayloadPlaceholderNonZero {
                                payload_offset: operand.payload_offset,
                                width: operand.encoded_width,
                            },
                        });
                    }
                    byte += 1;
                }
                // The other direction of the redundant pair.
                if operand.use_index != index {
                    return Err(R::OperandRecord {
                        index: operand_index,
                        reason: HeliosOperandRecordRejection::UseIndexNotOwningUse {
                            found: operand.use_index,
                            owning_use: index,
                        },
                    });
                }
                operand_index += 1;
            }
            next_operand = run_end;
        }
        index += 1;
    }

    // An operand owned by no use is a typed operand nobody rewrites, which
    // §10.4 does not admit.
    if next_operand as usize != operands.len() {
        return Err(R::OperandRunLeavesGap {
            covered: next_operand,
            operand_count: header.operand_count,
        });
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.4 — HOS1: the fixed 64-byte D3D12 private-data descriptor
// ─────────────────────────────────────────────────────────────────────────────

/// `HOS1` — magic of [`HeliosOuterSubmitV1`] (§10.4, offset 0).
pub const HELIOS_HOS1_MAGIC: u32 = 0x3153_4F48;
/// HOS1 ABI version (§10.4, offset 4).
pub const HELIOS_HOS1_ABI_VERSION: u16 = 1;
/// HOS1 size in bytes (§10.4, offset 6). KMD accepts exactly
/// `DmaBufferUmdPrivateDataSize == 64` for an HQA1 D3D12 context.
pub const HELIOS_HOS1_BYTES: u16 = 64;

/// The fixed 64-byte `pPrivateDriverData` a D3D12 `pfnSubmitCommandCb` carries
/// (`HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.4).
///
/// # It is metadata, not work
///
/// D3D12 leaves the HOB1 at the exact submitted command GPUVA and passes only
/// this record; dxgkrnl copies the prefix into KMD private data. "It contains
/// no command bytes, resource identity, pointer, handle, GPUVA, or host token."
/// Neither KMD nor QEMU interprets HOS1 itself as work: at virtual submit KMD
/// validates it against the already-attached context and nonblocking-enqueues
/// the `{process/page-table generation, DmaBufferVirtualAddress,
/// DmaBufferSize}` hardware work **without dereferencing the GPUVA**.
///
/// # It never carries `WrittenPrimaries`
///
/// The UMD/runtime primary association stays in `D3DDDICB_SUBMITCOMMAND` where
/// C1 defines it. `DXGKARG_SUBMITCOMMANDVIRTUAL` does not carry the field to
/// KMD at all, so nothing here may stand in for it.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosOuterSubmitV1 {
    /// `== HELIOS_HOS1_MAGIC` (offset 0).
    pub magic: u32,
    /// `== HELIOS_HOS1_ABI_VERSION` (offset 4).
    pub abi_version: u16,
    /// `== HELIOS_HOS1_BYTES` (offset 6).
    pub struct_size: u16,
    /// Exact package generation (offset 8).
    pub package_generation: u64,
    /// Exact HTS1 session generation (offset 16).
    pub session_generation: u64,
    /// Exact attached virtual context generation (offset 24).
    pub context_generation: u64,
    /// Exact direct endpoint (offset 32).
    pub endpoint_id: u32,
    /// HOB1 byte count; equals `CommandLength`/`DmaBufferSize` (offset 36).
    pub hob1_bytes: u32,
    /// Context-local batch ID; equals the HOB1's (offset 40).
    pub batch_id: u64,
    /// HOB1 CRC64-ECMA; equals the HOB1's (offset 48).
    pub hob1_crc64: u64,
    /// Reserved; zero (offset 56).
    pub reserved: u64,
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md §10.4 — the 64-byte HOS1 table.
const _: () = {
    assert!(core::mem::size_of::<HeliosOuterSubmitV1>() == 64);
    assert!(core::mem::size_of::<HeliosOuterSubmitV1>() == HELIOS_HOS1_BYTES as usize);
    assert!(core::mem::align_of::<HeliosOuterSubmitV1>() == 8);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, session_generation) == 16);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, context_generation) == 24);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, endpoint_id) == 32);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, hob1_bytes) == 36);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, batch_id) == 40);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, hob1_crc64) == 48);
    assert!(core::mem::offset_of!(HeliosOuterSubmitV1, reserved) == 56);

    // ⭐ "HOS1 carries no command/resource bytes" (§17.1), asserted rather than
    // asserted-in-prose: the eleven declared scalar fields account for all 64
    // bytes, so there is no byte array, no trailing capacity, and no padding a
    // producer could smuggle a command, GPUVA, or handle through. Adding such a
    // field would break this sum, and shrinking a field to make room would
    // break the offset assertions above.
    let scalar_bytes = core::mem::size_of::<u32>()      // magic
        + core::mem::size_of::<u16>()                   // abi_version
        + core::mem::size_of::<u16>()                   // struct_size
        + core::mem::size_of::<u64>()                   // package_generation
        + core::mem::size_of::<u64>()                   // session_generation
        + core::mem::size_of::<u64>()                   // context_generation
        + core::mem::size_of::<u32>()                   // endpoint_id
        + core::mem::size_of::<u32>()                   // hob1_bytes
        + core::mem::size_of::<u64>()                   // batch_id
        + core::mem::size_of::<u64>()                   // hob1_crc64
        + core::mem::size_of::<u64>(); // reserved
    assert!(scalar_bytes == core::mem::size_of::<HeliosOuterSubmitV1>());

    // Four ASCII bytes read little-endian; see the same block under HOB1.
    assert!(HELIOS_HOS1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HOS1_MAGIC.to_le_bytes()[1] == b'O');
    assert!(HELIOS_HOS1_MAGIC.to_le_bytes()[2] == b'S');
    assert!(HELIOS_HOS1_MAGIC.to_le_bytes()[3] == b'1');
};

/// Why a HOS1 record was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosOuterSubmitRejection {
    Magic {
        found: u32,
    },
    AbiVersion {
        found: u16,
    },
    StructSize {
        found: u16,
    },
    /// The runtime supplied a private-data buffer that is not exactly 64 bytes.
    PrivateDataSize {
        found: usize,
    },
    PackageGeneration {
        found: u64,
        expected: u64,
    },
    SessionGeneration {
        found: u64,
        expected: u64,
    },
    ContextGeneration {
        found: u64,
        expected: u64,
    },
    EndpointId {
        found: u32,
        expected: u32,
    },
    /// This context did not attach with the D3D12 virtual arm.
    NotD3D12VirtualContext {
        context_flags: u32,
    },
    BatchIdZero,
    BatchIdNotIncreasing {
        found: u64,
        last: u64,
    },
    Hob1BytesZero,
    /// `hob1_bytes` is below the 112-byte HOB1 header, so the GPUVA it describes
    /// cannot contain a record at all. KMD never dereferences that GPUVA
    /// (§10.4), so this is the only place the impossibility is visible before
    /// QEMU — one trust boundary later.
    Hob1BytesBelowHeader {
        found: u32,
        header_bytes: u16,
    },
    Hob1BytesAboveLimit {
        found: u32,
        limit: u64,
    },
    /// `hob1_bytes` disagrees with the runtime's `CommandLength`/
    /// `DmaBufferSize`.
    Hob1BytesMismatch {
        found: u32,
        command_length: u64,
    },
    ReservedNonZero {
        found: u64,
    },
    /// A cross-check against the HOB1 itself disagreed.
    Hob1BatchIdMismatch {
        hos1: u64,
        hob1: u64,
    },
    Hob1CrcMismatch {
        hos1: u64,
        hob1: u64,
    },
    Hob1TotalBytesMismatch {
        hos1: u32,
        hob1: u64,
    },
}

impl HeliosOuterSubmitV1 {
    /// Validate the private-data descriptor against the already-attached
    /// context and the runtime's command length.
    ///
    /// `command_length` is the runtime's `CommandLength`/`DmaBufferSize` for
    /// this submit. `exp.flags` must be [`HELIOS_HOB1_FLAG_D3D12_VIRTUAL`]:
    /// HOS1 exists only on the D3D12 virtual arm.
    pub fn validate(
        &self,
        exp: &HeliosOuterBatchExpectation,
        command_length: u64,
    ) -> Result<(), HeliosOuterSubmitRejection> {
        use HeliosOuterSubmitRejection as R;

        if self.magic != HELIOS_HOS1_MAGIC {
            return Err(R::Magic { found: self.magic });
        }
        if self.abi_version != HELIOS_HOS1_ABI_VERSION {
            return Err(R::AbiVersion {
                found: self.abi_version,
            });
        }
        if self.struct_size != HELIOS_HOS1_BYTES {
            return Err(R::StructSize {
                found: self.struct_size,
            });
        }
        if self.reserved != 0 {
            return Err(R::ReservedNonZero {
                found: self.reserved,
            });
        }
        if exp.flags != HELIOS_HOB1_FLAG_D3D12_VIRTUAL {
            return Err(R::NotD3D12VirtualContext {
                context_flags: exp.flags,
            });
        }
        // Zero is never a live package generation on *either* side; see
        // `crate::HELIOS_PACKAGE_GENERATION`.
        if self.package_generation != exp.package_generation || self.package_generation == 0 {
            return Err(R::PackageGeneration {
                found: self.package_generation,
                expected: exp.package_generation,
            });
        }
        if self.session_generation != exp.session_generation || self.session_generation == 0 {
            return Err(R::SessionGeneration {
                found: self.session_generation,
                expected: exp.session_generation,
            });
        }
        if self.context_generation != exp.context_generation || self.context_generation == 0 {
            return Err(R::ContextGeneration {
                found: self.context_generation,
                expected: exp.context_generation,
            });
        }
        if self.endpoint_id != exp.endpoint_id || self.endpoint_id == 0 {
            return Err(R::EndpointId {
                found: self.endpoint_id,
                expected: exp.endpoint_id,
            });
        }
        if self.batch_id == 0 {
            return Err(R::BatchIdZero);
        }
        if self.batch_id <= exp.last_batch_id {
            return Err(R::BatchIdNotIncreasing {
                found: self.batch_id,
                last: exp.last_batch_id,
            });
        }
        if self.hob1_bytes == 0 {
            return Err(R::Hob1BytesZero);
        }
        if self.hob1_bytes < HELIOS_HOB1_HEADER_BYTES as u32 {
            return Err(R::Hob1BytesBelowHeader {
                found: self.hob1_bytes,
                header_bytes: HELIOS_HOB1_HEADER_BYTES,
            });
        }
        if self.hob1_bytes as u64 > HELIOS_HOB1_MAX_BYTES {
            return Err(R::Hob1BytesAboveLimit {
                found: self.hob1_bytes,
                limit: HELIOS_HOB1_MAX_BYTES,
            });
        }
        if self.hob1_bytes as u64 != command_length {
            return Err(R::Hob1BytesMismatch {
                found: self.hob1_bytes,
                command_length,
            });
        }
        Ok(())
    }

    /// Cross-check a HOS1 against the HOB1 it describes.
    ///
    /// ⚠ Deliberately separate from [`Self::validate`]: at virtual submit **KMD
    /// never dereferences the command GPUVA** (§10.4), so it can only run
    /// [`Self::validate`]. This routine is for the sealing UMD's own self-check
    /// and for KMD after it snapshots HOB1 from the exact HOC1 allocation.
    pub fn cross_check(&self, hob1: &HeliosOuterBatchV1) -> Result<(), HeliosOuterSubmitRejection> {
        use HeliosOuterSubmitRejection as R;

        if self.batch_id != hob1.batch_id {
            return Err(R::Hob1BatchIdMismatch {
                hos1: self.batch_id,
                hob1: hob1.batch_id,
            });
        }
        if self.hob1_crc64 != hob1.crc64 {
            return Err(R::Hob1CrcMismatch {
                hos1: self.hob1_crc64,
                hob1: hob1.crc64,
            });
        }
        if self.hob1_bytes as u64 != hob1.total_bytes {
            return Err(R::Hob1TotalBytesMismatch {
                hos1: self.hob1_bytes,
                hob1: hob1.total_bytes,
            });
        }
        Ok(())
    }

    /// Parse a runtime private-data buffer that must be exactly 64 bytes.
    ///
    /// Returns an **owned** record, read unaligned. Dxgkrnl copies the UMD
    /// prefix into KMD private data (§10.4) and promises nothing about that
    /// buffer's alignment, so a borrow would either fault or turn a
    /// correct-length buffer into a length refusal that names the wrong cause.
    /// This is the same shape as
    /// [`crate::translation_session::parse_create_context_private_data`].
    pub fn from_private_data(bytes: &[u8]) -> Result<Self, HeliosOuterSubmitRejection> {
        if bytes.len() != HELIOS_HOS1_BYTES as usize {
            return Err(HeliosOuterSubmitRejection::PrivateDataSize { found: bytes.len() });
        }
        // With the length already exact, `try_pod_read_unaligned` cannot fail;
        // the arm is kept because a total function may not `unwrap`.
        bytemuck::try_pod_read_unaligned::<Self>(bytes)
            .map_err(|_| HeliosOuterSubmitRejection::PrivateDataSize { found: bytes.len() })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// §10.6 — C65: the D3D12 HOB1 command-buffer pool
// ─────────────────────────────────────────────────────────────────────────────

/// `HOC1` — magic of [`HeliosOuterCommandAllocationV1`] (§10.6, offset 0).
pub const HELIOS_HOC1_MAGIC: u32 = 0x3143_4F48;
/// HOC1 ABI version (§10.6, offset 4).
pub const HELIOS_HOC1_ABI_VERSION: u16 = 1;
/// HOC1 structure size in bytes (§10.6, offset 6).
pub const HELIOS_HOC1_BYTES: u16 = 64;

/// The C65 pool is exactly 64 MiB (§10.6, HOC1 offset 24).
pub const HELIOS_HOC1_POOL_BYTES: u64 = 64 * 1024 * 1024;
/// Every extent starts on a 64-KiB boundary (§10.6, HOC1 offset 32).
pub const HELIOS_HOC1_EXTENT_ALIGNMENT: u32 = 64 * 1024;
/// At most 256 extents may be live (§10.6).
pub const HELIOS_HOC1_MAX_LIVE_EXTENTS: u32 = 256;

/// [`HeliosOuterCommandAllocationV1::access`] — CPU write.
pub const HELIOS_HOC1_ACCESS_CPU_WRITE: u32 = 1;
/// [`HeliosOuterCommandAllocationV1::access`] — device read.
pub const HELIOS_HOC1_ACCESS_DEVICE_READ: u32 = 2;
/// The exact access word §10.6 requires: `CPU_WRITE|DEVICE_READ`, nothing else.
/// ⛔ The pool is write-once from the CPU and read-only to the device; there is
/// no device-write or CPU-read arm to select.
pub const HELIOS_HOC1_ACCESS_REQUIRED: u32 =
    HELIOS_HOC1_ACCESS_CPU_WRITE | HELIOS_HOC1_ACCESS_DEVICE_READ;

/// [`HeliosOuterCommandAllocationV1::cache_policy`] — the exact and only
/// policy: write-combined (§10.6, offset 40).
pub const HELIOS_HOC1_CACHE_WRITE_COMBINED: u32 = 1;
/// [`HeliosOuterCommandAllocationV1::physical_adapter_mask`] — exactly node bit
/// 0 (§10.6, offset 44; §10.2 exposes one physical node).
pub const HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0: u32 = 1;

/// The 64-byte per-allocation record for the C65 D3D12 command-buffer pool
/// (`HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.6).
///
/// The D3D12 UMD calls `pfnAllocateCb` with `hResource=NULL`, one zeroed legacy
/// `D3DDDI_ALLOCATIONINFO`, `pSystemMem=NULL`, and this record. There is **no
/// resource-level private data and no runtime resource handle**.
///
/// # HOC1 is neither a renderer/resource identity nor a shareable object
///
/// KMD admits it only as a **nonprimary, nonshared**, CPU-visible/WC allocation
/// in the ordinary aperture, with its exact CPU view supplied by
/// ShareBackingStoreWithKmd. It is *not* an HVM1 renderer resource: nothing in
/// it names a host backing, a `resid`, a Venus object, or anything another
/// process could open. Its only mutable field is
/// [`Self::allocation_generation`], which is zero on input and which KMD writes
/// back nonzero at create — the one create-time write-back in this ABI.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pod, Zeroable)]
pub struct HeliosOuterCommandAllocationV1 {
    /// `== HELIOS_HOC1_MAGIC` (offset 0).
    pub magic: u32,
    /// `== HELIOS_HOC1_ABI_VERSION` (offset 4).
    pub abi_version: u16,
    /// `== HELIOS_HOC1_BYTES` (offset 6).
    pub struct_size: u16,
    /// Exact atomic package generation (offset 8).
    pub package_generation: u64,
    /// Zero on input; KMD returns nonzero (offset 16).
    pub allocation_generation: u64,
    /// Exactly [`HELIOS_HOC1_POOL_BYTES`] (offset 24).
    pub byte_size: u64,
    /// Exactly [`HELIOS_HOC1_EXTENT_ALIGNMENT`] (offset 32).
    pub extent_alignment: u32,
    /// Exactly [`HELIOS_HOC1_ACCESS_REQUIRED`] (offset 36).
    pub access: u32,
    /// Exactly [`HELIOS_HOC1_CACHE_WRITE_COMBINED`] (offset 40).
    pub cache_policy: u32,
    /// Exactly [`HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0`] (offset 44).
    pub physical_adapter_mask: u32,
    /// Reserved; zero (offset 48, 16 bytes).
    pub reserved: [u8; 16],
}

// HELIOS_PRESENT_SYNC_RETIREMENT.md §10.6 — the 64-byte HOC1 table.
const _: () = {
    assert!(core::mem::size_of::<HeliosOuterCommandAllocationV1>() == 64);
    assert!(core::mem::size_of::<HeliosOuterCommandAllocationV1>() == HELIOS_HOC1_BYTES as usize);
    assert!(core::mem::align_of::<HeliosOuterCommandAllocationV1>() == 8);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, magic) == 0);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, abi_version) == 4);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, struct_size) == 6);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, package_generation) == 8);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, allocation_generation) == 16);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, byte_size) == 24);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, extent_alignment) == 32);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, access) == 36);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, cache_policy) == 40);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, physical_adapter_mask) == 44);
    assert!(core::mem::offset_of!(HeliosOuterCommandAllocationV1, reserved) == 48);

    // C65 arithmetic that must hold for the allocator to be expressible at all.
    assert!(HELIOS_HOC1_POOL_BYTES == 67_108_864);
    assert!(HELIOS_HOC1_EXTENT_ALIGNMENT == 65_536);
    assert!(HELIOS_HOC1_POOL_BYTES % HELIOS_HOC1_EXTENT_ALIGNMENT as u64 == 0);
    assert!(HELIOS_HOC1_MAX_LIVE_EXTENTS == 256);
    // 256 minimum-size extents fit the pool with room to spare, so the live
    // limit is a bookkeeping bound and never the binding one.
    assert!(
        HELIOS_HOC1_MAX_LIVE_EXTENTS as u64 * HELIOS_HOC1_EXTENT_ALIGNMENT as u64
            <= HELIOS_HOC1_POOL_BYTES
    );

    // Four ASCII bytes read little-endian; see the same block under HOB1.
    assert!(HELIOS_HOC1_MAGIC.to_le_bytes()[0] == b'H');
    assert!(HELIOS_HOC1_MAGIC.to_le_bytes()[1] == b'O');
    assert!(HELIOS_HOC1_MAGIC.to_le_bytes()[2] == b'C');
    assert!(HELIOS_HOC1_MAGIC.to_le_bytes()[3] == b'1');
};

/// Why a HOC1 record was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosOuterCommandAllocRejection {
    Magic {
        found: u32,
    },
    AbiVersion {
        found: u16,
    },
    StructSize {
        found: u16,
    },
    PackageGeneration {
        found: u64,
        expected: u64,
    },
    /// Input must carry zero; KMD assigns the generation.
    AllocationGenerationNonZeroOnInput {
        found: u64,
    },
    /// KMD returned zero: the create-time write-back did not happen.
    AllocationGenerationZeroOnOutput,
    ByteSize {
        found: u64,
        required: u64,
    },
    ExtentAlignment {
        found: u32,
        required: u32,
    },
    Access {
        found: u32,
        required: u32,
    },
    CachePolicy {
        found: u32,
        required: u32,
    },
    PhysicalAdapterMask {
        found: u32,
        required: u32,
    },
    ReservedNonZero,
    /// The `pfnAllocateCb` private-data buffer is not exactly
    /// [`HELIOS_HOC1_BYTES`]. Same obligation, and same reason, as
    /// [`HeliosAllocDescRejection::PrivateDataSize`].
    PrivateDataSize {
        found: usize,
        expected: usize,
    },
}

impl HeliosOuterCommandAllocationV1 {
    /// The exact create-time input record: everything fixed by §10.6 with
    /// `allocation_generation == 0` awaiting the KMD write-back.
    #[inline]
    pub const fn new(package_generation: u64) -> Self {
        Self {
            magic: HELIOS_HOC1_MAGIC,
            abi_version: HELIOS_HOC1_ABI_VERSION,
            struct_size: HELIOS_HOC1_BYTES,
            package_generation,
            allocation_generation: 0,
            byte_size: HELIOS_HOC1_POOL_BYTES,
            extent_alignment: HELIOS_HOC1_EXTENT_ALIGNMENT,
            access: HELIOS_HOC1_ACCESS_REQUIRED,
            cache_policy: HELIOS_HOC1_CACHE_WRITE_COMBINED,
            physical_adapter_mask: HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0,
            reserved: [0; 16],
        }
    }

    /// Read one HOC1 out of an allocation private-data buffer that must be
    /// exactly [`HELIOS_HOC1_BYTES`] long.
    ///
    /// Owned and unaligned for the same reason as
    /// [`HeliosWddmAllocationDescV2::from_private_data`]. ⚠ The create-time
    /// generation write-back happens **in the runtime's buffer**, so a KMD that
    /// parses through here must write the returned generation back through the
    /// original pointer; this reader is the validation entry point, not a
    /// substitute for the write-back.
    pub fn from_private_data(bytes: &[u8]) -> Result<Self, HeliosOuterCommandAllocRejection> {
        let expected = core::mem::size_of::<Self>();
        if bytes.len() != expected {
            return Err(HeliosOuterCommandAllocRejection::PrivateDataSize {
                found: bytes.len(),
                expected,
            });
        }
        // With the length already exact, `try_pod_read_unaligned` cannot fail;
        // the arm is kept because a total function may not `unwrap`.
        bytemuck::try_pod_read_unaligned::<Self>(bytes).map_err(|_| {
            HeliosOuterCommandAllocRejection::PrivateDataSize {
                found: bytes.len(),
                expected,
            }
        })
    }

    /// Everything §10.6 fixes exactly, independent of create direction.
    fn validate_fixed(
        &self,
        package_generation: u64,
    ) -> Result<(), HeliosOuterCommandAllocRejection> {
        use HeliosOuterCommandAllocRejection as R;

        if self.magic != HELIOS_HOC1_MAGIC {
            return Err(R::Magic { found: self.magic });
        }
        if self.abi_version != HELIOS_HOC1_ABI_VERSION {
            return Err(R::AbiVersion {
                found: self.abi_version,
            });
        }
        if self.struct_size != HELIOS_HOC1_BYTES {
            return Err(R::StructSize {
                found: self.struct_size,
            });
        }
        // Zero is never a live package generation on *either* side: a zeroed
        // buffer must not be admitted just because the caller has not yet
        // established the package (crate-level rule, `crate::HELIOS_PACKAGE_GENERATION`).
        if self.package_generation != package_generation || self.package_generation == 0 {
            return Err(R::PackageGeneration {
                found: self.package_generation,
                expected: package_generation,
            });
        }
        if self.byte_size != HELIOS_HOC1_POOL_BYTES {
            return Err(R::ByteSize {
                found: self.byte_size,
                required: HELIOS_HOC1_POOL_BYTES,
            });
        }
        if self.extent_alignment != HELIOS_HOC1_EXTENT_ALIGNMENT {
            return Err(R::ExtentAlignment {
                found: self.extent_alignment,
                required: HELIOS_HOC1_EXTENT_ALIGNMENT,
            });
        }
        if self.access != HELIOS_HOC1_ACCESS_REQUIRED {
            return Err(R::Access {
                found: self.access,
                required: HELIOS_HOC1_ACCESS_REQUIRED,
            });
        }
        if self.cache_policy != HELIOS_HOC1_CACHE_WRITE_COMBINED {
            return Err(R::CachePolicy {
                found: self.cache_policy,
                required: HELIOS_HOC1_CACHE_WRITE_COMBINED,
            });
        }
        if self.physical_adapter_mask != HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0 {
            return Err(R::PhysicalAdapterMask {
                found: self.physical_adapter_mask,
                required: HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0,
            });
        }
        let mut i = 0;
        while i < self.reserved.len() {
            if self.reserved[i] != 0 {
                return Err(R::ReservedNonZero);
            }
            i += 1;
        }
        Ok(())
    }

    /// KMD-side validation of the record the UMD supplied to `pfnAllocateCb`.
    pub fn validate_create_input(
        &self,
        package_generation: u64,
    ) -> Result<(), HeliosOuterCommandAllocRejection> {
        self.validate_fixed(package_generation)?;
        if self.allocation_generation != 0 {
            return Err(
                HeliosOuterCommandAllocRejection::AllocationGenerationNonZeroOnInput {
                    found: self.allocation_generation,
                },
            );
        }
        Ok(())
    }

    /// UMD-side validation of the record after create: the same fixed fields
    /// plus the KMD's nonzero create-time generation write-back.
    pub fn validate_create_output(
        &self,
        package_generation: u64,
    ) -> Result<(), HeliosOuterCommandAllocRejection> {
        self.validate_fixed(package_generation)?;
        if self.allocation_generation == 0 {
            return Err(HeliosOuterCommandAllocRejection::AllocationGenerationZeroOnOutput);
        }
        Ok(())
    }
}

/// Why an extent reservation inside the C65 pool was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosExtentRejection {
    OffsetMisaligned {
        offset: u64,
        alignment: u32,
    },
    ByteLengthZero,
    ByteLengthAboveHob1Limit {
        found: u64,
        limit: u64,
    },
    /// `offset + bytes` overflowed `u64`.
    RangeOverflow {
        offset: u64,
        bytes: u64,
    },
    RangeOutsidePool {
        end: u64,
        pool_bytes: u64,
    },
    LiveExtentLimit {
        live: u32,
        limit: u32,
    },
}

/// Validate one candidate extent against the C65 pool geometry (§10.6): the
/// pool is exactly 64 MiB, every extent starts on a 64-KiB boundary, at most
/// 256 extents may be live, and each HOB1 remains at most 15 MiB.
///
/// `live_extents` is the count **already** live, i.e. before this reservation.
pub fn validate_pool_extent(
    offset: u64,
    bytes: u64,
    live_extents: u32,
) -> Result<(), HeliosExtentRejection> {
    use HeliosExtentRejection as R;

    if live_extents >= HELIOS_HOC1_MAX_LIVE_EXTENTS {
        return Err(R::LiveExtentLimit {
            live: live_extents,
            limit: HELIOS_HOC1_MAX_LIVE_EXTENTS,
        });
    }
    if offset % HELIOS_HOC1_EXTENT_ALIGNMENT as u64 != 0 {
        return Err(R::OffsetMisaligned {
            offset,
            alignment: HELIOS_HOC1_EXTENT_ALIGNMENT,
        });
    }
    if bytes == 0 {
        return Err(R::ByteLengthZero);
    }
    if bytes > HELIOS_HOB1_MAX_BYTES {
        return Err(R::ByteLengthAboveHob1Limit {
            found: bytes,
            limit: HELIOS_HOB1_MAX_BYTES,
        });
    }
    let end = match offset.checked_add(bytes) {
        Some(end) => end,
        None => return Err(R::RangeOverflow { offset, bytes }),
    };
    if end > HELIOS_HOC1_POOL_BYTES {
        return Err(R::RangeOutsidePool {
            end,
            pool_bytes: HELIOS_HOC1_POOL_BYTES,
        });
    }
    Ok(())
}

/// The immutable-seal state of one C65 pool extent (§10.6).
///
/// The lifecycle the doc describes, made exhaustive so a missing arm fails to
/// compile:
///
/// ```text
///   Free ──reserve──▶ Reserved ──write+SFENCE+release──▶ Sealed
///        ◀──retire── Retired ◀──HQC1 completes── Submitted ◀──SubmitCommandCb──
/// ```
///
/// * **Reserved** — a short device-local allocator lock recorded
///   `{queue context, context generation, batch ID}` ([`HeliosExtentRetirementV1`])
///   and released before any runtime, translator, KMD, or host call.
/// * **Sealed** — the complete HOB1 bytes are written, the x64 write-combining
///   store drain (`SFENCE` or the platform's equivalent primitive) has run, and
///   the state was published with a release operation. `pfnSubmitCommandCb`
///   occurs only after that publication.
/// * **Submitted** — ⛔ *"Once submitted, the extent is immutable."* No CPU
///   store may touch it. QEMU reads only after the resulting scheduler/device
///   enqueue.
/// * **Retired** — the exact recorded HQC1 value has completed; only now is the
///   extent reusable. A new allocation performs at most one completed-value
///   check per candidate extent and never loops on the mapped value.
/// * **Poisoned** — signal failure, reset, or removal poisoned the device;
///   old-generation extents stay unavailable until teardown and never return to
///   `Free`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosExtentSealState {
    Free,
    Reserved,
    Sealed,
    Submitted,
    Retired,
    Poisoned,
}

/// Why an extent seal-state transition was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosSealTransitionRejection {
    /// The transition is not part of the C65 lifecycle.
    IllegalTransition {
        from: HeliosExtentSealState,
        to: HeliosExtentSealState,
    },
    /// A poisoned extent never returns to service.
    ExtentPoisoned,
}

impl HeliosExtentSealState {
    /// The one legal successor set. Total function; a refusal names both ends.
    ///
    /// `Poisoned` is reachable from every live state (signal failure, reset, or
    /// removal) and from nothing afterwards — which is what "leaves all
    /// old-generation extents unavailable until teardown" means.
    pub fn advance(
        self,
        to: HeliosExtentSealState,
    ) -> Result<HeliosExtentSealState, HeliosSealTransitionRejection> {
        use HeliosExtentSealState as S;
        use HeliosSealTransitionRejection as R;

        if self == S::Poisoned {
            return Err(R::ExtentPoisoned);
        }
        if to == S::Poisoned {
            return Ok(S::Poisoned);
        }
        let legal = matches!(
            (self, to),
            (S::Free, S::Reserved)
                | (S::Reserved, S::Sealed)
                | (S::Sealed, S::Submitted)
                | (S::Submitted, S::Retired)
                | (S::Retired, S::Free)
        );
        if legal {
            Ok(to)
        } else {
            Err(R::IllegalTransition { from: self, to })
        }
    }

    /// May a CPU store still touch the extent's bytes?
    ///
    /// ⛔ False from `Sealed` onward: the write-combining drain has already
    /// published the bytes and `Submitted` is immutable by contract.
    #[inline]
    pub const fn cpu_writable(&self) -> bool {
        matches!(self, HeliosExtentSealState::Reserved)
    }
}

/// The exact HQC1 retirement tuple recorded on one C65 extent (§10.6).
///
/// The allocator lock records `{queue context, context generation, batch ID}`
/// at reservation; after `pfnSubmitCommandCb` succeeds the UMD inserts the next
/// strictly increasing HQC1 bottom-of-pipe signal on that **same** outer
/// context and tags the extent with that exact retire value. The extent is
/// reusable only after that exact value has completed.
///
/// ⚠ **UMD-local bookkeeping, not a wire record.** It is deliberately not
/// `Pod`/`repr(C)`: [`Self::queue_context`] is a process-local runtime context
/// token, and §10.4 forbids placing any user pointer or process-local handle on
/// the wire. Nothing here is ever serialized into HOB1, HOS1, or HOC1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeliosExtentRetirementV1 {
    /// Opaque, process-local token for the owning queue's runtime context.
    /// Compared for identity only; never transmitted.
    pub queue_context: u64,
    /// The owning outer context's HQA1 generation.
    pub context_generation: u64,
    /// The context-local batch ID written into this extent's HOB1.
    pub batch_id: u64,
    /// The HQC1 monitored-fence value signalled bottom-of-pipe after
    /// `pfnSubmitCommandCb` succeeded. Nonzero: HQC1 progress starts at zero
    /// and the first recorded value is already incremented.
    pub hqc1_value: u64,
}

/// Why an [`HeliosExtentRetirementV1`] tuple was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliosRetirementRejection {
    QueueContextZero,
    ContextGenerationZero,
    BatchIdZero,
    Hqc1ValueZero,
    /// The recorded value did not strictly increase on this context, so it
    /// cannot be a *next* bottom-of-pipe signal.
    Hqc1ValueNotIncreasing {
        found: u64,
        previous: u64,
    },
}

impl HeliosExtentRetirementV1 {
    /// Validate a freshly recorded retirement tuple.
    ///
    /// `previous_hqc1_value` is the last value already inserted on this outer
    /// context, or zero if none.
    pub fn validate(&self, previous_hqc1_value: u64) -> Result<(), HeliosRetirementRejection> {
        use HeliosRetirementRejection as R;

        if self.queue_context == 0 {
            return Err(R::QueueContextZero);
        }
        if self.context_generation == 0 {
            return Err(R::ContextGenerationZero);
        }
        if self.batch_id == 0 {
            return Err(R::BatchIdZero);
        }
        if self.hqc1_value == 0 {
            return Err(R::Hqc1ValueZero);
        }
        if self.hqc1_value <= previous_hqc1_value {
            return Err(R::Hqc1ValueNotIncreasing {
                found: self.hqc1_value,
                previous: previous_hqc1_value,
            });
        }
        Ok(())
    }

    /// Has the exact recorded HQC1 value completed on the owning context?
    ///
    /// `completed_hqc1_value` must be the monitored fence's completed value for
    /// **this** context — the caller reads it once per candidate extent and
    /// never loops on the mapped value (§10.6).
    #[inline]
    pub const fn is_retired_at(&self, completed_hqc1_value: u64) -> bool {
        completed_hqc1_value >= self.hqc1_value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: u64 = 0x0102_0304_0506_0708;

    // ── CRC-64/ECMA-182 ─────────────────────────────────────────────────────

    /// An independent, deliberately naive bit-at-a-time reference. It shares no
    /// code with [`CRC64_ECMA182_TABLE`], so agreement between the two is real
    /// evidence rather than a restatement of one implementation.
    fn crc64_reference(bytes: &[u8]) -> u64 {
        let mut crc: u64 = 0;
        for &b in bytes {
            crc ^= (b as u64) << 56;
            for _ in 0..8 {
                crc = if crc & (1 << 63) != 0 {
                    (crc << 1) ^ 0x42F0_E1EB_A9EA_3693
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    /// The published ECMA-182 check value, plus agreement with the independent
    /// reference on several shapes. A table typo cannot survive both.
    #[test]
    fn crc64_matches_the_published_check_value_and_an_independent_reference() {
        assert_eq!(crc64_ecma(b"123456789"), HELIOS_CRC64_ECMA182_CHECK);
        assert_eq!(crc64_ecma(b"123456789"), 0x6C40_DF5F_0B49_7347);
        assert_eq!(crc64_reference(b"123456789"), HELIOS_CRC64_ECMA182_CHECK);

        for v in [
            &b""[..],
            &b"H"[..],
            &b"HOB1"[..],
            &[0u8; 8][..],
            &[0xFFu8; 37][..],
            &[
                0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17,
            ][..],
        ] {
            assert_eq!(crc64_ecma(v), crc64_reference(v), "vector {v:?}");
        }

        // init=0/xorout=0 means an all-zero buffer checksums to zero. Pinned so
        // a C mirror is never "fixed" into a reflected variant by accident.
        assert_eq!(crc64_ecma(&[0u8; 64]), 0);

        // Chaining must equal a single pass, or a streaming mirror would drift.
        let (a, b) = (b"HOB1-header".as_slice(), b"payload-bytes".as_slice());
        let mut joined = [0u8; 24];
        joined[..a.len()].copy_from_slice(a);
        joined[a.len()..].copy_from_slice(b);
        assert_eq!(
            crc64_ecma(&joined),
            crc64_ecma_update(crc64_ecma_update(HELIOS_CRC64_ECMA182_INIT, a), b)
        );
    }

    #[test]
    fn hob1_record_crc_folds_the_checksum_field_as_zero() {
        let mut with_field = [0xABu8; 112];
        for byte in with_field
            .iter_mut()
            .skip(HELIOS_HOB1_CRC_FIELD_OFFSET)
            .take(8)
        {
            *byte = 0xFF;
        }
        let mut zeroed = with_field;
        for byte in zeroed.iter_mut().skip(HELIOS_HOB1_CRC_FIELD_OFFSET).take(8) {
            *byte = 0;
        }
        assert_eq!(hob1_record_crc64(&with_field), Ok(crc64_ecma(&zeroed)));
        assert_eq!(hob1_record_crc64(&with_field), hob1_record_crc64(&zeroed));

        assert_eq!(
            hob1_record_crc64(&[0u8; 111]),
            Err(HeliosOuterBatchRejection::RecordShorterThanHeader { found: 111 })
        );
    }

    // ── HWA2 ────────────────────────────────────────────────────────────────

    fn primary_desc() -> HeliosWddmAllocationDescV2 {
        let mut d = HeliosWddmAllocationDescV2::header(PKG, 0x51);
        d.byte_size = 1920 * 4 * 1080;
        d.width = 1920;
        d.height = 1080;
        d.depth_or_array_size = 1;
        d.mip_levels = 1;
        d.dxgi_format = DXGI_FORMAT_B8G8R8A8_UNORM;
        d.d3d_ddi_format = D3DDDIFMT_A8R8G8B8;
        d.sample_count = 1;
        d.sample_quality = 0;
        d.allocation_kind = HELIOS_HWA2_KIND_STANDARD_PRIMARY;
        d.flags = HELIOS_HWA2_FLAG_PRIMARY
            | HELIOS_HWA2_FLAG_DISPLAYABLE
            | HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE
            | HELIOS_HWA2_FLAG_SHARED
            | HELIOS_HWA2_FLAG_STANDARD;
        d.bind_flags = HELIOS_HWA2_BIND_RENDER_TARGET | HELIOS_HWA2_BIND_SHADER_RESOURCE;
        d.vidpn_source = 0;
        d.standard_allocation_type = 1; // D3DKMDT_STANDARDALLOCATION_SHAREDPRIMARYSURFACE
        d.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        d.memory_class = HELIOS_HWA2_MEMORY_DEVICE_LOCAL;
        d.plane_count = 1;
        d.planes[0] = HeliosWddmPlaneRecordV2 {
            offset: 0,
            row_pitch: 1920 * 4,
            slice_pitch: 1920 * 4 * 1080,
        };
        d
    }

    fn buffer_desc() -> HeliosWddmAllocationDescV2 {
        let mut d = HeliosWddmAllocationDescV2::header(PKG, 0x77);
        d.byte_size = 65536;
        d.allocation_kind = HELIOS_HWA2_KIND_BUFFER;
        d.bind_flags = HELIOS_HWA2_BIND_CONSTANT_BUFFER;
        d.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
        d.memory_class = HELIOS_HWA2_MEMORY_CPU_VISIBLE;
        d.flags = HELIOS_HWA2_FLAG_CPU_VISIBLE;
        d
    }

    #[test]
    fn hwa2_accepts_the_two_canonical_shapes() {
        assert_eq!(primary_desc().validate(PKG), Ok(()));
        assert_eq!(buffer_desc().validate(PKG), Ok(()));
    }

    /// The create-input form of [`primary_desc`]: the same 168 bytes with the
    /// two things only the kernel may write taken back out.
    fn primary_create_input() -> HeliosWddmAllocationDescV2 {
        let mut d = primary_desc();
        d.allocation_generation = 0;
        d.flags &= !HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        d
    }

    /// HWA2 is a two-stage record (`docs/retirement/K4-CONTRACT.md` §1): the
    /// UMD supplies a complete descriptor, the KMD validates it in full, and
    /// the KMD writes back all 168 bytes. This pins the whole difference
    /// between the two sides — `allocation_generation` plus
    /// [`HELIOS_HWA2_FLAG_KMD_OWNED_MASK`] — by round-tripping one record
    /// through both stages and asserting the echo is verbatim.
    #[test]
    fn hwa2_create_input_becomes_the_output_by_adding_only_kmd_owned_fields() {
        let input = primary_create_input();
        assert_eq!(input.validate_create_input(PKG), Ok(()));

        // Before the write-back, the output form must refuse — exactly as
        // HOC1's does, and with the pre-existing zero-generation reason.
        assert_eq!(
            input.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationZero)
        );
        assert_eq!(
            input.validate(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationZero)
        );

        // The KMD stamps its generation and the Direct Flip finding it has
        // now actually established, and touches nothing else.
        let mut output = input;
        output.allocation_generation = 0x51;
        output.flags |= HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
        assert_eq!(output.validate_create_output(PKG), Ok(()));
        assert_eq!(output.validate(PKG), Ok(()));
        // …and that is byte-for-byte the canonical output record.
        assert_eq!(output, primary_desc());

        // The finished record is no longer a legal input: a UMD that resubmits
        // it is claiming an identity the kernel assigns.
        assert_eq!(
            output.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationNonZeroOnInput { found: 0x51 })
        );

        // Every UMD-supplied field is echoed verbatim: undo the two KMD-owned
        // writes and the output is the input again, all 168 bytes.
        let mut echoed = output;
        echoed.allocation_generation = 0;
        echoed.flags &= !HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        assert_eq!(echoed, input);
    }

    /// The input stage's own two rules, each by name. §10.3 says the KMD sets
    /// `DIRECT_FLIP_COMPATIBLE` and `D3D12_RUNTIME_PRIMARY` and that "neither
    /// is inferred by an opener" — so a UMD that pre-sets one is refused, not
    /// silently corrected, because after an echo nothing in the finished record
    /// could distinguish the UMD's guess from the kernel's finding.
    #[test]
    fn hwa2_create_input_refuses_the_fields_only_the_kmd_writes() {
        let mut input = buffer_desc();
        input.allocation_generation = 0;
        assert_eq!(input.validate_create_input(PKG), Ok(()));

        let mut prefilled = input;
        prefilled.allocation_generation = 1;
        assert_eq!(
            prefilled.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationNonZeroOnInput { found: 1 })
        );

        // Each KMD-owned bit alone. Note the buffer kind could never legally
        // carry `DIRECT_FLIP_COMPATIBLE` at all — and the input stage still
        // refuses it for being KMD-owned, ahead of the Direct Flip cross-field
        // rule, so the counter names the defect the UMD actually has.
        for bit in [
            HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE,
            HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY,
        ] {
            let mut d = input;
            d.flags |= bit;
            assert_eq!(
                d.validate_create_input(PKG),
                Err(HeliosAllocDescRejection::KmdOwnedFlagSetOnInput { bits: bit })
            );
        }

        // Both at once: the payload names the whole offending subset, not the
        // first bit found.
        let mut both = input;
        both.flags |= HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        assert_eq!(
            both.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::KmdOwnedFlagSetOnInput {
                bits: HELIOS_HWA2_FLAG_KMD_OWNED_MASK,
            })
        );

        // An undefined bit still wins: the flags word is checked as a
        // vocabulary before it is checked as ownership.
        let mut junk = input;
        junk.flags |= HELIOS_HWA2_FLAG_KMD_OWNED_MASK | (1 << 31);
        assert_eq!(
            junk.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::UnknownFlagBits { found: 1 << 31 })
        );

        // The KMD-owned bits are legal on the output side, which is the whole
        // asymmetry: the same record passes once the kernel owns it.
        let mut stamped = both;
        stamped.allocation_generation = 9;
        stamped.flags = input.flags | HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        assert_eq!(
            stamped.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::D3D12RuntimePrimaryWithoutPrimary)
        );
        let mut df = primary_desc();
        df.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        df.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(df.validate_create_output(PKG), Ok(()));
    }

    /// One core behind three entry points, so no §10.3 cross-field rule can be
    /// enforced on the open path and missed on the create path that produces
    /// the bytes the open path then trusts as `const`. Each rule below is
    /// asserted on all three, with only the stage fields changed.
    #[test]
    fn hwa2_cross_field_rules_hold_on_every_stage() {
        let legal_input = primary_create_input();

        // A non-image kind carrying texel geometry.
        let mut geometry = buffer_desc();
        geometry.allocation_generation = 0;
        geometry.width = 64;
        let expected = Err(HeliosAllocDescRejection::NonImageGeometryNonZero {
            field: HeliosAllocDescField::Width,
            found: 64,
        });
        assert_eq!(geometry.validate_create_input(PKG), expected);
        geometry.allocation_generation = 7;
        assert_eq!(geometry.validate_create_output(PKG), expected);
        assert_eq!(geometry.validate(PKG), expected);

        // A plane escaping `byte_size`.
        let mut plane = legal_input;
        plane.planes[0].slice_pitch = plane.planes[0].slice_pitch.wrapping_add(4);
        plane.byte_size = plane.planes[0].slice_pitch as u64 - 4;
        let expected = Err(HeliosAllocDescRejection::PlaneRangeExceedsByteSize { index: 0 });
        assert_eq!(plane.validate_create_input(PKG), expected);
        plane.allocation_generation = 7;
        assert_eq!(plane.validate_create_output(PKG), expected);
        assert_eq!(plane.validate(PKG), expected);

        // Memory class disagreeing with CPU visibility.
        let mut memory = buffer_desc();
        memory.allocation_generation = 0;
        memory.memory_class = HELIOS_HWA2_MEMORY_DEVICE_LOCAL;
        let expected = Err(HeliosAllocDescRejection::MemoryClassCpuVisibilityMismatch {
            memory_class: HELIOS_HWA2_MEMORY_DEVICE_LOCAL,
            flags: memory.flags,
        });
        assert_eq!(memory.validate_create_input(PKG), expected);
        memory.allocation_generation = 7;
        assert_eq!(memory.validate_create_output(PKG), expected);
        assert_eq!(memory.validate(PKG), expected);

        // And the identity/package rules, which precede every stage rule.
        let mut wrong_package = legal_input;
        wrong_package.package_generation = PKG ^ 1;
        let expected = Err(HeliosAllocDescRejection::PackageGeneration {
            found: PKG ^ 1,
            expected: PKG,
        });
        assert_eq!(wrong_package.validate_create_input(PKG), expected);
        wrong_package.allocation_generation = 7;
        assert_eq!(wrong_package.validate_create_output(PKG), expected);
        assert_eq!(wrong_package.validate(PKG), expected);
    }

    /// Zero is never a wildcard on *either* side of the package-generation
    /// comparison. A zeroed private-data buffer reaching a caller that has not
    /// yet established the package generation must be refused, not admitted
    /// because `0 == 0` — see [`crate::HELIOS_PACKAGE_GENERATION`].
    #[test]
    fn hwa2_refuses_a_zero_package_generation_on_both_sides() {
        let mut d = buffer_desc();
        d.package_generation = 0;
        assert_eq!(
            d.validate(0),
            Err(HeliosAllocDescRejection::PackageGeneration {
                found: 0,
                expected: 0,
            })
        );
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::PackageGeneration {
                found: 0,
                expected: PKG,
            })
        );

        let d = buffer_desc();
        assert_eq!(
            d.validate(0),
            Err(HeliosAllocDescRejection::PackageGeneration {
                found: PKG,
                expected: 0,
            })
        );
    }

    /// §10.3's "truncated" refusal needs a bounded reader, or every consumer
    /// hand-rolls the length check on a `(pPrivateDriverData, size)` pair — and
    /// a KMD that reads 168 bytes out of a shorter buffer takes an
    /// out-of-bounds kernel read no validator in this crate could catch.
    #[test]
    fn cpu_backing_side_record_refuses_everything_but_its_own_shape() {
        let good = HeliosCpuBackingV1::new(0x1_0000_0000, 4 << 20);
        assert!(good.validate());
        assert_eq!(
            HeliosCpuBackingV1::from_private_data(bytemuck::bytes_of(&good)),
            Some(good)
        );
        // Wrong length is "no pages offered", never a partial read.
        assert_eq!(
            HeliosCpuBackingV1::from_private_data(&bytemuck::bytes_of(&good)[..23]),
            None
        );
        assert_eq!(HeliosCpuBackingV1::from_private_data(&[]), None);
        // A sub-page VA would make the MDL's first page cover bytes the
        // creator does not own; a sub-page length, its last.
        assert!(!HeliosCpuBackingV1::new(0x1_0000_0001, 4 << 20).validate());
        assert!(!HeliosCpuBackingV1::new(0x1_0000_0000, (4 << 20) + 1).validate());
        assert!(!HeliosCpuBackingV1::new(0, 4 << 20).validate());
        assert!(!HeliosCpuBackingV1::new(0x1_0000_0000, 0).validate());
        // Another producer's bytes of the same length are refused by magic.
        let mut foreign = good;
        foreign.magic = 0xDEAD_BEEF;
        assert_eq!(
            HeliosCpuBackingV1::from_private_data(bytemuck::bytes_of(&foreign)),
            None
        );
    }

    #[test]
    fn create_time_records_are_read_through_a_bounded_reader() {
        let d = primary_desc();
        let bytes = bytemuck::bytes_of(&d);
        assert_eq!(
            HeliosWddmAllocationDescV2::from_private_data(bytes),
            Ok(d)
        );
        assert_eq!(
            HeliosWddmAllocationDescV2::from_private_data(&bytes[..40]),
            Err(HeliosAllocDescRejection::PrivateDataSize {
                found: 40,
                expected: HELIOS_HWA2_BYTES as usize,
            })
        );

        let c = HeliosOuterCommandAllocationV1::new(PKG);
        let bytes = bytemuck::bytes_of(&c);
        assert_eq!(
            HeliosOuterCommandAllocationV1::from_private_data(bytes),
            Ok(c)
        );
        assert_eq!(
            HeliosOuterCommandAllocationV1::from_private_data(&bytes[..63]),
            Err(HeliosOuterCommandAllocRejection::PrivateDataSize {
                found: 63,
                expected: HELIOS_HOC1_BYTES as usize,
            })
        );

        // A correct-length buffer at an odd address must parse, not be refused
        // with a *length* error that names the correct length.
        let mut staging = [0u8; 72];
        staging[4..68].copy_from_slice(bytemuck::bytes_of(&c));
        assert_eq!(
            HeliosOuterCommandAllocationV1::from_private_data(&staging[4..68]),
            Ok(c)
        );

        // Same for HWA2, at an *odd* offset: `allocation_generation` is a `u64`
        // at offset 16 and the record's alignment is 8, so this slice satisfies
        // neither. It must still parse — the read is unaligned on purpose —
        // and a longer-than-exact buffer must still be refused by length.
        let d = primary_desc();
        let mut staging = [0u8; 180];
        staging[1..169].copy_from_slice(bytemuck::bytes_of(&d));
        assert_eq!(
            HeliosWddmAllocationDescV2::from_private_data(&staging[1..169]),
            Ok(d)
        );
        assert_eq!(
            HeliosWddmAllocationDescV2::from_private_data(&staging[1..170]),
            Err(HeliosAllocDescRejection::PrivateDataSize {
                found: 169,
                expected: HELIOS_HWA2_BYTES as usize,
            })
        );
    }

    #[test]
    fn hwa2_refuses_wrong_identity_and_generation_by_name() {
        let mut d = primary_desc();
        d.magic = 0;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::Magic { found: 0 })
        );

        let mut d = primary_desc();
        d.abi_version = 1;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::AbiVersion { found: 1 })
        );

        let mut d = primary_desc();
        d.struct_size = 160;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::StructSize { found: 160 })
        );

        assert_eq!(
            primary_desc().validate(PKG + 1),
            Err(HeliosAllocDescRejection::PackageGeneration {
                found: PKG,
                expected: PKG + 1,
            })
        );

        let mut d = primary_desc();
        d.allocation_generation = 0;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationZero)
        );

        let mut d = primary_desc();
        d.reserved = 1;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::ReservedNonZero { found: 1 })
        );

        // An all-zero buffer must fail, and on the magic — never fall through
        // to a legacy parser.
        let zero = HeliosWddmAllocationDescV2::zeroed();
        assert_eq!(
            zero.validate(PKG),
            Err(HeliosAllocDescRejection::Magic { found: 0 })
        );
    }

    #[test]
    fn hwa2_refuses_every_undefined_bit_and_class() {
        let mut d = primary_desc();
        d.flags |= 1 << 11;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::UnknownFlagBits { found: 1 << 11 })
        );

        let mut d = primary_desc();
        d.bind_flags |= 1 << 11;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::UnknownBindBits { found: 1 << 11 })
        );

        let mut d = primary_desc();
        d.misc_flags = 1 << 4;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::UnknownMiscBits { found: 1 << 4 })
        );

        let mut d = primary_desc();
        d.swizzle_class = HELIOS_HWA2_SWIZZLE_MAX + 1;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::UnknownSwizzleClass {
                found: HELIOS_HWA2_SWIZZLE_MAX + 1
            })
        );

        let mut d = primary_desc();
        d.memory_class = HELIOS_HWA2_MEMORY_INVALID;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::UnknownMemoryClass { found: 0 })
        );

        let mut d = primary_desc();
        d.allocation_kind = HELIOS_HWA2_KIND_MAX + 1;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::UnknownAllocationKind {
                found: HELIOS_HWA2_KIND_MAX + 1
            })
        );
    }

    /// The create-input form of a C44 D3D12 runtime primary: PRIMARY, the
    /// [`D3DDDI_ID_UNINITIALIZED`] sentinel, and `D3D12_RUNTIME_PRIMARY` **clear**
    /// — because it is KMD-owned and the creator may not assert it.
    ///
    /// This is not one shape among several: it is the *only* shape a D3D12 UMD
    /// can put on the wire (`docs/retirement/K4-CONTRACT.md` §1.1).
    fn d3d12_primary_create_input() -> HeliosWddmAllocationDescV2 {
        let mut d = primary_create_input();
        d.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        d
    }

    /// Both primary shapes, both stages, end to end — the regression test for a
    /// defect the seam review found by enumerating what a D3D12 UMD can actually
    /// send: the C44 primary/VidPn rule was stage-INDEPENDENT, so
    /// [`d3d12_primary_create_input`] was refused with
    /// `PrimaryVidPnSourceNotConcrete`, the same record with the bit pre-set was
    /// refused with `KmdOwnedFlagSetOnInput`, and those are the only two shapes
    /// that exist. A D3D12 runtime primary could therefore never be created, and
    /// the KMD's stamp was unreachable code — a rule that refuses every input
    /// that could reach the stamp is indistinguishable from not implementing the
    /// stamp at all.
    #[test]
    fn hwa2_admits_both_primary_shapes_as_input_and_refuses_them_unstamped_on_output() {
        // ── the D3D11 shape: PRIMARY with a concrete source ─────────────────
        let d11_in = primary_create_input();
        assert_eq!(d11_in.validate_create_input(PKG), Ok(()));
        // Output before the KMD writes anything: refused for the missing stamp,
        // not for the VidPn source.
        assert_eq!(
            d11_in.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationZero)
        );
        let mut d11_out = d11_in;
        d11_out.allocation_generation = 0x51;
        d11_out.flags |= HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
        assert_eq!(d11_out.validate_create_output(PKG), Ok(()));
        assert_eq!(d11_out.validate(PKG), Ok(()));

        // ── the D3D12 shape: PRIMARY with the sentinel and no bit ───────────
        let d12_in = d3d12_primary_create_input();
        assert_eq!(d12_in.validate_create_input(PKG), Ok(()));

        // The one alternative a D3D12 UMD might try — asserting the bit itself —
        // is still refused by name, and by the KMD-ownership rule rather than by
        // C44, so the counter names the defect the creator actually has.
        let mut d12_presumptuous = d12_in;
        d12_presumptuous.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        assert_eq!(
            d12_presumptuous.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::KmdOwnedFlagSetOnInput {
                bits: HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY,
            })
        );

        // Output before the stamp: still the generation, for the same reason.
        assert_eq!(
            d12_in.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationZero)
        );

        // ⛔ The output rule is NOT weakened. A generation-stamped record whose
        // C44 bit the KMD forgot is exactly the "primary belonging to no source"
        // an opener must never see, and it is still refused by name.
        let mut d12_half = d12_in;
        d12_half.allocation_generation = 0x52;
        assert_eq!(
            d12_half.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::PrimaryVidPnSourceNotConcrete)
        );
        assert_eq!(
            d12_half.validate(PKG),
            Err(HeliosAllocDescRejection::PrimaryVidPnSourceNotConcrete)
        );

        // Fully stamped: accepted on both output entry points.
        let mut d12_out = d12_half;
        d12_out.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        assert_eq!(d12_out.validate_create_output(PKG), Ok(()));
        assert_eq!(d12_out.validate(PKG), Ok(()));

        // And the finished record is not a legal input again: the generation
        // check precedes the flag check, so the refusal order is the documented
        // one even when both KMD-owned things are present at once.
        assert_eq!(
            d12_out.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::AllocationGenerationNonZeroOnInput { found: 0x52 })
        );
        let mut regen = d12_out;
        regen.allocation_generation = 0;
        assert_eq!(
            regen.validate_create_input(PKG),
            Err(HeliosAllocDescRejection::KmdOwnedFlagSetOnInput {
                bits: HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY,
            })
        );

        // Undoing the two KMD-owned writes gives back the input, all 168 bytes:
        // the D3D12 stamp echoes every UMD field exactly as the D3D11 one does.
        let mut echoed = d12_out;
        echoed.allocation_generation = 0;
        echoed.flags &= !HELIOS_HWA2_FLAG_KMD_OWNED_MASK;
        assert_eq!(echoed, d12_in);
    }

    /// Every illegal C44 combination, named, on the stage that can reach it.
    ///
    /// The point of the single shared core is that a rule cannot be enforced on
    /// one path and not another, so the two arms that are NOT stage-dependent are
    /// asserted on both stages here — only the bit-less-primary arm differs, and
    /// it differs in exactly one direction.
    #[test]
    fn hwa2_c44_illegal_combinations_are_refused_by_name_on_every_reachable_stage() {
        // The bit without the sentinel — output only, since the bit itself is
        // illegal on input.
        let mut bit_no_sentinel = primary_desc();
        bit_no_sentinel.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        assert_eq!(
            bit_no_sentinel.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::D3D12RuntimePrimaryNotSentinel { found: 0 })
        );

        // The bit without PRIMARY — likewise output only.
        let mut bit_no_primary = primary_desc();
        bit_no_primary.flags &=
            !(HELIOS_HWA2_FLAG_PRIMARY | HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE);
        bit_no_primary.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        bit_no_primary.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(
            bit_no_primary.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::D3D12RuntimePrimaryWithoutPrimary)
        );

        // A non-primary carrying a concrete source. Nothing about this arm is
        // stage-dependent, and it is asserted on both so the gate above cannot
        // quietly grow to cover it.
        let mut non_primary = buffer_desc();
        non_primary.allocation_generation = 0;
        non_primary.vidpn_source = 2;
        let expected = Err(HeliosAllocDescRejection::NonPrimaryVidPnSourceNotSentinel { found: 2 });
        assert_eq!(non_primary.validate_create_input(PKG), expected);
        non_primary.allocation_generation = 7;
        assert_eq!(non_primary.validate_create_output(PKG), expected);
        assert_eq!(non_primary.validate(PKG), expected);

        // A D3D11 primary that lost its concrete source: refused on output,
        // admitted on input as the D3D12 request it is indistinguishable from.
        // ⚠ This asymmetry is the whole fix, and it is deliberate — before the
        // KMD reads the D3D12 create record there is nothing in these 168 bytes
        // that could tell the two apart, so the input stage cannot decide it and
        // must not pretend to.
        let mut sentinel_primary = primary_desc();
        sentinel_primary.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(
            sentinel_primary.validate_create_output(PKG),
            Err(HeliosAllocDescRejection::PrimaryVidPnSourceNotConcrete)
        );
        assert_eq!(
            sentinel_primary.validate(PKG),
            Err(HeliosAllocDescRejection::PrimaryVidPnSourceNotConcrete)
        );
        assert_eq!(
            d3d12_primary_create_input().validate_create_input(PKG),
            Ok(())
        );
    }

    /// C44: the `D3D12_RUNTIME_PRIMARY` bit and `D3DDDI_ID_UNINITIALIZED` are
    /// cross-validated, and neither is inferred from the other.
    #[test]
    fn hwa2_cross_validates_the_c44_runtime_primary_pair() {
        // Accepted only with PRIMARY *and* the sentinel.
        let mut ok = primary_desc();
        ok.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        ok.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(ok.validate(PKG), Ok(()));

        // The bit without the sentinel.
        let mut d = ok;
        d.vidpn_source = 0;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::D3D12RuntimePrimaryNotSentinel { found: 0 })
        );

        // The bit without PRIMARY.
        let mut d = ok;
        d.flags &= !HELIOS_HWA2_FLAG_PRIMARY;
        d.flags &= !HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::D3D12RuntimePrimaryWithoutPrimary)
        );

        // A conventional D3D11 primary may not carry the sentinel …
        let mut d = primary_desc();
        d.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::PrimaryVidPnSourceNotConcrete)
        );

        // … and a non-primary must carry it.
        let mut d = buffer_desc();
        d.vidpn_source = 2;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::NonPrimaryVidPnSourceNotSentinel { found: 2 })
        );
    }

    #[test]
    fn hwa2_bounds_every_plane_inside_byte_size() {
        let mut d = primary_desc();
        d.planes[0].slice_pitch = d.byte_size as u32 + 4;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::PlaneRangeExceedsByteSize { index: 0 })
        );

        let mut d = primary_desc();
        d.planes[0].offset = u64::MAX;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::PlaneRangeOverflow { index: 0 })
        );

        let mut d = primary_desc();
        d.planes[0].row_pitch = d.planes[0].slice_pitch + 1;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::PlaneRowPitchExceedsSlicePitch { index: 0 })
        );

        // An unused record must be zero, even though plane_count ignores it.
        let mut d = primary_desc();
        d.planes[3].offset = 8;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::UnusedPlaneNonZero { index: 3 })
        );

        let mut d = primary_desc();
        d.plane_count = HELIOS_HWA2_MAX_PLANES + 1;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::PlaneCountOutOfRange {
                found: HELIOS_HWA2_MAX_PLANES + 1
            })
        );
    }

    #[test]
    fn hwa2_separates_image_geometry_from_non_image_kinds() {
        let mut d = primary_desc();
        d.width = 0;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::ImageGeometryZero {
                field: HeliosAllocDescField::Width
            })
        );

        let mut d = buffer_desc();
        d.height = 4;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::NonImageGeometryNonZero {
                field: HeliosAllocDescField::Height,
                found: 4
            })
        );

        let mut d = buffer_desc();
        d.dxgi_format = DXGI_FORMAT_B8G8R8A8_UNORM;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::NonImageDxgiFormatSet {
                found: DXGI_FORMAT_B8G8R8A8_UNORM
            })
        );

        let mut d = primary_desc();
        d.dxgi_format = DXGI_FORMAT_UNKNOWN;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::ImageDxgiFormatUnknown)
        );
    }

    #[test]
    fn hwa2_couples_the_standard_flag_to_the_os_type_and_kind() {
        let mut d = primary_desc();
        d.standard_allocation_type = 0;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::StandardAllocationTypeZero)
        );

        let mut d = primary_desc();
        d.flags &= !HELIOS_HWA2_FLAG_STANDARD;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::StandardAllocationTypeWithoutStandardFlag { found: 1 })
        );

        // A GDI surface has no kind of its own: IMAGE + STANDARD + the exact OS
        // enum is how §10.3's enum expresses it, and that must validate.
        let mut gdi = primary_desc();
        gdi.allocation_kind = HELIOS_HWA2_KIND_IMAGE;
        gdi.flags &= !(HELIOS_HWA2_FLAG_PRIMARY | HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE);
        gdi.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        gdi.standard_allocation_type = 4; // D3DKMDT_STANDARDALLOCATION_GDISURFACE
        gdi.misc_flags = HELIOS_HWA2_MISC_GDI_COMPATIBLE;
        assert_eq!(gdi.validate(PKG), Ok(()));
    }

    #[test]
    fn hwa2_gates_the_direct_flip_bit_on_its_stated_preconditions() {
        let mut d = primary_desc();
        d.flags |= HELIOS_HWA2_FLAG_PROTECTED;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::DirectFlipProtected)
        );

        let mut d = primary_desc();
        d.flags |= HELIOS_HWA2_FLAG_CROSS_ADAPTER;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::DirectFlipCrossAdapter)
        );

        let mut d = primary_desc();
        d.swizzle_class = HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL;
        assert_eq!(
            d.validate(PKG),
            Err(
                HeliosAllocDescRejection::DirectFlipUnsupportedSwizzleClass {
                    found: HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL
                }
            )
        );
    }

    #[test]
    fn hwa2_couples_the_memory_class_to_cpu_visibility() {
        let mut d = buffer_desc();
        d.flags &= !HELIOS_HWA2_FLAG_CPU_VISIBLE;
        assert_eq!(
            d.validate(PKG),
            Err(HeliosAllocDescRejection::MemoryClassCpuVisibilityMismatch {
                memory_class: HELIOS_HWA2_MEMORY_CPU_VISIBLE,
                flags: 0,
            })
        );
    }

    // ── C43/C44 pair admission ──────────────────────────────────────────────

    #[test]
    fn direct_flip_pair_admits_equal_concrete_sources_and_the_c44_sentinel() {
        let app = primary_desc();
        let dwm = primary_desc();
        assert_eq!(
            direct_flip_pair_admissible(&app, &dwm, PKG, HeliosAdapterMatch::SameAdapter, 0),
            Ok(())
        );

        // Concrete sources must be equal.
        let mut other = primary_desc();
        other.vidpn_source = 1;
        assert_eq!(
            direct_flip_pair_admissible(&app, &other, PKG, HeliosAdapterMatch::SameAdapter, 0),
            Err(HeliosDirectFlipRefusal::ConcreteVidPnSourceMismatch { app: 0, dwm: 1 })
        );

        // A C44 runtime primary's sentinel is never compared as an identity.
        let mut runtime = primary_desc();
        runtime.flags |= HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY;
        runtime.vidpn_source = D3DDDI_ID_UNINITIALIZED;
        assert_eq!(
            direct_flip_pair_admissible(&runtime, &app, PKG, HeliosAdapterMatch::SameAdapter, 0),
            Ok(())
        );

        // IMMEDIATE is refused in this generation.
        assert_eq!(
            direct_flip_pair_admissible(&app, &dwm, PKG, HeliosAdapterMatch::SameAdapter, 1),
            Err(HeliosDirectFlipRefusal::ImmediateFlipRequested { flags: 1 })
        );

        // §3: no cross-adapter inference.
        assert_eq!(
            direct_flip_pair_admissible(&app, &dwm, PKG, HeliosAdapterMatch::DifferentOrUnknown, 0),
            Err(HeliosDirectFlipRefusal::NotSameAdapter)
        );

        // Stereo is refused.
        let mut stereo = primary_desc();
        stereo.flags |= HELIOS_HWA2_FLAG_STEREO;
        assert_eq!(
            direct_flip_pair_admissible(&stereo, &dwm, PKG, HeliosAdapterMatch::SameAdapter, 0),
            Err(HeliosDirectFlipRefusal::ForbiddenFlags {
                flags: HELIOS_HWA2_FLAG_STEREO
            })
        );
    }

    // ── HOB1 ────────────────────────────────────────────────────────────────

    const REC_BYTES: usize = 232;
    const USE_OFF: usize = 112;
    const OPERAND_OFF: usize = 152;
    const PAYLOAD_OFF: usize = 168;
    const PAYLOAD_LEN: usize = 64;

    /// 8-aligned backing store so [`hob1_header`] can borrow the header without
    /// an unaligned read, exactly as the KMD's copied command slot is.
    #[repr(C, align(8))]
    struct Record([u8; REC_BYTES]);

    /// `D3DDDI_ALLOCATIONLIST` length the D3D11 arm of these fixtures assumes.
    const ALLOC_LIST: u32 = 8;

    fn expectation(flags: u32) -> HeliosOuterBatchExpectation {
        HeliosOuterBatchExpectation {
            package_generation: PKG,
            session_generation: 0x1111_1111_1111_1111,
            context_generation: 0x2222_2222_2222_2222,
            endpoint_id: 3,
            flags,
            max_command_bytes: 256 * 1024,
            last_batch_id: 6,
            // The virtual arm has no allocation list at all (§10.4).
            allocation_list_count: if flags == HELIOS_HOB1_FLAG_D3D12_VIRTUAL {
                0
            } else {
                ALLOC_LIST
            },
        }
    }

    fn header(flags: u32) -> HeliosOuterBatchV1 {
        HeliosOuterBatchV1 {
            magic: HELIOS_HOB1_MAGIC,
            abi_version: HELIOS_HOB1_ABI_VERSION,
            header_size: HELIOS_HOB1_HEADER_BYTES,
            package_generation: PKG,
            session_generation: 0x1111_1111_1111_1111,
            context_generation: 0x2222_2222_2222_2222,
            batch_id: 7,
            endpoint_id: 3,
            flags,
            total_bytes: REC_BYTES as u64,
            payload_offset: PAYLOAD_OFF as u32,
            payload_bytes: PAYLOAD_LEN as u32,
            use_offset: USE_OFF as u32,
            use_count: 1,
            operand_offset: OPERAND_OFF as u32,
            operand_count: 1,
            crc64: 0,
            reserved: [0; 24],
        }
    }

    fn use_record(kind: u16, address_or_index: u64) -> HeliosOuterBatchUseV1 {
        HeliosOuterBatchUseV1 {
            address_or_index,
            byte_length: 4096,
            expected_allocation_generation: 0x51,
            access_flags: HELIOS_HOB1_ACCESS_READ | HELIOS_HOB1_ACCESS_WRITE,
            identity_kind: kind,
            operand_count: 1,
            first_operand: 0,
            reserved: 0,
        }
    }

    fn operand_record() -> HeliosOuterBatchOperandV1 {
        HeliosOuterBatchOperandV1 {
            payload_offset: PAYLOAD_OFF as u32,
            use_index: 0,
            operand_kind: HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE,
            encoded_width: HELIOS_HOB1_OPERAND_WIDTH_8,
            reserved: 0,
        }
    }

    /// Assemble a complete, CRC-sealed record. `mutate` runs on the header
    /// before sealing, so a test can corrupt exactly one field.
    fn build(flags: u32, mutate: impl FnOnce(&mut HeliosOuterBatchV1)) -> Record {
        let mut h = header(flags);
        mutate(&mut h);
        let ident = if flags == HELIOS_HOB1_FLAG_D3D12_VIRTUAL {
            use_record(HELIOS_HOB1_IDENTITY_D3D12_GPUVA, 0x1_0000_0000)
        } else {
            use_record(HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, 5)
        };

        let mut rec = Record([0u8; REC_BYTES]);
        rec.0[USE_OFF..USE_OFF + 40].copy_from_slice(bytemuck::bytes_of(&ident));
        rec.0[OPERAND_OFF..OPERAND_OFF + 16].copy_from_slice(bytemuck::bytes_of(&operand_record()));
        rec.0[..112].copy_from_slice(bytemuck::bytes_of(&h));
        let crc = hob1_record_crc64(&rec.0).expect("record covers the header");
        h.crc64 = crc;
        rec.0[..112].copy_from_slice(bytemuck::bytes_of(&h));
        rec
    }

    #[test]
    fn hob1_accepts_a_sealed_record_on_both_arms() {
        let d3d11 = build(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, |_| {});
        assert_eq!(
            validate_batch_record(&d3d11.0, &expectation(HELIOS_HOB1_FLAG_D3D11_PHYSICAL)),
            Ok(())
        );
        let d3d12 = build(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, |_| {});
        assert_eq!(
            validate_batch_record(&d3d12.0, &expectation(HELIOS_HOB1_FLAG_D3D12_VIRTUAL)),
            Ok(())
        );
    }

    #[test]
    fn hob1_refuses_a_flipped_bit_anywhere_in_the_record() {
        let mut rec = build(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, |_| {});
        rec.0[PAYLOAD_OFF + 9] ^= 0x01;
        assert!(matches!(
            validate_batch_record(&rec.0, &expectation(HELIOS_HOB1_FLAG_D3D11_PHYSICAL)),
            Err(HeliosOuterBatchRejection::Crc64Mismatch { .. })
        ));
    }

    #[test]
    fn hob1_enforces_every_stated_bound() {
        let exp = expectation(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);

        let over = header(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);
        let mut h = over;
        h.total_bytes = HELIOS_HOB1_MAX_BYTES + 1;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::TotalBytesAboveLimit {
                found: HELIOS_HOB1_MAX_BYTES + 1,
                limit: HELIOS_HOB1_MAX_BYTES,
            })
        );

        let mut h = over;
        h.total_bytes = exp.max_command_bytes + 1;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::TotalBytesAboveCommandBuffer {
                found: exp.max_command_bytes + 1,
                capacity: exp.max_command_bytes,
            })
        );

        let mut h = over;
        h.use_count = HELIOS_HOB1_MAX_USE_RECORDS + 1;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::UseCountAboveLimit {
                found: HELIOS_HOB1_MAX_USE_RECORDS + 1,
                limit: HELIOS_HOB1_MAX_USE_RECORDS,
            })
        );

        let mut h = over;
        h.operand_count = HELIOS_HOB1_MAX_OPERAND_RECORDS + 1;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::OperandCountAboveLimit {
                found: HELIOS_HOB1_MAX_OPERAND_RECORDS + 1,
                limit: HELIOS_HOB1_MAX_OPERAND_RECORDS,
            })
        );

        let mut h = over;
        h.payload_bytes = 0;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::PayloadBytesZero)
        );

        // A batch ID must strictly increase on its own context.
        let mut h = over;
        h.batch_id = exp.last_batch_id;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::BatchIdNotIncreasing {
                found: exp.last_batch_id,
                last: exp.last_batch_id,
            })
        );

        // Neither arm, or the wrong arm for this context.
        let mut h = over;
        h.flags = 3;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::Flags {
                found: 3,
                expected: HELIOS_HOB1_FLAG_D3D11_PHYSICAL,
            })
        );
        let mut h = over;
        h.flags = HELIOS_HOB1_FLAG_D3D12_VIRTUAL;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::Flags {
                found: HELIOS_HOB1_FLAG_D3D12_VIRTUAL,
                expected: HELIOS_HOB1_FLAG_D3D11_PHYSICAL,
            })
        );
    }

    #[test]
    fn hob1_refuses_misaligned_overlapping_or_escaping_regions() {
        let exp = expectation(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);
        let base = header(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);

        let mut h = base;
        h.use_offset = 116;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::UseTableMisaligned { offset: 116 })
        );

        let mut h = base;
        h.use_offset = 8;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::UseTableBeforeHeaderEnd { offset: 8 })
        );

        // Tables that overlap each other — at their common start …
        let mut h = base;
        h.operand_offset = USE_OFF as u32;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::UseAndOperandTablesOverlap)
        );
        // … and part-way in.
        let mut h = base;
        h.operand_offset = 120;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::UseAndOperandTablesOverlap)
        );

        // Payload before the tables end.
        let mut h = base;
        h.payload_offset = USE_OFF as u32;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::PayloadNotAfterTables {
                payload_offset: USE_OFF as u32
            })
        );

        // A region that leaves the record.
        let mut h = base;
        h.payload_bytes = REC_BYTES as u32;
        assert!(matches!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::PayloadOutsideRecord { .. })
        ));

        // A nonzero offset with a zero count, and the converse.
        let mut h = base;
        h.use_count = 0;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::TableOffsetCountMismatch {
                offset: USE_OFF as u32,
                count: 0
            })
        );
    }

    /// §17.1's "D3D11-type-1 versus D3D12-type-2 validator": the batch's arm
    /// decides, and a record can never re-select it.
    #[test]
    fn use_record_identity_is_decided_by_the_batch_arm() {
        let idx = use_record(HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, 5);
        assert_eq!(
            idx.validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST),
            Ok(HeliosUseIdentity::D3D11AllocationListIndex(5))
        );
        assert_eq!(
            idx.validate(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, 1, 0),
            Err(HeliosUseRecordRejection::IdentityKindWrongForBatch {
                batch_flags: HELIOS_HOB1_FLAG_D3D12_VIRTUAL,
                identity_kind: HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX,
            })
        );

        let gpuva = use_record(HELIOS_HOB1_IDENTITY_D3D12_GPUVA, 0x1_0000_0000);
        assert_eq!(
            gpuva.validate(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, 1, 0),
            Ok(HeliosUseIdentity::D3D12GpuVirtualAddress(0x1_0000_0000))
        );

        // Type 1 must have the upper 32 address bits zero.
        let wide = use_record(HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, 1 << 32);
        assert_eq!(
            wide.validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST),
            Err(HeliosUseRecordRejection::D3D11IndexUpperBitsSet { found: 1 << 32 })
        );

        // Type 2 may not be null.
        let null = use_record(HELIOS_HOB1_IDENTITY_D3D12_GPUVA, 0);
        assert_eq!(
            null.validate(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, 1, 0),
            Err(HeliosUseRecordRejection::D3D12GpuVaZero)
        );

        // An unknown kind is never adopted.
        let unknown = use_record(3, 5);
        assert_eq!(
            unknown.validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST),
            Err(HeliosUseRecordRejection::UnknownIdentityKind { found: 3 })
        );
    }

    #[test]
    fn use_record_access_bits_are_closed_and_primary_implies_write() {
        let mut u = use_record(HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, 5);
        u.access_flags = HELIOS_HOB1_ACCESS_READ | HELIOS_HOB1_ACCESS_PRIMARY_WRITE;
        assert_eq!(
            u.validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST),
            Err(HeliosUseRecordRejection::PrimaryWriteWithoutWrite)
        );

        u.access_flags = HELIOS_HOB1_ACCESS_MASK;
        assert!(u
            .validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST)
            .is_ok());

        u.access_flags = 0;
        assert_eq!(
            u.validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST),
            Err(HeliosUseRecordRejection::AccessFlagsZero)
        );

        u.access_flags = 1 << 3;
        assert_eq!(
            u.validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST),
            Err(HeliosUseRecordRejection::UnknownAccessBits { found: 1 << 3 })
        );

        // Operand slice must stay inside the batch's operand table.
        let mut u = use_record(HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, 5);
        u.first_operand = 1;
        assert_eq!(
            u.validate(HELIOS_HOB1_FLAG_D3D11_PHYSICAL, 1, ALLOC_LIST),
            Err(HeliosUseRecordRejection::OperandRangeOutsideTable {
                first_operand: 1,
                operand_count: 1,
                batch_operand_count: 1,
            })
        );
    }

    #[test]
    fn typed_operands_must_name_a_generated_resource_inside_the_payload() {
        let h = header(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);
        assert_eq!(operand_record().validate(&h), Ok(()));

        let mut o = operand_record();
        o.operand_kind = HELIOS_HOB1_OPERAND_KIND_MAX + 1;
        assert_eq!(
            o.validate(&h),
            Err(HeliosOperandRecordRejection::UnknownOperandKind {
                found: HELIOS_HOB1_OPERAND_KIND_MAX + 1
            })
        );

        let mut o = operand_record();
        o.encoded_width = 2;
        assert_eq!(
            o.validate(&h),
            Err(HeliosOperandRecordRejection::UnsupportedEncodedWidth { found: 2 })
        );

        // An operand may not name a byte of the header or of either table.
        let mut o = operand_record();
        o.payload_offset = USE_OFF as u32;
        assert_eq!(
            o.validate(&h),
            Err(HeliosOperandRecordRejection::OutsidePayloadRegion {
                payload_offset: USE_OFF as u32,
                width: HELIOS_HOB1_OPERAND_WIDTH_8,
            })
        );

        // Nor run past the payload's end.
        let mut o = operand_record();
        o.payload_offset = (PAYLOAD_OFF + PAYLOAD_LEN - 4) as u32;
        assert_eq!(
            o.validate(&h),
            Err(HeliosOperandRecordRejection::OutsidePayloadRegion {
                payload_offset: (PAYLOAD_OFF + PAYLOAD_LEN - 4) as u32,
                width: HELIOS_HOB1_OPERAND_WIDTH_8,
            })
        );

        let mut o = operand_record();
        o.use_index = 1;
        assert_eq!(
            o.validate(&h),
            Err(HeliosOperandRecordRejection::UseIndexOutsideTable {
                found: 1,
                use_count: 1
            })
        );

        // The Venus payload is encoded in 4-byte units, so an operand at an odd
        // offset is the "arbitrary byte patching" §10.4 rejects.
        let mut o = operand_record();
        o.payload_offset = PAYLOAD_OFF as u32 + 1;
        assert_eq!(
            o.validate(&h),
            Err(HeliosOperandRecordRejection::OffsetMisaligned {
                payload_offset: PAYLOAD_OFF as u32 + 1,
                alignment: HELIOS_HOB1_OPERAND_ALIGN,
            })
        );
    }

    /// The two-use / two-operand fixture the whole-table rules need. Layout:
    /// 112 header, 2×40 use, 2×16 operand, 64 payload = 288 bytes.
    #[repr(C, align(8))]
    struct WideRecord([u8; 288]);

    const WIDE_USE_OFF: usize = 112;
    const WIDE_OPERAND_OFF: usize = 192;
    const WIDE_PAYLOAD_OFF: usize = 224;

    /// Assemble a CRC-sealed two-use record; `mutate` runs on the whole set
    /// before sealing so a test can break exactly one whole-table property.
    fn build_wide(
        mutate: impl FnOnce(&mut [HeliosOuterBatchUseV1; 2], &mut [HeliosOuterBatchOperandV1; 2]),
    ) -> WideRecord {
        let mut h = header(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);
        h.total_bytes = 288;
        h.use_offset = WIDE_USE_OFF as u32;
        h.use_count = 2;
        h.operand_offset = WIDE_OPERAND_OFF as u32;
        h.operand_count = 2;
        h.payload_offset = WIDE_PAYLOAD_OFF as u32;
        h.payload_bytes = 64;

        let mut uses = [
            use_record(HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, 5),
            use_record(HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, 6),
        ];
        uses[0].first_operand = 0;
        uses[0].operand_count = 1;
        uses[1].first_operand = 1;
        uses[1].operand_count = 1;

        let mut operands = [operand_record(), operand_record()];
        operands[0].payload_offset = WIDE_PAYLOAD_OFF as u32;
        operands[0].use_index = 0;
        operands[1].payload_offset = WIDE_PAYLOAD_OFF as u32 + 8;
        operands[1].use_index = 1;

        mutate(&mut uses, &mut operands);

        let mut rec = WideRecord([0u8; 288]);
        rec.0[WIDE_USE_OFF..WIDE_USE_OFF + 80].copy_from_slice(bytemuck::cast_slice(&uses));
        rec.0[WIDE_OPERAND_OFF..WIDE_OPERAND_OFF + 32]
            .copy_from_slice(bytemuck::cast_slice(&operands));
        rec.0[..112].copy_from_slice(bytemuck::bytes_of(&h));
        let crc = hob1_record_crc64(&rec.0).expect("record covers the header");
        h.crc64 = crc;
        rec.0[..112].copy_from_slice(bytemuck::bytes_of(&h));
        rec
    }

    /// §10.4's three whole-table properties, each of which is silent if only the
    /// per-record validators run.
    #[test]
    fn hob1_enforces_the_whole_table_properties() {
        let exp = expectation(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);
        assert_eq!(validate_batch_record(&build_wide(|_, _| {}).0, &exp), Ok(()));

        // A type-1 index past the runtime's allocation list — the kernel OOB
        // read this bound exists to stop.
        let rec = build_wide(|uses, _| uses[1].address_or_index = ALLOC_LIST as u64);
        assert_eq!(
            validate_batch_record(&rec.0, &exp),
            Err(HeliosOuterBatchRejection::UseRecord {
                index: 1,
                reason: HeliosUseRecordRejection::D3D11IndexOutsideAllocationList {
                    found: ALLOC_LIST,
                    allocation_list_count: ALLOC_LIST,
                },
            })
        );
        let rec = build_wide(|uses, _| uses[1].address_or_index = 0xFFFF_FFFF);
        assert!(matches!(
            validate_batch_record(&rec.0, &exp),
            Err(HeliosOuterBatchRejection::UseRecord {
                index: 1,
                reason: HeliosUseRecordRejection::D3D11IndexOutsideAllocationList { .. },
            })
        ));

        // "the complete unique-allocation closure": one allocation, two records
        // with contradictory access.
        let rec = build_wide(|uses, _| {
            uses[1].address_or_index = 5;
            uses[1].access_flags = HELIOS_HOB1_ACCESS_READ;
        });
        assert_eq!(
            validate_batch_record(&rec.0, &exp),
            Err(HeliosOuterBatchRejection::AllocationUsedTwice {
                allocation_index: 5,
                use_index: 1,
            })
        );

        // The redundant pair must agree in both directions.
        let rec = build_wide(|_, operands| operands[1].use_index = 0);
        assert_eq!(
            validate_batch_record(&rec.0, &exp),
            Err(HeliosOuterBatchRejection::OperandRecord {
                index: 1,
                reason: HeliosOperandRecordRejection::UseIndexNotOwningUse {
                    found: 0,
                    owning_use: 1,
                },
            })
        );

        // A nonzero placeholder is how a raw host resource/renderer ID would
        // reach the host at a blessed position.
        let rec = build_wide(|_, _| {});
        let mut poisoned = WideRecord(rec.0);
        poisoned.0[WIDE_PAYLOAD_OFF + 1] = 0xAB;
        // Re-seal so the CRC is not what refuses it.
        let mut h = *hob1_header(&poisoned.0).unwrap();
        h.crc64 = 0;
        poisoned.0[..112].copy_from_slice(bytemuck::bytes_of(&h));
        h.crc64 = hob1_record_crc64(&poisoned.0).unwrap();
        poisoned.0[..112].copy_from_slice(bytemuck::bytes_of(&h));
        assert_eq!(
            validate_batch_record(&poisoned.0, &exp),
            Err(HeliosOuterBatchRejection::OperandRecord {
                index: 0,
                reason: HeliosOperandRecordRejection::PayloadPlaceholderNonZero {
                    payload_offset: WIDE_PAYLOAD_OFF as u32,
                    width: HELIOS_HOB1_OPERAND_WIDTH_8,
                },
            })
        );

        // Runs must tile the operand table: no reordering …
        let rec = build_wide(|uses, operands| {
            uses[0].first_operand = 1;
            uses[1].first_operand = 0;
            operands[0].use_index = 1;
            operands[1].use_index = 0;
        });
        assert_eq!(
            validate_batch_record(&rec.0, &exp),
            Err(HeliosOuterBatchRejection::OperandRunNotContiguous {
                use_index: 0,
                expected_first_operand: 0,
                found_first_operand: 1,
            })
        );
        // … and no operand owned by nobody. A use may legally own no operand
        // (`first_operand` is then zero), but the operands it abandons must not
        // still be in the table.
        let rec = build_wide(|uses, _| {
            uses[1].first_operand = 0;
            uses[1].operand_count = 0;
        });
        assert_eq!(
            validate_batch_record(&rec.0, &exp),
            Err(HeliosOuterBatchRejection::OperandRunLeavesGap {
                covered: 1,
                operand_count: 2,
            })
        );

        // A D3D12 context has no allocation list, so a nonzero bound is a KMD
        // bug, not a wire error, and is named as such.
        let mut virtual_exp = expectation(HELIOS_HOB1_FLAG_D3D12_VIRTUAL);
        virtual_exp.allocation_list_count = 1;
        let d3d12 = build(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, |_| {});
        assert_eq!(
            validate_batch_record(&d3d12.0, &virtual_exp),
            Err(HeliosOuterBatchRejection::AllocationListOnVirtualArm { found: 1 })
        );
    }

    /// "total bytes | header through payload" — exactly, so no undescribed byte
    /// rides inside a sealed record.
    #[test]
    fn hob1_refuses_bytes_no_table_describes() {
        let exp = expectation(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);
        let mut h = header(HELIOS_HOB1_FLAG_D3D11_PHYSICAL);
        h.payload_bytes = PAYLOAD_LEN as u32 - 8;
        assert_eq!(
            h.validate(&exp),
            Err(HeliosOuterBatchRejection::RecordHasTrailingBytes {
                payload_end: (PAYLOAD_OFF + PAYLOAD_LEN - 8) as u64,
                total_bytes: REC_BYTES as u64,
            })
        );
    }

    /// `protocol/include/helios_wddm.h` hand-copies every constant below. Pin
    /// the exact literals here so a change on the Rust side without the matching
    /// header edit is caught by a failing test that names the header, not by a
    /// live VM mis-resolving a GPUVA. (Offsets and sizes need no test: both
    /// sides assert them at compile time.)
    #[test]
    fn c_mirror_carries_these_exact_constants() {
        // protocol/include/helios_wddm.h
        assert_eq!(HELIOS_HWA2_MAGIC, 0x3241_5748);
        assert_eq!(HELIOS_HWA2_ABI_VERSION, 2);
        assert_eq!(HELIOS_HWA2_BYTES, 168);
        assert_eq!(HELIOS_HWA2_FLAG_MASK, 0x0000_07FF);
        assert_eq!(HELIOS_HWA2_FLAG_KMD_OWNED_MASK, 0x0000_0030);
        assert_eq!(HELIOS_HWA2_BIND_MASK, 0x0000_07FF);
        assert_eq!(HELIOS_HWA2_MISC_MASK, 0x0000_000F);
        assert_eq!(HELIOS_HWA2_MAX_PLANES, 4);
        assert_eq!(D3DDDI_ID_UNINITIALIZED, 0xFFFF_FFFF);

        assert_eq!(HELIOS_HOB1_MAGIC, 0x3142_4F48);
        assert_eq!(HELIOS_HOB1_ABI_VERSION, 1);
        assert_eq!(HELIOS_HOB1_HEADER_BYTES, 112);
        assert_eq!(HELIOS_HOB1_CRC_FIELD_OFFSET, 80);
        assert_eq!(HELIOS_HOB1_USE_RECORD_BYTES, 40);
        assert_eq!(HELIOS_HOB1_OPERAND_RECORD_BYTES, 16);
        assert_eq!(HELIOS_HOB1_MAX_USE_RECORDS, 4096);
        assert_eq!(HELIOS_HOB1_MAX_OPERAND_RECORDS, 8192);
        assert_eq!(HELIOS_HOB1_MAX_BYTES, 15_728_640);
        assert_eq!(HELIOS_HOB1_OFFSET_ALIGNMENT, 8);
        assert_eq!(HELIOS_HOB1_OPERAND_ALIGN, 4);
        assert_eq!(HELIOS_HOB1_ACCESS_MASK, 7);
        assert_eq!(HELIOS_CRC64_ECMA182_POLY, 0x42F0_E1EB_A9EA_3693);
        assert_eq!(HELIOS_CRC64_ECMA182_CHECK, 0x6C40_DF5F_0B49_7347);

        assert_eq!(HELIOS_HOS1_MAGIC, 0x3153_4F48);
        assert_eq!(HELIOS_HOS1_ABI_VERSION, 1);
        assert_eq!(HELIOS_HOS1_BYTES, 64);

        assert_eq!(HELIOS_HOC1_MAGIC, 0x3143_4F48);
        assert_eq!(HELIOS_HOC1_ABI_VERSION, 1);
        assert_eq!(HELIOS_HOC1_BYTES, 64);
        assert_eq!(HELIOS_HOC1_POOL_BYTES, 67_108_864);
        assert_eq!(HELIOS_HOC1_EXTENT_ALIGNMENT, 65_536);
        assert_eq!(HELIOS_HOC1_MAX_LIVE_EXTENTS, 256);
        assert_eq!(HELIOS_HOC1_ACCESS_REQUIRED, 3);
        assert_eq!(HELIOS_HOC1_CACHE_WRITE_COMBINED, 1);
        assert_eq!(HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0, 1);
    }

    /// A type-2 range that wraps `u64` must never reach QEMU's page-table walk.
    #[test]
    fn hob1_refuses_a_wrapping_gpu_virtual_range() {
        let mut u = use_record(HELIOS_HOB1_IDENTITY_D3D12_GPUVA, u64::MAX - 8);
        u.byte_length = 4096;
        assert_eq!(
            u.validate(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, 1, 0),
            Err(HeliosUseRecordRejection::D3D12GpuVaRangeOverflow {
                address: u64::MAX - 8,
                byte_length: 4096,
            })
        );
    }

    // ── HOS1 ────────────────────────────────────────────────────────────────

    fn submit(h: &HeliosOuterBatchV1) -> HeliosOuterSubmitV1 {
        HeliosOuterSubmitV1 {
            magic: HELIOS_HOS1_MAGIC,
            abi_version: HELIOS_HOS1_ABI_VERSION,
            struct_size: HELIOS_HOS1_BYTES,
            package_generation: h.package_generation,
            session_generation: h.session_generation,
            context_generation: h.context_generation,
            endpoint_id: h.endpoint_id,
            hob1_bytes: h.total_bytes as u32,
            batch_id: h.batch_id,
            hob1_crc64: h.crc64,
            reserved: 0,
        }
    }

    #[test]
    fn hos1_validates_against_the_context_and_cross_checks_the_batch() {
        let rec = build(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, |_| {});
        let h = *hob1_header(&rec.0).unwrap();
        let exp = expectation(HELIOS_HOB1_FLAG_D3D12_VIRTUAL);
        let s = submit(&h);

        assert_eq!(s.validate(&exp, h.total_bytes), Ok(()));
        assert_eq!(s.cross_check(&h), Ok(()));

        // HOS1 exists only on the D3D12 virtual arm.
        assert_eq!(
            s.validate(&expectation(HELIOS_HOB1_FLAG_D3D11_PHYSICAL), h.total_bytes),
            Err(HeliosOuterSubmitRejection::NotD3D12VirtualContext {
                context_flags: HELIOS_HOB1_FLAG_D3D11_PHYSICAL
            })
        );

        // hob1_bytes must equal CommandLength/DmaBufferSize.
        assert_eq!(
            s.validate(&exp, h.total_bytes + 8),
            Err(HeliosOuterSubmitRejection::Hob1BytesMismatch {
                found: h.total_bytes as u32,
                command_length: h.total_bytes + 8,
            })
        );

        let mut bad = s;
        bad.hob1_crc64 ^= 1;
        assert_eq!(
            bad.cross_check(&h),
            Err(HeliosOuterSubmitRejection::Hob1CrcMismatch {
                hos1: h.crc64 ^ 1,
                hob1: h.crc64,
            })
        );

        let mut bad = s;
        bad.reserved = 1;
        assert_eq!(
            bad.validate(&exp, h.total_bytes),
            Err(HeliosOuterSubmitRejection::ReservedNonZero { found: 1 })
        );
    }

    #[test]
    fn hos1_private_data_must_be_exactly_sixty_four_bytes() {
        assert_eq!(
            HeliosOuterSubmitV1::from_private_data(&[0u8; 63]).err(),
            Some(HeliosOuterSubmitRejection::PrivateDataSize { found: 63 })
        );
        assert_eq!(
            HeliosOuterSubmitV1::from_private_data(&[0u8; 65]).err(),
            Some(HeliosOuterSubmitRejection::PrivateDataSize { found: 65 })
        );

        // Dxgkrnl promises nothing about the alignment of the buffer it copies
        // the UMD prefix into. An exactly-64-byte buffer at an odd address must
        // parse — refusing it would report "wrong private-data size" while
        // naming the correct size, pointing a maintainer at the wrong subsystem.
        let rec = build(HELIOS_HOB1_FLAG_D3D12_VIRTUAL, |_| {});
        let s = submit(hob1_header(&rec.0).unwrap());
        let mut staging = [0u8; 72];
        staging[4..68].copy_from_slice(bytemuck::bytes_of(&s));
        assert_eq!(
            HeliosOuterSubmitV1::from_private_data(&staging[4..68]),
            Ok(s)
        );
    }

    // ── HOC1 and the C65 pool ───────────────────────────────────────────────

    #[test]
    fn hoc1_input_is_fixed_and_the_generation_is_written_back_at_create() {
        let input = HeliosOuterCommandAllocationV1::new(PKG);
        assert_eq!(input.validate_create_input(PKG), Ok(()));
        // Before the write-back, the output form must refuse.
        assert_eq!(
            input.validate_create_output(PKG),
            Err(HeliosOuterCommandAllocRejection::AllocationGenerationZeroOnOutput)
        );

        let mut output = input;
        output.allocation_generation = 0x9;
        assert_eq!(output.validate_create_output(PKG), Ok(()));
        // And a prefilled generation on input is a UMD bug, not an adoption.
        assert_eq!(
            output.validate_create_input(PKG),
            Err(
                HeliosOuterCommandAllocRejection::AllocationGenerationNonZeroOnInput { found: 0x9 }
            )
        );
    }

    #[test]
    fn hoc1_refuses_every_field_the_doc_fixes_exactly() {
        let base = HeliosOuterCommandAllocationV1::new(PKG);

        let mut d = base;
        d.byte_size = HELIOS_HOC1_POOL_BYTES / 2;
        assert_eq!(
            d.validate_create_input(PKG),
            Err(HeliosOuterCommandAllocRejection::ByteSize {
                found: HELIOS_HOC1_POOL_BYTES / 2,
                required: HELIOS_HOC1_POOL_BYTES,
            })
        );

        let mut d = base;
        d.extent_alignment = 4096;
        assert_eq!(
            d.validate_create_input(PKG),
            Err(HeliosOuterCommandAllocRejection::ExtentAlignment {
                found: 4096,
                required: HELIOS_HOC1_EXTENT_ALIGNMENT,
            })
        );

        let mut d = base;
        d.access = HELIOS_HOC1_ACCESS_CPU_WRITE;
        assert_eq!(
            d.validate_create_input(PKG),
            Err(HeliosOuterCommandAllocRejection::Access {
                found: HELIOS_HOC1_ACCESS_CPU_WRITE,
                required: HELIOS_HOC1_ACCESS_REQUIRED,
            })
        );

        let mut d = base;
        d.cache_policy = 0;
        assert_eq!(
            d.validate_create_input(PKG),
            Err(HeliosOuterCommandAllocRejection::CachePolicy {
                found: 0,
                required: HELIOS_HOC1_CACHE_WRITE_COMBINED,
            })
        );

        let mut d = base;
        d.physical_adapter_mask = 3;
        assert_eq!(
            d.validate_create_input(PKG),
            Err(HeliosOuterCommandAllocRejection::PhysicalAdapterMask {
                found: 3,
                required: HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0,
            })
        );

        let mut d = base;
        d.reserved[15] = 1;
        assert_eq!(
            d.validate_create_input(PKG),
            Err(HeliosOuterCommandAllocRejection::ReservedNonZero)
        );
    }

    #[test]
    fn pool_extents_obey_the_c65_geometry() {
        assert_eq!(validate_pool_extent(0, HELIOS_HOB1_MAX_BYTES, 0), Ok(()));
        assert_eq!(
            validate_pool_extent(HELIOS_HOC1_EXTENT_ALIGNMENT as u64, 4096, 255),
            Ok(())
        );

        assert_eq!(
            validate_pool_extent(4096, 4096, 0),
            Err(HeliosExtentRejection::OffsetMisaligned {
                offset: 4096,
                alignment: HELIOS_HOC1_EXTENT_ALIGNMENT,
            })
        );
        assert_eq!(
            validate_pool_extent(0, HELIOS_HOB1_MAX_BYTES + 1, 0),
            Err(HeliosExtentRejection::ByteLengthAboveHob1Limit {
                found: HELIOS_HOB1_MAX_BYTES + 1,
                limit: HELIOS_HOB1_MAX_BYTES,
            })
        );
        assert_eq!(
            validate_pool_extent(HELIOS_HOC1_POOL_BYTES, 65536, 0),
            Err(HeliosExtentRejection::RangeOutsidePool {
                end: HELIOS_HOC1_POOL_BYTES + 65536,
                pool_bytes: HELIOS_HOC1_POOL_BYTES,
            })
        );
        assert_eq!(
            validate_pool_extent(0, 4096, HELIOS_HOC1_MAX_LIVE_EXTENTS),
            Err(HeliosExtentRejection::LiveExtentLimit {
                live: HELIOS_HOC1_MAX_LIVE_EXTENTS,
                limit: HELIOS_HOC1_MAX_LIVE_EXTENTS,
            })
        );
    }

    #[test]
    fn extent_seal_state_walks_the_c65_lifecycle_and_nothing_else() {
        use HeliosExtentSealState as S;

        let mut s = S::Free;
        for next in [S::Reserved, S::Sealed, S::Submitted, S::Retired, S::Free] {
            s = s.advance(next).expect("legal C65 transition");
        }
        assert_eq!(s, S::Free);

        // Only a Reserved extent is CPU-writable; once sealed, the WC drain has
        // already published the bytes.
        assert!(S::Reserved.cpu_writable());
        assert!(!S::Sealed.cpu_writable());
        assert!(!S::Submitted.cpu_writable());

        // Submitted is immutable: no rewrite path back to Sealed.
        assert_eq!(
            S::Submitted.advance(S::Sealed),
            Err(HeliosSealTransitionRejection::IllegalTransition {
                from: S::Submitted,
                to: S::Sealed,
            })
        );
        // A reserved extent cannot be submitted without sealing.
        assert_eq!(
            S::Reserved.advance(S::Submitted),
            Err(HeliosSealTransitionRejection::IllegalTransition {
                from: S::Reserved,
                to: S::Submitted,
            })
        );
        // Poison is reachable from anywhere live and from nothing afterwards.
        assert_eq!(S::Submitted.advance(S::Poisoned), Ok(S::Poisoned));
        assert_eq!(
            S::Poisoned.advance(S::Free),
            Err(HeliosSealTransitionRejection::ExtentPoisoned)
        );
    }

    #[test]
    fn hqc1_retirement_tuple_is_complete_and_strictly_increasing() {
        let t = HeliosExtentRetirementV1 {
            queue_context: 0xDEAD_BEEF,
            context_generation: 0x2222_2222_2222_2222,
            batch_id: 7,
            hqc1_value: 12,
        };
        assert_eq!(t.validate(11), Ok(()));
        assert_eq!(
            t.validate(12),
            Err(HeliosRetirementRejection::Hqc1ValueNotIncreasing {
                found: 12,
                previous: 12
            })
        );

        let mut bad = t;
        bad.queue_context = 0;
        assert_eq!(
            bad.validate(0),
            Err(HeliosRetirementRejection::QueueContextZero)
        );

        let mut bad = t;
        bad.hqc1_value = 0;
        assert_eq!(
            bad.validate(0),
            Err(HeliosRetirementRejection::Hqc1ValueZero)
        );

        // The extent is reusable only at or after its exact recorded value.
        assert!(!t.is_retired_at(11));
        assert!(t.is_retired_at(12));
        assert!(t.is_retired_at(13));
    }
}
