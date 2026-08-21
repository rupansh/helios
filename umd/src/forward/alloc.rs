//! Validated descriptors for the WDDM allocation path, and the one place this
//! driver builds an HWA2 create-input record.
//!
//! Resource creation allocates the exact WDDM object first. The resulting
//! package-owned outer allocation token is then associated with DXVK/Mesa
//! resource creation; no host resource ID, blob identity, or scanout-private
//! descriptor crosses this allocation seam.
use helios_protocol::{
    HeliosWddmAllocationDescV2, HeliosWddmPlaneRecordV2, DXGI_FORMAT_UNKNOWN,
    HELIOS_HWA2_BIND_CONSTANT_BUFFER, HELIOS_HWA2_BIND_DEPTH_STENCIL,
    HELIOS_HWA2_BIND_INDEX_BUFFER, HELIOS_HWA2_BIND_PRESENT, HELIOS_HWA2_BIND_RENDER_TARGET,
    HELIOS_HWA2_BIND_SHADER_RESOURCE, HELIOS_HWA2_BIND_STREAM_OUTPUT,
    HELIOS_HWA2_BIND_UNORDERED_ACCESS, HELIOS_HWA2_BIND_VERTEX_BUFFER,
    HELIOS_HWA2_BIND_VIDEO_DECODER, HELIOS_HWA2_BIND_VIDEO_ENCODER, HELIOS_HWA2_FLAG_CPU_VISIBLE,
    HELIOS_HWA2_FLAG_DISPLAYABLE, HELIOS_HWA2_FLAG_PRIMARY, HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED,
    HELIOS_HWA2_FLAG_SHARED, HELIOS_HWA2_KIND_BUFFER, HELIOS_HWA2_KIND_IMAGE,
    HELIOS_HWA2_MEMORY_CPU_VISIBLE, HELIOS_HWA2_MISC_GDI_COMPATIBLE,
    HELIOS_HWA2_MISC_RESOURCE_CLAMP, HELIOS_HWA2_MISC_TEXTURE_CUBE, HELIOS_HWA2_SWIZZLE_LINEAR,
    HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL, HELIOS_PACKAGE_GENERATION,
};

use super::{note_ddi_refusal, ResourceDimension, DDI_REFUSALS};

// ── the D3D11 DDI bit vocabularies, spelled once ────────────────────────────
//
// ⛔ These are `D3D10DDI_BIND_*` / `D3D10DDI_RESOURCE_MISC_*` values, i.e. the
// word the runtime hands us in `D3D11DDIARG_CREATERESOURCE`. They are NOT the
// HWA2 vocabulary and are NOT the D3D11 API vocabulary, even where the numbers
// coincide. The retired `HeliosWddmAllocMeta::bind_flags` shipped this word
// RAW, and `helios_umd12.dll` shipped a `D3D12DDI_RESOURCE_FLAGS_0003` word
// into the same field — two different vocabularies at overlapping bit
// positions, which no reader can tell apart. HWA2's bind bits are deliberately
// non-coincident with both (`protocol/src/wddm.rs`, offset 72), so the
// translation below is mandatory and an untranslated word now trips the
// reserved-bit reject instead of looking plausible.

const DDI_BIND_VERTEX_BUFFER: u32 = 0x0000_0001;
const DDI_BIND_INDEX_BUFFER: u32 = 0x0000_0002;
const DDI_BIND_CONSTANT_BUFFER: u32 = 0x0000_0004;
const DDI_BIND_SHADER_RESOURCE: u32 = 0x0000_0008;
const DDI_BIND_STREAM_OUTPUT: u32 = 0x0000_0010;
const DDI_BIND_RENDER_TARGET: u32 = 0x0000_0020;
const DDI_BIND_DEPTH_STENCIL: u32 = 0x0000_0040;
const DDI_BIND_PRESENT: u32 = 0x0000_0080;
const DDI_BIND_UNORDERED_ACCESS: u32 = 0x0000_0100;
const DDI_BIND_DECODER: u32 = 0x0000_0200;
const DDI_BIND_VIDEO_ENCODER: u32 = 0x0000_0400;

