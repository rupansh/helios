//! The pure half of the D5b present copy: a legacy BLT-model windowed present
//! hands `DxgkDdiPresent` the app's back buffer AND DWM's redirection surface
//! and expects the KMD to copy one into the other. Nothing else does — the UMD
//! never sees the destination (`hDstResource == 0`) — so the window is black
//! unless the KMD issues a host `vkCmdCopyImage` itself (ROADMAP D5b).
//!
//! This module owns everything that is a function of its arguments: the copy
//! rectangles, the DXVK-identical `VkImageCreateInfo` for the source alias, the
//! Venus command stream, and the 16-byte record that links a Present packet to
//! its prepared copy slot. Wire shapes are transcribed from
//! `icd/mesa/src/virtio/venus-protocol/vn_protocol_driver_{command_buffer,
//! command_pool,queue}.h`; enum values from `vulkan_core.h`.

use crate::{ImageCreateSpec, StreamWriter, IMAGE_TILING_OPTIMAL};
use helios_protocol::{
    HeliosWddmAllocationDescV2, HELIOS_HWA2_BIND_DEPTH_STENCIL, HELIOS_HWA2_BIND_PRESENT,
    HELIOS_HWA2_BIND_RENDER_TARGET, HELIOS_HWA2_BIND_SHADER_RESOURCE,
    HELIOS_HWA2_BIND_UNORDERED_ACCESS, HELIOS_HWA2_FLAG_STANDARD, HELIOS_HWA2_SWIZZLE_LINEAR,
};

/// Prepared-copy slots per adapter: dxgkrnl's present queue is 3 deep.
pub const MAX_SLOTS: usize = 4;
/// `VkImageCopy` records one stream may carry. A window's clip list is rarely
/// above 8; past this the caller copies the whole destination rect instead.
pub const MAX_REGIONS: usize = 32;
/// 600 fixed bytes + 68 per region (see [`encode_copy_stream`]); 32 regions
/// need 2776, so 4 KiB leaves headroom without a second buffer class.
pub const STREAM_BYTES: usize = 4096;

// ── VkCommandTypeEXT (vn_protocol_driver_defines.h) ──────────────────────────
pub const CMD_GET_DEVICE_QUEUE: u32 = 17;
pub const CMD_QUEUE_SUBMIT: u32 = 18;
/// `vkGetDeviceQueue2`: the only entry point whose info struct chains
/// `VkDeviceQueueTimelineInfoMESA` (`vn_encode_VkDeviceQueueInfo2_pnext`);
/// the ICD binds every queue this way (`vn_device.c`).
pub const CMD_GET_DEVICE_QUEUE2: u32 = 155;
pub const CMD_CREATE_COMMAND_POOL: u32 = 85;
pub const CMD_DESTROY_COMMAND_POOL: u32 = 86;
pub const CMD_ALLOCATE_COMMAND_BUFFERS: u32 = 88;
pub const CMD_BEGIN_COMMAND_BUFFER: u32 = 90;
pub const CMD_END_COMMAND_BUFFER: u32 = 91;
pub const CMD_CMD_COPY_IMAGE: u32 = 113;
pub const CMD_CMD_COPY_IMAGE_TO_BUFFER: u32 = 116;
pub const CMD_CMD_PIPELINE_BARRIER: u32 = 126;
pub const CMD_BIND_BUFFER_MEMORY: u32 = 28;
pub const CMD_GET_BUFFER_MEMORY_REQUIREMENTS: u32 = 30;
pub const CMD_CREATE_BUFFER: u32 = 50;
pub const CMD_DESTROY_BUFFER: u32 = 51;

// ── VkStructureType ──────────────────────────────────────────────────────────
pub const ST_SUBMIT_INFO: i32 = 4;
pub const ST_BUFFER_CREATE_INFO: i32 = 12;
pub const ST_MEMORY_BARRIER: i32 = 46;
pub const ST_COMMAND_POOL_CREATE_INFO: i32 = 39;
pub const ST_COMMAND_BUFFER_ALLOCATE_INFO: i32 = 40;
pub const ST_COMMAND_BUFFER_BEGIN_INFO: i32 = 42;
pub const ST_IMAGE_MEMORY_BARRIER: i32 = 45;
pub const ST_DEVICE_QUEUE_INFO_2: i32 = 1000145003;
/// Venus-private (`vn_protocol_driver_defines.h:23`): names the host ring a
/// `VkQueue`'s fences retire on. Ring 0 is the decoder timeline, whose
/// completion is never a GPU-complete fact.
pub const ST_DEVICE_QUEUE_TIMELINE_INFO_MESA: i32 = 1000384005;

// ── enums / bits ─────────────────────────────────────────────────────────────
pub const IMAGE_LAYOUT_GENERAL: i32 = 1;
pub const PIPELINE_STAGE_TRANSFER: u32 = 0x0000_1000;
pub const PIPELINE_STAGE_ALL_COMMANDS: u32 = 0x0001_0000;
pub const ACCESS_TRANSFER_READ: u32 = 0x0000_0800;
pub const ACCESS_TRANSFER_WRITE: u32 = 0x0000_1000;
pub const ACCESS_MEMORY_READ: u32 = 0x0000_8000;
pub const ACCESS_MEMORY_WRITE: u32 = 0x0001_0000;
pub const IMAGE_ASPECT_COLOR: u32 = 0x1;
pub const QUEUE_FAMILY_IGNORED: u32 = u32::MAX;
pub const COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER: u32 = 0x2;
pub const COMMAND_BUFFER_LEVEL_PRIMARY: i32 = 0;
pub const COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT: u32 = 0x1;
pub const IMAGE_CREATE_MUTABLE_FORMAT: u32 = 0x8;
pub const IMAGE_USAGE_TRANSFER_SRC: u32 = 0x01;
pub const IMAGE_USAGE_TRANSFER_DST: u32 = 0x02;
pub const IMAGE_USAGE_SAMPLED: u32 = 0x04;
pub const IMAGE_USAGE_STORAGE: u32 = 0x08;
pub const IMAGE_USAGE_COLOR_ATTACHMENT: u32 = 0x10;
pub const IMAGE_LAYOUT_UNDEFINED: u32 = 0;
pub const BUFFER_USAGE_TRANSFER_SRC: u32 = 0x1;
pub const BUFFER_USAGE_TRANSFER_DST: u32 = 0x2;
/// Every present-copy format is 4 bytes per texel (BGRA/RGBA 8-bit).
pub const TEXEL_BYTES: u32 = 4;

/// The one KMD render queue's host ring. Nonzero so its fences retire on GPU
/// completion, not decoder progress; 1 because the KMD context has exactly one
/// queue. The ICD's per-process contexts number their own rings independently.
pub const PRESENT_COPY_RING_IDX: u32 = 1;

// ── the Present-packet copy reference ───────────────────────────────────────