const DDI_MISC_SHARED: u32 = 0x0000_0002;
const DDI_MISC_RESOURCE_CLAMP: u32 = 0x0000_0080;
const DDI_MISC_SHARED_KEYEDMUTEX: u32 = 0x0000_0100;
const DDI_MISC_GDI_COMPATIBLE: u32 = 0x0000_0200;

/// Every DDI bind bit that HAS an HWA2 counterpart. Anything outside this mask
/// is dropped and counted, never reinterpreted.
const DDI_BIND_TRANSLATABLE: u32 = DDI_BIND_VERTEX_BUFFER
    | DDI_BIND_INDEX_BUFFER
    | DDI_BIND_CONSTANT_BUFFER
    | DDI_BIND_SHADER_RESOURCE
    | DDI_BIND_STREAM_OUTPUT
    | DDI_BIND_RENDER_TARGET
    | DDI_BIND_DEPTH_STENCIL
    | DDI_BIND_PRESENT
    | DDI_BIND_UNORDERED_ACCESS
    | DDI_BIND_DECODER
    | DDI_BIND_VIDEO_ENCODER;

/// Every DDI misc bit that HAS an HWA2 counterpart, INCLUDING the two that map
/// to an HWA2 *flag* rather than a misc bit (`SHARED`, `SHARED_KEYEDMUTEX` →
/// [`HELIOS_HWA2_FLAG_SHARED`]) — they are translated, so they are not drops.
const DDI_MISC_TRANSLATABLE: u32 = DDI_MISC_SHARED
    | DDI_MISC_SHARED_KEYEDMUTEX
    | DDI_MISC_RESOURCE_CLAMP
    | DDI_MISC_GDI_COMPATIBLE;

/// Translate a D3D11 DDI bind word into the HWA2 bind vocabulary.
///
/// Bits with no HWA2 counterpart (`D3D11DDI_BIND_CAPTURE`, 0x800, and anything
/// the runtime adds later) are DROPPED and counted — a downgrade, in the same
/// class as `alloc_meta_format_unknown`, not a refusal: HWA2's bind word is
/// descriptive metadata for an opener, and the capture bind has no consumer in
/// this stack at all. It is counted so "we lost a bind bit" can never become
/// invisible.
fn hwa2_bind_flags(ddi_bind: u32) -> u32 {
    let dropped = ddi_bind & !DDI_BIND_TRANSLATABLE;
    if dropped != 0 {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_bind_bits_dropped);
    }

    let mut out = 0;
    for (ddi_bit, hwa2_bit) in [
        (DDI_BIND_VERTEX_BUFFER, HELIOS_HWA2_BIND_VERTEX_BUFFER),
        (DDI_BIND_INDEX_BUFFER, HELIOS_HWA2_BIND_INDEX_BUFFER),
        (DDI_BIND_CONSTANT_BUFFER, HELIOS_HWA2_BIND_CONSTANT_BUFFER),
        (DDI_BIND_SHADER_RESOURCE, HELIOS_HWA2_BIND_SHADER_RESOURCE),
        (DDI_BIND_STREAM_OUTPUT, HELIOS_HWA2_BIND_STREAM_OUTPUT),
        (DDI_BIND_RENDER_TARGET, HELIOS_HWA2_BIND_RENDER_TARGET),
        (DDI_BIND_DEPTH_STENCIL, HELIOS_HWA2_BIND_DEPTH_STENCIL),
        (DDI_BIND_PRESENT, HELIOS_HWA2_BIND_PRESENT),
        (DDI_BIND_UNORDERED_ACCESS, HELIOS_HWA2_BIND_UNORDERED_ACCESS),
        (DDI_BIND_DECODER, HELIOS_HWA2_BIND_VIDEO_DECODER),
        (DDI_BIND_VIDEO_ENCODER, HELIOS_HWA2_BIND_VIDEO_ENCODER),
    ] {
        if ddi_bind & ddi_bit != 0 {
            out |= hwa2_bit;
        }
    }
    out
}