/// `"HPCR"` little-endian, at [`COPY_REF_OFFSET`] of the Present packet's KMD
/// private data (after the 16-byte `HPDP` header, before the flip record at 32).
pub const COPY_REF_MAGIC: u32 = 0x5243_5048;
pub const COPY_REF_OFFSET: usize = 16;
pub const COPY_REF_BYTES: usize = 16;

/// `[magic][slot][serial][0]`; `serial == 0` is "no copy". Written on EVERY
/// present packet so a recycled private buffer cannot replay a stale slot.
pub fn encode_copy_ref(copy: Option<(u32, u32)>) -> [u8; COPY_REF_BYTES] {
    let mut bytes = [0u8; COPY_REF_BYTES];
    if let Some((slot, serial)) = copy {
        if serial != 0 && (slot as usize) < MAX_SLOTS {
            bytes[0..4].copy_from_slice(&COPY_REF_MAGIC.to_le_bytes());
            bytes[4..8].copy_from_slice(&slot.to_le_bytes());
            bytes[8..12].copy_from_slice(&serial.to_le_bytes());
        }
    }
    bytes
}

/// `Some((slot, serial))` only for an exact record: magic, a slot below
/// [`MAX_SLOTS`], a nonzero serial and a zero reserved word.
pub fn decode_copy_ref(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < COPY_REF_BYTES {
        return None;
    }
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    if word(0) != COPY_REF_MAGIC || word(12) != 0 {
        return None;
    }
    let (slot, serial) = (word(4), word(8));
    if serial == 0 || slot as usize >= MAX_SLOTS {
        return None;
    }
    Some((slot, serial))
}

// ── geometry ────────────────────────────────────────────────────────────────

/// A WDK `RECT`, exclusive right/bottom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CopyRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl CopyRect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    const fn is_empty(self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    const fn width(self) -> i32 {
        self.right - self.left
    }

    const fn height(self) -> i32 {
        self.bottom - self.top
    }

    fn intersect(self, other: Self) -> Self {
        Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }
}

/// One `VkImageCopy` for a 2D single-mip color image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CopyRegion {
    pub src_x: u32,
    pub src_y: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeometryRefusal {
    /// A rect with no area, or a texel extent of zero.
    Empty,
    /// `SrcRect` and `DstRect` differ in size: a stretch, which a copy cannot do.
    Stretch,
    /// More sub-rects than [`MAX_REGIONS`]; the caller retries with none.
    TooManyRects,
    /// A rect lies outside a (small) `i32` range a texel offset can hold.
    OutOfRange,
}

/// The exact copy the present asked for, in texels, after clipping.
pub struct CopyGeometry<'a> {
    pub src_extent: (u32, u32),
    pub dst_extent: (u32, u32),
    pub src_rect: CopyRect,
    pub dst_rect: CopyRect,
    /// `pDstSubRects`, in destination coordinates; empty means "all of
    /// `dst_rect`".
    pub sub_rects: &'a [CopyRect],
}

impl CopyGeometry<'_> {
    /// Fill `out` with the clipped regions and return how many. Every region is
    /// inside both images; an empty result after clipping is `Empty`.
    pub fn regions(&self, out: &mut [CopyRegion; MAX_REGIONS]) -> Result<usize, GeometryRefusal> {
        if self.sub_rects.len() > MAX_REGIONS {
            return Err(GeometryRefusal::TooManyRects);
        }
        let (sw, sh) = self.src_extent;
        let (dw, dh) = self.dst_extent;
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return Err(GeometryRefusal::Empty);
        }
        let src_bounds = extent_rect(sw, sh).ok_or(GeometryRefusal::OutOfRange)?;
        let dst_bounds = extent_rect(dw, dh).ok_or(GeometryRefusal::OutOfRange)?;
        let (src, dst) = (self.src_rect, self.dst_rect);
        if src.is_empty() || dst.is_empty() {
            return Err(GeometryRefusal::Empty);
        }
        if src.width() != dst.width() || src.height() != dst.height() {
            return Err(GeometryRefusal::Stretch);
        }
        // dst texel -> src texel is a pure translation.
        let dx = src.left.checked_sub(dst.left).ok_or(GeometryRefusal::OutOfRange)?;
        let dy = src.top.checked_sub(dst.top).ok_or(GeometryRefusal::OutOfRange)?;

        let whole = [dst];
        let rects: &[CopyRect] = if self.sub_rects.is_empty() {
            &whole
        } else {
            self.sub_rects
        };
        let mut n = 0usize;
        for &r in rects {
            // Clip in destination space, then re-clip the translated rect in
            // source space and map back, so both ends stay inside their image.
            let d = r.intersect(dst).intersect(dst_bounds);
            if d.is_empty() {
                continue;
            }
            let s = CopyRect::new(
                d.left.checked_add(dx).ok_or(GeometryRefusal::OutOfRange)?,
                d.top.checked_add(dy).ok_or(GeometryRefusal::OutOfRange)?,
                d.right.checked_add(dx).ok_or(GeometryRefusal::OutOfRange)?,
                d.bottom.checked_add(dy).ok_or(GeometryRefusal::OutOfRange)?,
            )
            .intersect(src_bounds);
            if s.is_empty() {
                continue;
            }
            let d = CopyRect::new(s.left - dx, s.top - dy, s.right - dx, s.bottom - dy);
            out[n] = CopyRegion {
                src_x: s.left as u32,
                src_y: s.top as u32,
                dst_x: d.left as u32,
                dst_y: d.top as u32,
                width: s.width() as u32,
                height: s.height() as u32,
            };
            n += 1;
        }
        if n == 0 {
            return Err(GeometryRefusal::Empty);
        }
        Ok(n)
    }
}

fn extent_rect(width: u32, height: u32) -> Option<CopyRect> {
    Some(CopyRect::new(
        0,
        0,
        i32::try_from(width).ok()?,
        i32::try_from(height).ok()?,
    ))
}

// ── the source alias image ──────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AliasRefusal {
    /// Not a single-sample, single-mip, single-layer 2D image.
    Shape = 1,
    /// Neither render-target, present nor shader-resource bindable: the one
    /// class DXVK may build LINEAR and host-mapped (a staging surface).
    Tiling = 2,
    /// No `VkFormat` mapping for this `DXGI_FORMAT`.
    Format = 3,
    /// A depth/stencil target; never a present source.
    DepthStencil = 4,
}

/// `VkFormat` for the four BGRA/RGBA 8-bit formats a present source can carry.
pub const fn dxgi_to_vk_format(dxgi: u32) -> Option<u32> {
    match dxgi {
        87 => Some(44), // B8G8R8A8_UNORM
        88 => Some(50), // B8G8R8A8_UNORM_SRGB -> VK_FORMAT_B8G8R8A8_SRGB
        28 => Some(37), // R8G8B8A8_UNORM
        29 => Some(43), // R8G8B8A8_UNORM_SRGB -> VK_FORMAT_R8G8B8A8_SRGB
        _ => None,
    }
}