/// Translate a D3D11 DDI misc word into the HWA2 misc vocabulary.
///
/// ⚠ HWA2 offset 76 has exactly four bits. `AUTO_GEN_MIP_MAP`,
/// `DRAWINDIRECT_ARGS`, `BUFFER_ALLOW_RAW_VIEWS`, `BUFFER_STRUCTURED`, `TILED`
/// and `TILE_POOL` have **no counterpart** and are dropped and counted. The
/// retired trailer carried `a.MiscFlags` raw, so those bits used to survive to
/// an opener; they do not any more. That is a deliberate narrowing (§10.3: the
/// misc word is a "versioned protocol vocabulary", not a D3D11 passthrough),
/// and it is countable rather than silent.
///
/// `HELIOS_HWA2_MISC_SHARED_NT_HANDLE` is never set: the D3D11 DDI does not
/// distinguish an NT-handle share at this layer — `api_misc_flags` *synthesises*
/// `D3D11_RESOURCE_MISC_SHARED_NTHANDLE` for DXVK from the plain DDI `SHARED`
/// bit — so setting it here would be an inference, and an opener may not infer.
fn hwa2_misc_flags(ddi_misc: u32, texture_cube: bool) -> u32 {
    let dropped = ddi_misc & !DDI_MISC_TRANSLATABLE;
    if dropped != 0 {
        note_ddi_refusal(&DDI_REFUSALS.hwa2_misc_bits_dropped);
    }

    let mut out = 0;
    if ddi_misc & DDI_MISC_GDI_COMPATIBLE != 0 {
        out |= HELIOS_HWA2_MISC_GDI_COMPATIBLE;
    }
    if ddi_misc & DDI_MISC_RESOURCE_CLAMP != 0 {
        out |= HELIOS_HWA2_MISC_RESOURCE_CLAMP;
    }
    if texture_cube {
        out |= HELIOS_HWA2_MISC_TEXTURE_CUBE;
    }
    out
}

/// Everything the D3D11 create path knows that HWA2 offsets 24–167 need.
///
/// A struct rather than eighteen positional arguments for the reason R806 gives
/// above: half of these are `u32`s that would swap silently, and the KMD
/// refuses rather than corrects, so a swapped pair is a failed create with no
/// hint at which pair it was.
pub(crate) struct Hwa2CreateInput {
    /// Selects [`HELIOS_HWA2_KIND_BUFFER`] vs [`HELIOS_HWA2_KIND_IMAGE`].
    pub(crate) dimension: ResourceDimension,
    /// `RES_TEXCUBE` arrived at the DDI (`ResourceDimension` collapses it into
    /// `Texture2D`, so it cannot be recovered from `dimension`).
    pub(crate) texture_cube: bool,
    pub(crate) texel_width: u32,
    pub(crate) texel_height: u32,
    pub(crate) texel_depth: u32,
    pub(crate) array_size: u32,
    pub(crate) mip_levels: u32,
    /// The creator's EXACT `DXGI_FORMAT` (HWA2 offset 48).
    pub(crate) dxgi_format: u32,
    /// The lossy `D3DDDIFORMAT` downgrade (HWA2 offset 52), already counted by
    /// `dxgi_to_d3dddi_format` when it has no spelling.
    pub(crate) d3d_ddi_format: u32,
    pub(crate) sample_count: u32,
    pub(crate) sample_quality: u32,
    /// Raw `D3D11DDIARG_CREATERESOURCE::BindFlags`; translated here.
    pub(crate) ddi_bind_flags: u32,
    /// Raw `D3D11DDIARG_CREATERESOURCE::MiscFlags`; translated here.
    pub(crate) ddi_misc_flags: u32,
    /// The extent the KMD is being asked to back. Preserved verbatim from the
    /// pre-retirement `HeliosWddmAllocPrivate::size` computation.
    pub(crate) byte_size: u64,
    /// Row stride of plane 0.
    pub(crate) row_pitch: u32,
    /// Byte offset of plane 0 inside the extent (nonzero only for a scan-out
    /// primary whose pixels do not start at the blob base).
    pub(crate) plane_offset: u64,
    /// `Some(source)` when `pPrimaryDesc != NULL`; the concrete VidPn source.
    pub(crate) primary_vidpn_source: Option<u32>,
    /// The primary is scanned out directly from this (OPTIMAL) allocation
    /// rather than copied into the KMD-owned LINEAR target.
    pub(crate) direct_scanout_primary: bool,
}

/// Why an HWA2 create-input descriptor could not be built.
///
/// One variant per reason so the caller raises a distinct counter without
/// re-deriving the test, exactly as `HeliosAllocDescRejection` does on the
/// protocol side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hwa2InputRefusal {
    /// An image kind whose `DXGI_FORMAT` is `UNKNOWN`. HWA2 hard-fails this
    /// (`ImageDxgiFormatUnknown`) and there is nothing to substitute: the exact
    /// format is what an opener rebuilds the image from.
    ImageDxgiFormatUnknown,
    /// The extent is zero. Nothing can be bounded against it.
    ByteSizeZero,
    /// Plane 0 cannot be expressed: a zero row pitch, a slice extent that
    /// overflows `u32`/`u64`, or `offset + slice_pitch > byte_size`.
    PlaneUnrepresentable {
        row_pitch: u32,
        height: u32,
        plane_offset: u64,
        byte_size: u64,
    },
}

impl Hwa2CreateInput {
    /// Build the create-input descriptor for `pfnAllocateCb`.
    ///
    /// # The field partition this obeys (`K4-CONTRACT.md` §1.1)
    ///
    /// * header (`magic`, `abi_version`, `struct_size`, `package_generation`):
    ///   written exactly, echoed by the KMD;
    /// * `allocation_generation`: **zero** — the KMD assigns it and
    ///   `validate_create_input` rejects a nonzero one;
    /// * `DIRECT_FLIP_COMPATIBLE` and `D3D12_RUNTIME_PRIMARY`: **zero** — both
    ///   are KMD-owned and an opener never infers either;
    /// * everything else: written exactly, validated and echoed verbatim. The
    ///   KMD refuses the create rather than correcting a field.
    pub(crate) fn build(&self) -> Result<HeliosWddmAllocationDescV2, Hwa2InputRefusal> {
        // The one package-generation constant §17.1 requires — shared by
        // protocol, Mesa, both UMDs, KMD, QEMU and the installer
        // (`protocol/src/lib.rs`). ⛔ Do not introduce a second one; the UMD has
        // no negotiated per-adapter value to use instead.
        //
        // `header()` takes the allocation generation as its second argument;
        // ZERO is the create-input value and the KMD stamps the real one.
        let mut desc = HeliosWddmAllocationDescV2::header(HELIOS_PACKAGE_GENERATION, 0);

        if self.byte_size == 0 {
            return Err(Hwa2InputRefusal::ByteSizeZero);
        }
        desc.byte_size = self.byte_size;

        let image = !matches!(self.dimension, ResourceDimension::Buffer);
        desc.allocation_kind = if image {
            HELIOS_HWA2_KIND_IMAGE
        } else {
            HELIOS_HWA2_KIND_BUFFER
        };

        // ── geometry (offsets 32-63) ────────────────────────────────────────
        //
        // HWA2 reads "zero only for a non-image kind" fail-closed in BOTH
        // directions: an image must carry every one of these nonzero, a
        // non-image must carry all of them zero. `header()` already zeroed
        // them, so the non-image arm writes nothing.
        if image {
            // ⚠ `.max(1)` on height/depth/mips/samples is a REPRESENTATION
            // choice, not a fixup of a wrong value: a 1D texture arrives with
            // `TexelHeight == 0` and a non-arrayed resource with
            // `ArraySize == 0`, and HWA2 has no "not applicable" encoding —
            // a height-1, one-layer, one-mip, one-sample image is exactly what
            // those describe. Width is NOT clamped: a zero-width image is
            // nonsense and must fail (it fails below, on the plane record,
            // because its row pitch is zero).
            desc.width = self.texel_width;
            desc.height = self.texel_height.max(1);
            desc.depth_or_array_size = match self.dimension {
                // One field for both, per §10.3. A 3D texture's third extent is
                // its depth; everything else's is its array length.
                ResourceDimension::Texture3D => self.texel_depth.max(1),
                _ => self.array_size.max(1),
            };
            desc.mip_levels = self.mip_levels.max(1);
            if self.dxgi_format == DXGI_FORMAT_UNKNOWN {
                return Err(Hwa2InputRefusal::ImageDxgiFormatUnknown);
            }
            desc.dxgi_format = self.dxgi_format;
            desc.d3d_ddi_format = self.d3d_ddi_format;
            desc.sample_count = self.sample_count.max(1);
            desc.sample_quality = self.sample_quality;
        }

        // ── vocabularies (offsets 68-79) ────────────────────────────────────
        let mut flags = 0;
        if self.primary_vidpn_source.is_some() {
            flags |= HELIOS_HWA2_FLAG_PRIMARY;
        }
        if self.ddi_misc_flags & (DDI_MISC_SHARED | DDI_MISC_SHARED_KEYEDMUTEX) != 0 {
            flags |= HELIOS_HWA2_FLAG_SHARED;
        }
        if self.direct_scanout_primary {
            // The successor of `HELIOS_WDDM_ALLOC_MISC_DIRECT_SCANOUT`: this
            // exact allocation is what `SET_SCANOUT_BLOB` binds. A primary that
            // is copied into the KMD-owned LINEAR target is NOT displayable in
            // this sense and must not claim to be.
            flags |= HELIOS_HWA2_FLAG_DISPLAYABLE;
        }
        // The retired record asked for `VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE` on
        // every KMD-backed allocation, i.e. host-visible memory. That is what
        // the memory class and the flag both record now, and `validate` requires
        // them to agree (`MemoryClassCpuVisibilityMismatch`).
        //
        // ⚠ The venus-backed arm — which asked for `USE_SHAREABLE` on
        // device-local memory and would map to `MEMORY_DEVICE_LOCAL` with the
        // flag clear — cannot reach here: it is refused at the create boundary
        // (see the module doc and `K4-CONTRACT.md` §5).
        flags |= HELIOS_HWA2_FLAG_CPU_VISIBLE;
        // Every `pfnAllocateCb` this driver makes passes the runtime resource
        // handle (`D3DDDICB_ALLOCATE::hResource`), so dxgkrnl always creates a
        // resource object and always sets `DXGK_CREATEALLOCATIONFLAGS::Resource`
        // on the kernel side. Recorded verbatim so no later diagnostic has to
        // infer it from dimensions, process, or order (§10.3, offset 68).
        flags |= HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED;
        //
        // ⛔ NOT set here, deliberately, and each for its own reason:
        //   * DIRECT_FLIP_COMPATIBLE / D3D12_RUNTIME_PRIMARY — KMD-owned
        //     (§1.1); `validate_create_input` rejects either on input.
        //   * STEREO — this driver has never created a stereo primary and the
        //     D3D11 create DDI gives no stereo signal we read; claiming it
        //     would be an inference.
        //   * PROTECTED / CROSS_ADAPTER — no D3D11 DDI create bit in this
        //     driver's vocabulary maps to either, and §3 forbids cross-adapter
        //     inference outright. Both stay zero until a real source exists.
        //   * STANDARD — an OS *standard allocation* is the
        //     `pfnGetStandardAllocationDriverData` path, which is dxgkrnl's and
        //     the KMD's. A `pPrimaryDesc` resource created through
        //     `pfnAllocateCb` with our own private data is not one, so the flag
        //     and `standard_allocation_type` stay zero (HWA2 hard-fails the
        //     flag without the OS enum, and the enum without the flag).
        desc.flags = flags;
        desc.bind_flags = hwa2_bind_flags(self.ddi_bind_flags);
        desc.misc_flags = hwa2_misc_flags(self.ddi_misc_flags, self.texture_cube);

        // ── VidPn source (offset 80) ────────────────────────────────────────
        //
        // A conventional D3D11 primary carries its concrete source; everything
        // else carries the sentinel, which means "any source on this exact
        // adapter" and is never compared as a concrete identity. `header()`
        // already wrote the sentinel.
        if let Some(source) = self.primary_vidpn_source {
            desc.vidpn_source = source;
        }

        // ── layout and memory class (offsets 88-95) ─────────────────────────
        desc.swizzle_class = if self.direct_scanout_primary {
            // The direct arm scans out of the OPTIMAL image DXVK rendered into;
            // its "pitch" is a logical scan-out stride, not a row-major one.
            HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL
        } else {
            HELIOS_HWA2_SWIZZLE_LINEAR
        };
        desc.memory_class = HELIOS_HWA2_MEMORY_CPU_VISIBLE;

        // ── plane records (offsets 96-167) ──────────────────────────────────
        //
        // A buffer has no planes and `header()` already zeroed all four; HWA2
        // hard-fails a non-image with a nonzero plane count.
        if image {
            let height = desc.height;
            if self.row_pitch == 0 {
                return Err(Hwa2InputRefusal::PlaneUnrepresentable {
                    row_pitch: self.row_pitch,
                    height,
                    plane_offset: self.plane_offset,
                    byte_size: self.byte_size,
                });
            }
            // `slice_pitch` is the plane's own extent, bounded against
            // `byte_size` by `offset + slice_pitch` — never `row_pitch *
            // height` as a claim about the whole allocation (§10.3's note on
            // chroma planes). Checked, not saturating: a saturated product
            // would silently understate the extent.
            let Some(slice_pitch) = self.row_pitch.checked_mul(height) else {
                return Err(Hwa2InputRefusal::PlaneUnrepresentable {
                    row_pitch: self.row_pitch,
                    height,
                    plane_offset: self.plane_offset,
                    byte_size: self.byte_size,
                });
            };
            let fits = self
                .plane_offset
                .checked_add(u64::from(slice_pitch))
                .is_some_and(|end| end <= self.byte_size);
            if !fits {
                return Err(Hwa2InputRefusal::PlaneUnrepresentable {
                    row_pitch: self.row_pitch,
                    height,
                    plane_offset: self.plane_offset,
                    byte_size: self.byte_size,
                });
            }
            desc.plane_count = 1;
            desc.planes[0] = HeliosWddmPlaneRecordV2 {
                offset: self.plane_offset,
                row_pitch: self.row_pitch,
                slice_pitch,
            };
        }

        // ⛔ Fields with NO counterpart in HWA2, listed so a reader does not go
        // looking for where they went (`K4-CONTRACT.md` §5, §6):
        //   * `HeliosWddmAllocPrivate::{ctx_id, blob_id, blob_mem, blob_flags,
        //     map_cache, adopt_resource_id}` — venus context id, virtio blob
        //     identity and the host resource id. No successor field; the
        //     replacement is the KMD patching the resid in from
        //     `HeliosNativeRenderPatch` (Mesa unit A3 + K6).
        //   * `HeliosWddmAllocMeta::{venus_alloc_size, memory_type_index}` —
        //     §10.3 is explicit that the Vulkan memory-type index is gone.
        //   * `HELIOS_WDDM_BLOB_FLAG_GLOBAL_VIDMM_TRACKER` and the `map_cache`
        //     overload that carried the global KMT share — the `GlobalVidMmTracker`
        //     mechanism has no successor at all (§6).
        Ok(desc)
    }
}