/// DXVK's DXGI format family (`dxgi_format.cpp`), the `VkImageFormatListCreateInfo`
/// it attaches to every MUTABLE_FORMAT color texture: UNORM then SRGB.
pub const fn dxgi_view_formats(dxgi: u32) -> Option<[u32; 2]> {
    match dxgi {
        87 | 88 => Some([44, 50]),
        28 | 29 => Some([37, 43]),
        _ => None,
    }
}

/// DXVK's `VkImageCreateInfo` for a D3D11 texture with these bind flags
/// (`d3d11_texture.cpp` GetImageUsage): TRANSFER_SRC|DST always, SAMPLED for
/// SRV, COLOR_ATTACHMENT for RTV, STORAGE for UAV; MUTABLE_FORMAT because every
/// 8-bit color family has an sRGB sibling; no external-memory declaration
/// (`HIM1 … ext=0x0`). Measured 2026-09-02: the BLT back buffer logged
/// `fmt=44 tiling=0 usage=0x13 flags=0x8`, which this reproduces from
/// `bind = RENDER_TARGET|PRESENT`.
///
/// Identical parameters bound to the same memory are what make the alias read
/// the ICD's image: the host GPU picks one layout for one description.
pub fn alias_image_spec(desc: &HeliosWddmAllocationDescV2) -> Result<ImageCreateSpec, AliasRefusal> {
    if desc.width == 0
        || desc.height == 0
        || desc.depth_or_array_size != 1
        || desc.mip_levels != 1
        || desc.sample_count != 1
    {
        return Err(AliasRefusal::Shape);
    }
    // ⚠ NOT `swizzle_class`: the UMD declares LINEAR (the WDDM pitch claim
    // for CPU/cross-adapter access) on every ordinary D3D11 texture while the
    // ICD builds an OPTIMAL VkImage (`HIM1 … tiling=0`). Measured 2026-09-02:
    // keying on the swizzle refused all 738 presents of the BLT probe.
    if desc.bind_flags & HELIOS_HWA2_BIND_DEPTH_STENCIL != 0 {
        return Err(AliasRefusal::DepthStencil);
    }
    if desc.bind_flags
        & (HELIOS_HWA2_BIND_RENDER_TARGET | HELIOS_HWA2_BIND_PRESENT | HELIOS_HWA2_BIND_SHADER_RESOURCE)
        == 0
    {
        return Err(AliasRefusal::Tiling);
    }
    let format = dxgi_to_vk_format(desc.dxgi_format).ok_or(AliasRefusal::Format)?;
    let view_formats = dxgi_view_formats(desc.dxgi_format).ok_or(AliasRefusal::Format)?;
    let mut usage = IMAGE_USAGE_TRANSFER_SRC | IMAGE_USAGE_TRANSFER_DST;
    if desc.bind_flags & HELIOS_HWA2_BIND_SHADER_RESOURCE != 0 {
        usage |= IMAGE_USAGE_SAMPLED;
    }
    if desc.bind_flags & HELIOS_HWA2_BIND_RENDER_TARGET != 0 {
        usage |= IMAGE_USAGE_COLOR_ATTACHMENT;
    }
    if desc.bind_flags & HELIOS_HWA2_BIND_UNORDERED_ACCESS != 0 {
        usage |= IMAGE_USAGE_STORAGE;
    }
    Ok(ImageCreateSpec {
        external_handle_type: 0,
        flags: IMAGE_CREATE_MUTABLE_FORMAT,
        format,
        width: desc.width,
        height: desc.height,
        tiling: IMAGE_TILING_OPTIMAL,
        usage,
        initial_layout: IMAGE_LAYOUT_UNDEFINED,
        view_format_count: 2,
        view_formats,
    })
}

/// What the KMD-side alias for a present allocation has to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TargetClass {
    /// A UMD texture: the ICD's `VkImage` is OPTIMAL; alias it with an
    /// identical image ([`alias_image_spec`]).
    OptimalImage,
    /// A KMD standard surface with a real pitched row layout (the legacy
    /// present's redirection target is a `D3DKMDT_STANDARDALLOCATION_
    /// STAGINGSURFACE`). Its bytes ARE the surface, so the copy is
    /// `vkCmdCopyImageToBuffer` at this pitch — an OPTIMAL alias here writes
    /// tiled bytes into linear rows (measured 2026-09-02: the window showed the
    /// app's colours sheared into bands).
    LinearBuffer { pitch: u32, offset: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TargetRefusal {
    /// The plane record does not describe `width × height` 4-byte texels
    /// inside `byte_size`.
    Plane,
}

/// Classify by producer: only a KMD-authored STANDARD allocation's LINEAR claim
/// is a byte layout; the UMD says LINEAR on every ordinary texture too.
pub fn target_class(desc: &HeliosWddmAllocationDescV2) -> Result<TargetClass, TargetRefusal> {
    if desc.flags & HELIOS_HWA2_FLAG_STANDARD == 0 || desc.swizzle_class != HELIOS_HWA2_SWIZZLE_LINEAR
    {
        return Ok(TargetClass::OptimalImage);
    }
    if desc.plane_count == 0 {
        return Err(TargetRefusal::Plane);
    }
    let plane = desc.planes[0];
    let row_bytes = desc.width.checked_mul(TEXEL_BYTES).ok_or(TargetRefusal::Plane)?;
    if plane.row_pitch < row_bytes || plane.row_pitch % TEXEL_BYTES != 0 {
        return Err(TargetRefusal::Plane);
    }
    let rows = u64::from(plane.row_pitch)
        .checked_mul(u64::from(desc.height))
        .ok_or(TargetRefusal::Plane)?;
    let end = plane.offset.checked_add(rows).ok_or(TargetRefusal::Plane)?;
    if end > desc.byte_size {
        return Err(TargetRefusal::Plane);
    }
    Ok(TargetClass::LinearBuffer {
        pitch: plane.row_pitch,
        offset: plane.offset,
    })
}

/// `VK_MEMORY_PROPERTY_LAZILY_ALLOCATED_BIT | VK_MEMORY_PROPERTY_PROTECTED_BIT`.
const MEMORY_PROPERTY_FORBIDDEN: u32 = 0x10 | 0x20;

/// The renderer memory type the ICD imports every outer allocation at
/// (`vn_physical_device.c`: the first DEVICE_LOCAL type that is neither lazily
/// allocated nor protected, in the renderer's declared order). The alias must
/// import at the same type: the host accepts this sysmem-backed dma-buf into
/// no other (`VK_ERROR_INVALID_EXTERNAL_HANDLE` at the creator's host-visible
/// type and at type 0, 2026-09-02; the ICD's `HAM2 … renderer_type=1` works).
pub fn choose_renderer_device_local_memory_type(
    memory_type_flags: &[u32],
    memory_type_count: u32,
) -> Option<u32> {
    let mut i = 0u32;
    while (i as usize) < memory_type_flags.len() && i < memory_type_count && i < 32 {
        let flags = memory_type_flags[i as usize];
        if flags & crate::MEMORY_PROPERTY_DEVICE_LOCAL != 0 && flags & MEMORY_PROPERTY_FORBIDDEN == 0 {
            return Some(i);
        }
        i += 1;
    }
    None
}

// ── the copy stream ─────────────────────────────────────────────────────────

/// Guest-assigned Venus object ids the stream names.
#[derive(Clone, Copy)]
pub struct CopyIds {
    pub queue: u64,
    pub command_buffer: u64,
    pub src_image: u64,
    pub dst_image: u64,
}

/// One Present's copy as a Venus command stream for the KMD context:
/// `vkBeginCommandBuffer` → barrier → `vkCmdCopyImage` → barrier →
/// `vkEndCommandBuffer` → `vkQueueSubmit`, no reply on any of them.
///
/// Both images are named in `VK_IMAGE_LAYOUT_GENERAL` with no transition: the
/// ICD and DWM own the images' real layouts in their own contexts, and a
/// transition from a guessed `oldLayout` would be the discard defect D6 chased.
/// On the admitted host these images carry no compression metadata (the
/// MUTABLE_FORMAT-without-format-list shape DXVK uses for every Helios image),
/// so GENERAL reads the same bytes every other layout does — the same reliance
/// DWM's cross-process open of a flip buffer already makes.
///
/// The submission's virtio fence rides [`PRESENT_COPY_RING_IDX`], so it
/// signals on GPU completion of this exact `vkQueueSubmit`.
pub fn encode_copy_stream(ids: CopyIds, regions: &[CopyRegion]) -> StreamWriter<STREAM_BYTES> {
    let mut w = StreamWriter::<STREAM_BYTES>::new();

    // vkBeginCommandBuffer(cb, {ONE_TIME_SUBMIT, pInheritanceInfo: NULL})
    w.header(CMD_BEGIN_COMMAND_BUFFER, 0);
    w.handle(ids.command_buffer);
    w.count(true); // pBeginInfo
    w.i32(ST_COMMAND_BUFFER_BEGIN_INFO);
    w.count(false); // pNext
    w.u32(COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT);
    w.count(false); // pInheritanceInfo

    // Everything before us → transfer.
    barrier(
        &mut w,
        ids,
        PIPELINE_STAGE_ALL_COMMANDS,
        PIPELINE_STAGE_TRANSFER,
        [
            (ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE, ACCESS_TRANSFER_READ),
            (ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE, ACCESS_TRANSFER_WRITE),
        ],
    );

    // vkCmdCopyImage(cb, src, GENERAL, dst, GENERAL, n, regions)
    w.header(CMD_CMD_COPY_IMAGE, 0);
    w.handle(ids.command_buffer);
    w.handle(ids.src_image);
    w.i32(IMAGE_LAYOUT_GENERAL);
    w.handle(ids.dst_image);
    w.i32(IMAGE_LAYOUT_GENERAL);
    w.u32(regions.len() as u32);
    w.u64(regions.len() as u64); // array_size
    for r in regions {
        subresource_layers(&mut w);
        w.i32(r.src_x as i32);
        w.i32(r.src_y as i32);
        w.i32(0);
        subresource_layers(&mut w);
        w.i32(r.dst_x as i32);
        w.i32(r.dst_y as i32);
        w.i32(0);
        w.u32(r.width);
        w.u32(r.height);
        w.u32(1);
    }

    // Transfer → everything after us (DWM samples dst; the app redraws src).
    barrier(
        &mut w,
        ids,
        PIPELINE_STAGE_TRANSFER,
        PIPELINE_STAGE_ALL_COMMANDS,
        [
            (ACCESS_TRANSFER_READ, ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE),
            (ACCESS_TRANSFER_WRITE, ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE),
        ],
    );

    // vkEndCommandBuffer(cb)
    w.header(CMD_END_COMMAND_BUFFER, 0);
    w.handle(ids.command_buffer);
    queue_submit(&mut w, ids.queue, ids.command_buffer);
    w
}

/// Ids for a copy into a pitched linear surface.
#[derive(Clone, Copy)]
pub struct CopyToBufferIds {
    pub queue: u64,
    pub command_buffer: u64,
    pub src_image: u64,
    pub dst_buffer: u64,
}

/// [`encode_copy_stream`] for a [`TargetClass::LinearBuffer`] destination:
/// `vkCmdCopyImageToBuffer` with `bufferRowLength = pitch / 4`, the buffer
/// ordered by a global `VkMemoryBarrier` on each side.
pub fn encode_copy_to_buffer_stream(
    ids: CopyToBufferIds,
    regions: &[CopyRegion],
    pitch: u32,
    plane_offset: u64,
) -> StreamWriter<STREAM_BYTES> {
    let mut w = StreamWriter::<STREAM_BYTES>::new();

    w.header(CMD_BEGIN_COMMAND_BUFFER, 0);
    w.handle(ids.command_buffer);
    w.count(true);
    w.i32(ST_COMMAND_BUFFER_BEGIN_INFO);
    w.count(false);
    w.u32(COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT);
    w.count(false);

    buffer_barrier(
        &mut w,
        ids,
        PIPELINE_STAGE_ALL_COMMANDS,
        PIPELINE_STAGE_TRANSFER,
        (ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE, ACCESS_TRANSFER_WRITE),
        (ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE, ACCESS_TRANSFER_READ),
    );

    // vkCmdCopyImageToBuffer(cb, src, GENERAL, dst, n, regions)
    w.header(CMD_CMD_COPY_IMAGE_TO_BUFFER, 0);
    w.handle(ids.command_buffer);
    w.handle(ids.src_image);
    w.i32(IMAGE_LAYOUT_GENERAL);
    w.handle(ids.dst_buffer);
    w.u32(regions.len() as u32);
    w.u64(regions.len() as u64);
    for r in regions {
        let offset = plane_offset
            + u64::from(r.dst_y) * u64::from(pitch)
            + u64::from(r.dst_x) * u64::from(TEXEL_BYTES);
        w.u64(offset); // bufferOffset
        w.u32(pitch / TEXEL_BYTES); // bufferRowLength (texels)
        w.u32(0); // bufferImageHeight: tightly packed rows
        subresource_layers(&mut w);
        w.i32(r.src_x as i32);
        w.i32(r.src_y as i32);
        w.i32(0);
        w.u32(r.width);
        w.u32(r.height);
        w.u32(1);
    }

    buffer_barrier(
        &mut w,
        ids,
        PIPELINE_STAGE_TRANSFER,
        PIPELINE_STAGE_ALL_COMMANDS,
        (ACCESS_TRANSFER_WRITE, ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE),
        (ACCESS_TRANSFER_READ, ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE),
    );

    w.header(CMD_END_COMMAND_BUFFER, 0);
    w.handle(ids.command_buffer);
    queue_submit(&mut w, ids.queue, ids.command_buffer);
    w
}

/// `vkCmdPipelineBarrier` with one global `VkMemoryBarrier` (the buffer) and
/// one full-image GENERAL→GENERAL barrier (the source alias).
fn buffer_barrier(
    w: &mut StreamWriter<STREAM_BYTES>,
    ids: CopyToBufferIds,
    src_stage: u32,
    dst_stage: u32,
    memory_access: (u32, u32),
    image_access: (u32, u32),
) {
    w.header(CMD_CMD_PIPELINE_BARRIER, 0);
    w.handle(ids.command_buffer);
    w.u32(src_stage);
    w.u32(dst_stage);
    w.u32(0); // dependencyFlags
    w.u32(1); // memoryBarrierCount
    w.u64(1); // array_size
    w.i32(ST_MEMORY_BARRIER);
    w.count(false); // pNext
    w.u32(memory_access.0);
    w.u32(memory_access.1);
    w.u32(0); // bufferMemoryBarrierCount
    w.u64(0);
    w.u32(1); // imageMemoryBarrierCount
    w.u64(1);
    w.i32(ST_IMAGE_MEMORY_BARRIER);
    w.count(false);
    w.u32(image_access.0);
    w.u32(image_access.1);
    w.i32(IMAGE_LAYOUT_GENERAL);
    w.i32(IMAGE_LAYOUT_GENERAL);
    w.u32(QUEUE_FAMILY_IGNORED);
    w.u32(QUEUE_FAMILY_IGNORED);
    w.handle(ids.src_image);
    w.u32(IMAGE_ASPECT_COLOR);
    w.u32(0);
    w.u32(1);
    w.u32(0);
    w.u32(1);
}

/// `vkQueueSubmit(queue, 1, [{0 waits, 1 cb, 0 signals}], VK_NULL_HANDLE)`.
fn queue_submit(w: &mut StreamWriter<STREAM_BYTES>, queue: u64, command_buffer: u64) {
    w.header(CMD_QUEUE_SUBMIT, 0);
    w.handle(queue);
    w.u32(1);
    w.u64(1);
    w.i32(ST_SUBMIT_INFO);
    w.count(false);
    w.u32(0);
    w.u64(0);
    w.u64(0);
    w.u32(1);
    w.u64(1);
    w.handle(command_buffer);
    w.u32(0);
    w.u64(0);
    w.handle(0u64);
}

/// Fixed bytes of [`encode_copy_to_buffer_stream`] and its per-region cost.
pub const BUFFER_STREAM_FIXED_BYTES: usize = 48 + 2 * 148 + 48 + 16 + 100;
pub const BUFFER_STREAM_REGION_BYTES: usize = 56;

/// `vkCmdPipelineBarrier` with two full-image GENERAL→GENERAL color barriers,
/// `(srcAccess, dstAccess)` for the source alias then the destination.
fn barrier(
    w: &mut StreamWriter<STREAM_BYTES>,
    ids: CopyIds,
    src_stage: u32,
    dst_stage: u32,
    access: [(u32, u32); 2],
) {
    w.header(CMD_CMD_PIPELINE_BARRIER, 0);
    w.handle(ids.command_buffer);
    w.u32(src_stage);
    w.u32(dst_stage);
    w.u32(0); // dependencyFlags
    w.u32(0); // memoryBarrierCount
    w.u64(0); // pMemoryBarriers array_size
    w.u32(0); // bufferMemoryBarrierCount
    w.u64(0); // pBufferMemoryBarriers array_size
    w.u32(2); // imageMemoryBarrierCount
    w.u64(2); // array_size
    for (image, (src_access, dst_access)) in [ids.src_image, ids.dst_image].into_iter().zip(access)
    {
        w.i32(ST_IMAGE_MEMORY_BARRIER);
        w.count(false); // pNext
        w.u32(src_access);
        w.u32(dst_access);
        w.i32(IMAGE_LAYOUT_GENERAL);
        w.i32(IMAGE_LAYOUT_GENERAL);
        w.u32(QUEUE_FAMILY_IGNORED);
        w.u32(QUEUE_FAMILY_IGNORED);
        w.handle(image);
        w.u32(IMAGE_ASPECT_COLOR); // subresourceRange
        w.u32(0);
        w.u32(1);
        w.u32(0);
        w.u32(1);
    }
}

fn subresource_layers(w: &mut StreamWriter<STREAM_BYTES>) {
    w.u32(IMAGE_ASPECT_COLOR);
    w.u32(0); // mipLevel
    w.u32(0); // baseArrayLayer
    w.u32(1); // layerCount
}

/// Fixed bytes of [`encode_copy_stream`] plus the per-region cost, stated so
/// the capacity claim above is checked by a test rather than by arithmetic in a
/// comment.
pub const STREAM_FIXED_BYTES: usize = 600;
pub const STREAM_REGION_BYTES: usize = 68;

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    fn qword(bytes: &[u8], at: usize) -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[at..at + 8]);
        u64::from_le_bytes(b)
    }

    const IDS: CopyIds = CopyIds {
        queue: 0x1001,
        command_buffer: 0x2002,
        src_image: 0x3003,
        dst_image: 0x4004,
    };

    #[test]
    fn copy_ref_round_trips_and_refuses_stale_shapes() {
        assert_eq!(decode_copy_ref(&encode_copy_ref(Some((2, 7)))), Some((2, 7)));
        assert_eq!(decode_copy_ref(&encode_copy_ref(None)), None);
        assert_eq!(encode_copy_ref(None), [0u8; COPY_REF_BYTES]);
        // A zero serial or an out-of-range slot encodes as "no copy".
        assert_eq!(encode_copy_ref(Some((1, 0))), [0u8; COPY_REF_BYTES]);
        assert_eq!(
            encode_copy_ref(Some((MAX_SLOTS as u32, 5))),
            [0u8; COPY_REF_BYTES]
        );
        let mut bad = encode_copy_ref(Some((1, 9)));
        bad[12] = 1;
        assert_eq!(decode_copy_ref(&bad), None);
        assert_eq!(decode_copy_ref(&[0xFFu8; 16]), None);
        assert_eq!(decode_copy_ref(&[0u8; 15]), None);
        assert!(COPY_REF_OFFSET + COPY_REF_BYTES <= 32, "must sit below the flip record");
    }

    #[test]
    fn whole_window_is_one_region() {
        let g = CopyGeometry {
            src_extent: (1280, 720),
            dst_extent: (1280, 720),
            src_rect: CopyRect::new(0, 0, 1280, 720),
            dst_rect: CopyRect::new(0, 0, 1280, 720),
            sub_rects: &[],
        };
        let mut out = [CopyRegion {
            src_x: 0,
            src_y: 0,
            dst_x: 0,
            dst_y: 0,
            width: 0,
            height: 0,
        }; MAX_REGIONS];
        assert_eq!(g.regions(&mut out), Ok(1));
        assert_eq!(
            out[0],
            CopyRegion {
                src_x: 0,
                src_y: 0,
                dst_x: 0,
                dst_y: 0,
                width: 1280,
                height: 720
            }
        );
    }

    #[test]
    fn sub_rects_are_clipped_translated_and_empties_dropped() {
        // Window content at src (0,0)-(640,480) lands at dst (100,50); the
        // destination surface is only 700x500, so the right column clips.
        let subs = [
            CopyRect::new(100, 50, 400, 250),
            CopyRect::new(400, 50, 800, 250), // clips at dst 700 and at dst_rect 740
            CopyRect::new(900, 900, 950, 950), // outside: dropped
        ];
        let g = CopyGeometry {
            src_extent: (640, 480),
            dst_extent: (700, 500),
            src_rect: CopyRect::new(0, 0, 640, 480),
            dst_rect: CopyRect::new(100, 50, 740, 530),
            sub_rects: &subs,
        };
        let mut out = [CopyRegion {
            src_x: 0,
            src_y: 0,
            dst_x: 0,
            dst_y: 0,
            width: 0,
            height: 0,
        }; MAX_REGIONS];
        assert_eq!(g.regions(&mut out), Ok(2));
        assert_eq!(
            out[0],
            CopyRegion {
                src_x: 0,
                src_y: 0,
                dst_x: 100,
                dst_y: 50,
                width: 300,
                height: 200
            }
        );
        assert_eq!(
            out[1],
            CopyRegion {
                src_x: 300,
                src_y: 0,
                dst_x: 400,
                dst_y: 50,
                width: 300,
                height: 200
            }
        );
    }

    #[test]
    fn stretch_empty_and_overflow_are_refused_by_name() {
        let mut out = [CopyRegion {
            src_x: 0,
            src_y: 0,
            dst_x: 0,
            dst_y: 0,
            width: 0,
            height: 0,
        }; MAX_REGIONS];
        let base = CopyGeometry {
            src_extent: (100, 100),
            dst_extent: (100, 100),
            src_rect: CopyRect::new(0, 0, 100, 100),
            dst_rect: CopyRect::new(0, 0, 50, 50),
            sub_rects: &[],
        };
        assert_eq!(base.regions(&mut out), Err(GeometryRefusal::Stretch));
        let empty = CopyGeometry {
            dst_rect: CopyRect::new(0, 0, 100, 100),
            src_extent: (0, 100),
            ..base
        };
        assert_eq!(empty.regions(&mut out), Err(GeometryRefusal::Empty));
        let many = [CopyRect::new(0, 0, 1, 1); MAX_REGIONS + 1];
        let too_many = CopyGeometry {
            dst_rect: CopyRect::new(0, 0, 100, 100),
            sub_rects: &many,
            ..base
        };
        assert_eq!(too_many.regions(&mut out), Err(GeometryRefusal::TooManyRects));
        let all_outside = [CopyRect::new(500, 500, 600, 600)];
        let outside = CopyGeometry {
            dst_rect: CopyRect::new(0, 0, 100, 100),
            sub_rects: &all_outside,
            ..base
        };
        assert_eq!(outside.regions(&mut out), Err(GeometryRefusal::Empty));
    }

    fn desc(dxgi: u32, bind: u32) -> HeliosWddmAllocationDescV2 {
        let mut d = HeliosWddmAllocationDescV2::header(0, 0);
        d.width = 1280;
        d.height = 720;
        d.depth_or_array_size = 1;
        d.mip_levels = 1;
        d.sample_count = 1;
        d.dxgi_format = dxgi;
        d.bind_flags = bind;
        d.swizzle_class = helios_protocol::HELIOS_HWA2_SWIZZLE_LINEAR;
        d
    }

    #[test]
    fn alias_spec_reproduces_the_measured_him1_shape() {
        // 2026-09-02 `HIM1 pid=6216 1280x720x1 fmt=44 … tiling=0 usage=0x13
        // flags=0x8 … ext=0x0` for bind = RENDER_TARGET|PRESENT.
        let spec = alias_image_spec(&desc(
            87,
            HELIOS_HWA2_BIND_RENDER_TARGET | helios_protocol::HELIOS_HWA2_BIND_PRESENT,
        ))
        .unwrap();
        assert_eq!(spec.format, 44);
        assert_eq!(spec.usage, 0x13);
        assert_eq!(spec.flags, 0x8);
        assert_eq!(spec.tiling, IMAGE_TILING_OPTIMAL);
        assert_eq!(spec.external_handle_type, 0);
        assert_eq!((spec.width, spec.height), (1280, 720));
        // SRV textures log usage=0x17.
        let srv = alias_image_spec(&desc(
            87,
            HELIOS_HWA2_BIND_RENDER_TARGET | HELIOS_HWA2_BIND_SHADER_RESOURCE,
        ))
        .unwrap();
        assert_eq!(srv.usage, 0x17);
    }

    #[test]
    fn alias_spec_refuses_what_it_cannot_mirror() {
        // A LINEAR swizzle claim is NOT a refusal (the UMD says LINEAR on every
        // ordinary texture); a bind set with no RTV/PRESENT/SRV is.
        let mut d = desc(87, HELIOS_HWA2_BIND_RENDER_TARGET);
        d.swizzle_class = helios_protocol::HELIOS_HWA2_SWIZZLE_LINEAR;
        assert!(alias_image_spec(&d).is_ok());
        let d = desc(87, HELIOS_HWA2_BIND_UNORDERED_ACCESS);
        assert_eq!(alias_image_spec(&d), Err(AliasRefusal::Tiling));
        let d = desc(24, HELIOS_HWA2_BIND_RENDER_TARGET);
        assert_eq!(alias_image_spec(&d), Err(AliasRefusal::Format));
        let mut d = desc(87, HELIOS_HWA2_BIND_RENDER_TARGET);
        d.sample_count = 4;
        assert_eq!(alias_image_spec(&d), Err(AliasRefusal::Shape));
        let d = desc(87, HELIOS_HWA2_BIND_DEPTH_STENCIL);
        assert_eq!(alias_image_spec(&d), Err(AliasRefusal::DepthStencil));
    }

    #[test]
    fn alias_image_create_has_no_pnext() {
        let spec = alias_image_spec(&desc(87, HELIOS_HWA2_BIND_RENDER_TARGET)).unwrap();
        let w = crate::encode_image_create(0x10, 0x20, &spec);
        let b = w.finished().unwrap();
        // header(54,1) device(0x10) sp(1) sType(14) pNext array_size(0) flags…
        assert_eq!(word(b, 0), crate::CMD_CREATE_IMAGE);
        assert_eq!(qword(b, 8), 0x10);
        assert_eq!(qword(b, 16), 1);
        assert_eq!(word(b, 24), crate::ST_IMAGE_CREATE_INFO as u32);
        // pNext: the format list only — no VkExternalMemoryImageCreateInfo.
        assert_eq!(qword(b, 28), 1);
        assert_eq!(word(b, 36), crate::ST_IMAGE_FORMAT_LIST_CREATE_INFO as u32);
        assert_eq!(qword(b, 40), 0, "format list pNext");
        assert_eq!(word(b, 48), 2, "viewFormatCount");
        assert_eq!(qword(b, 52), 2, "array_size");
        assert_eq!((word(b, 60), word(b, 64)), (44, 50), "UNORM then SRGB");
        // Then VkImageCreateInfo's own fields.
        assert_eq!(word(b, 68), IMAGE_CREATE_MUTABLE_FORMAT);
        assert_eq!(word(b, 72), crate::IMAGE_TYPE_2D);
        assert_eq!(word(b, 76), 44);
        assert_eq!(spec.view_formats, [44, 50]);
    }

    #[test]
    fn copy_stream_layout_is_exact_for_one_region() {
        let regions = [CopyRegion {
            src_x: 3,
            src_y: 5,
            dst_x: 7,
            dst_y: 11,
            width: 640,
            height: 480,
        }];
        let w = encode_copy_stream(IDS, &regions);
        let b = w.finished().expect("fits");
        assert_eq!(b.len(), STREAM_FIXED_BYTES + STREAM_REGION_BYTES);

        // vkBeginCommandBuffer: 48 bytes.
        assert_eq!(word(b, 0), CMD_BEGIN_COMMAND_BUFFER);
        assert_eq!(word(b, 4), 0);
        assert_eq!(qword(b, 8), IDS.command_buffer);
        assert_eq!(qword(b, 16), 1);
        assert_eq!(word(b, 24), ST_COMMAND_BUFFER_BEGIN_INFO as u32);
        assert_eq!(qword(b, 28), 0);
        assert_eq!(word(b, 36), COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT);
        assert_eq!(qword(b, 40), 0);

        // First barrier at 48: 192 bytes; second image barrier names dst with
        // TRANSFER_WRITE as its destination access.
        let bar = 48;
        assert_eq!(word(b, bar), CMD_CMD_PIPELINE_BARRIER);
        assert_eq!(qword(b, bar + 8), IDS.command_buffer);
        assert_eq!(word(b, bar + 16), PIPELINE_STAGE_ALL_COMMANDS);
        assert_eq!(word(b, bar + 20), PIPELINE_STAGE_TRANSFER);
        assert_eq!(word(b, bar + 52), 2, "imageMemoryBarrierCount");
        assert_eq!(qword(b, bar + 56), 2, "array_size");
        let ib0 = bar + 64;
        assert_eq!(word(b, ib0), ST_IMAGE_MEMORY_BARRIER as u32);
        assert_eq!(word(b, ib0 + 16), ACCESS_TRANSFER_READ);
        assert_eq!(qword(b, ib0 + 36), IDS.src_image);
        let ib1 = ib0 + 64;
        assert_eq!(word(b, ib1 + 16), ACCESS_TRANSFER_WRITE);
        assert_eq!(qword(b, ib1 + 36), IDS.dst_image);
        assert_eq!(word(b, ib1 + 44), IMAGE_ASPECT_COLOR);

        // vkCmdCopyImage at 240.
        let cp = 240;
        assert_eq!(word(b, cp), CMD_CMD_COPY_IMAGE);
        assert_eq!(qword(b, cp + 16), IDS.src_image);
        assert_eq!(word(b, cp + 24), IMAGE_LAYOUT_GENERAL as u32);
        assert_eq!(qword(b, cp + 28), IDS.dst_image);
        assert_eq!(word(b, cp + 40), 1, "regionCount");
        assert_eq!(qword(b, cp + 44), 1, "array_size");
        let r = cp + 52;
        assert_eq!(word(b, r), IMAGE_ASPECT_COLOR);
        assert_eq!(word(b, r + 12), 1, "layerCount");
        assert_eq!((word(b, r + 16), word(b, r + 20), word(b, r + 24)), (3, 5, 0));
        assert_eq!((word(b, r + 44), word(b, r + 48), word(b, r + 52)), (7, 11, 0));
        assert_eq!((word(b, r + 56), word(b, r + 60), word(b, r + 64)), (640, 480, 1));

        // Second barrier at 360, end at 552, submit at 568.
        assert_eq!(word(b, 360), CMD_CMD_PIPELINE_BARRIER);
        assert_eq!(word(b, 360 + 16), PIPELINE_STAGE_TRANSFER);
        assert_eq!(word(b, 360 + 20), PIPELINE_STAGE_ALL_COMMANDS);
        assert_eq!(word(b, 552), CMD_END_COMMAND_BUFFER);
        assert_eq!(qword(b, 560), IDS.command_buffer);
        let sub = 568;
        assert_eq!(word(b, sub), CMD_QUEUE_SUBMIT);
        assert_eq!(qword(b, sub + 8), IDS.queue);
        assert_eq!(word(b, sub + 16), 1);
        assert_eq!(qword(b, sub + 20), 1);
        assert_eq!(word(b, sub + 28), ST_SUBMIT_INFO as u32);
        assert_eq!(qword(b, sub + 32), 0);
        assert_eq!(word(b, sub + 40), 0);
        assert_eq!(qword(b, sub + 44), 0);
        assert_eq!(qword(b, sub + 52), 0);
        assert_eq!(word(b, sub + 60), 1);
        assert_eq!(qword(b, sub + 64), 1);
        assert_eq!(qword(b, sub + 72), IDS.command_buffer);
        assert_eq!(word(b, sub + 80), 0);
        assert_eq!(qword(b, sub + 84), 0);
        assert_eq!(qword(b, sub + 92), 0, "fence");
        assert_eq!(sub + 100, b.len());
    }

    #[test]
    fn target_class_separates_kmd_staging_from_umd_textures() {
        // The measured redirection target: a KMD STANDARD staging surface,
        // 1280 wide, pitch 5120, LINEAR.
        let mut d = desc(87, HELIOS_HWA2_BIND_RENDER_TARGET | HELIOS_HWA2_BIND_SHADER_RESOURCE);
        d.flags |= HELIOS_HWA2_FLAG_STANDARD;
        d.standard_allocation_type = 3;
        d.plane_count = 1;
        d.planes[0].row_pitch = 5120;
        d.planes[0].offset = 0;
        d.byte_size = 5120 * 720;
        assert_eq!(
            target_class(&d),
            Ok(TargetClass::LinearBuffer {
                pitch: 5120,
                offset: 0
            })
        );
        // Same claim from the UMD is a tiled ICD image.
        let u = desc(87, HELIOS_HWA2_BIND_RENDER_TARGET);
        assert_eq!(target_class(&u), Ok(TargetClass::OptimalImage));
        // A pitch that cannot hold the row, or a plane past the extent.
        d.planes[0].row_pitch = 5118;
        assert_eq!(target_class(&d), Err(TargetRefusal::Plane));
        d.planes[0].row_pitch = 5120;
        d.byte_size = 5120 * 719;
        assert_eq!(target_class(&d), Err(TargetRefusal::Plane));
    }

    #[test]
    fn copy_to_buffer_stream_layout_is_exact_for_one_region() {
        let ids = CopyToBufferIds {
            queue: 0x1001,
            command_buffer: 0x2002,
            src_image: 0x3003,
            dst_buffer: 0x5005,
        };
        let regions = [CopyRegion {
            src_x: 3,
            src_y: 5,
            dst_x: 7,
            dst_y: 11,
            width: 640,
            height: 480,
        }];
        let w = encode_copy_to_buffer_stream(ids, &regions, 5120, 256);
        let b = w.finished().expect("fits");
        assert_eq!(b.len(), BUFFER_STREAM_FIXED_BYTES + BUFFER_STREAM_REGION_BYTES);
        // First barrier at 48: one memory barrier then one image barrier.
        let bar = 48;
        assert_eq!(word(b, bar), CMD_CMD_PIPELINE_BARRIER);
        assert_eq!(word(b, bar + 28), 1, "memoryBarrierCount");
        assert_eq!(qword(b, bar + 32), 1);
        assert_eq!(word(b, bar + 40), ST_MEMORY_BARRIER as u32);
        assert_eq!(qword(b, bar + 44), 0);
        assert_eq!(word(b, bar + 52), ACCESS_MEMORY_READ | ACCESS_MEMORY_WRITE);
        assert_eq!(word(b, bar + 56), ACCESS_TRANSFER_WRITE);
        assert_eq!(word(b, bar + 60), 0, "bufferMemoryBarrierCount");
        assert_eq!(qword(b, bar + 64), 0);
        assert_eq!(word(b, bar + 72), 1, "imageMemoryBarrierCount");
        assert_eq!(qword(b, bar + 76), 1);
        let ib = bar + 84;
        assert_eq!(word(b, ib), ST_IMAGE_MEMORY_BARRIER as u32);
        assert_eq!(word(b, ib + 16), ACCESS_TRANSFER_READ);
        assert_eq!(qword(b, ib + 36), ids.src_image);
        assert_eq!(bar + 148, 196);
        // vkCmdCopyImageToBuffer at 196.
        let cp = 196;
        assert_eq!(word(b, cp), CMD_CMD_COPY_IMAGE_TO_BUFFER);
        assert_eq!(qword(b, cp + 8), ids.command_buffer);
        assert_eq!(qword(b, cp + 16), ids.src_image);
        assert_eq!(word(b, cp + 24), IMAGE_LAYOUT_GENERAL as u32);
        assert_eq!(qword(b, cp + 28), ids.dst_buffer);
        assert_eq!(word(b, cp + 36), 1);
        assert_eq!(qword(b, cp + 40), 1);
        let r = cp + 48;
        assert_eq!(qword(b, r), 256 + 11 * 5120 + 7 * 4, "bufferOffset");
        assert_eq!(word(b, r + 8), 1280, "bufferRowLength in texels");
        assert_eq!(word(b, r + 12), 0);
        assert_eq!(word(b, r + 16), IMAGE_ASPECT_COLOR);
        assert_eq!((word(b, r + 32), word(b, r + 36), word(b, r + 40)), (3, 5, 0));
        assert_eq!((word(b, r + 44), word(b, r + 48), word(b, r + 52)), (640, 480, 1));
        // Second barrier at 300, end at 448, submit at 464.
        assert_eq!(word(b, 300), CMD_CMD_PIPELINE_BARRIER);
        assert_eq!(word(b, 448), CMD_END_COMMAND_BUFFER);
        assert_eq!(word(b, 464), CMD_QUEUE_SUBMIT);
        assert_eq!(qword(b, 464 + 72), ids.command_buffer);
        assert_eq!(464 + 100, b.len());
    }

    #[test]
    fn dedicated_import_chain_matches_the_export_dedicated_shape() {
        let w = crate::encode_memory_allocate(
            0x10,
            0x20,
            &crate::MemoryAllocateSpec {
                pnext: crate::MemoryPNext::ImportResourceDedicated {
                    resource_id: 237,
                    image: 0x30,
                    buffer: 0,
                },
                size: 4096,
                memory_type_index: 1,
            },
        );
        let b = w.finished().unwrap();
        assert_eq!(word(b, 0), crate::CMD_ALLOCATE_MEMORY);
        assert_eq!(qword(b, 8), 0x10);
        assert_eq!(qword(b, 16), 1);
        assert_eq!(word(b, 24), crate::ST_MEMORY_ALLOCATE_INFO as u32);
        assert_eq!(qword(b, 28), 1, "pNext present");
        assert_eq!(word(b, 36), crate::ST_IMPORT_MEMORY_RESOURCE_INFO_MESA as u32);
        assert_eq!(qword(b, 40), 1, "import's pNext present");
        assert_eq!(word(b, 48), crate::ST_MEMORY_DEDICATED_ALLOCATE_INFO as u32);
        assert_eq!(qword(b, 52), 0, "dedicated's pNext");
        assert_eq!(qword(b, 60), 0x30, "image");
        assert_eq!(qword(b, 68), 0, "buffer");
        assert_eq!(word(b, 76), 237, "resourceId after the nested struct");
        assert_eq!(qword(b, 80), 4096, "allocationSize");
        assert_eq!(word(b, 88), 1, "memoryTypeIndex");
    }

    #[test]
    fn renderer_device_local_type_follows_the_icd_rule() {
        // The admitted host: type 0 flagless, type 1 DEVICE_LOCAL, type 2
        // HOST_VISIBLE|HOST_COHERENT, type 3 lazily-allocated device-local.
        let flags = [0x0, 0x1, 0x6, 0x11];
        assert_eq!(choose_renderer_device_local_memory_type(&flags, 4), Some(1));
        // A lazy/protected type is skipped even when it comes first.
        let flags = [0x11, 0x21, 0x1];
        assert_eq!(choose_renderer_device_local_memory_type(&flags, 3), Some(2));
        assert_eq!(choose_renderer_device_local_memory_type(&[0x6, 0x2], 2), None);
        assert_eq!(choose_renderer_device_local_memory_type(&[0x1], 0), None);
    }

    #[test]
    fn max_regions_fit_the_stream_buffer() {
        let regions = [CopyRegion {
            src_x: 0,
            src_y: 0,
            dst_x: 0,
            dst_y: 0,
            width: 1,
            height: 1,
        }; MAX_REGIONS];
        let w = encode_copy_stream(IDS, &regions);
        let b = w.finished().expect("MAX_REGIONS must fit STREAM_BYTES");
        assert_eq!(b.len(), STREAM_FIXED_BYTES + MAX_REGIONS * STREAM_REGION_BYTES);
        assert!(b.len() <= STREAM_BYTES);
        let ids = CopyToBufferIds {
            queue: 1,
            command_buffer: 2,
            src_image: 3,
            dst_buffer: 4,
        };
        let w = encode_copy_to_buffer_stream(ids, &regions, 5120, 0);
        let b = w.finished().expect("MAX_REGIONS must fit STREAM_BYTES");
        assert_eq!(
            b.len(),
            BUFFER_STREAM_FIXED_BYTES + MAX_REGIONS * BUFFER_STREAM_REGION_BYTES
        );
    }
}
