//! Venus wire constants: command ids, `VkStructureType` values, the memory,
//! format, usage and layout bit sets, and the ring shared-memory layout.
//!
//! Moved verbatim out of `virtio/venus.rs` by T8/R1104. Adapter-free by
//! construction -- nothing here names `AdapterContext`, `VenusClient` or a
//! mapping. The `Writer` the review also assigns to this module already lives
//! in `kmd_logic` (T0) with seven host tests, so it is not moved here.

// ── venus command type ids (VkCommandTypeEXT) ────────────────────────────────
// Verified against vn_protocol_driver_defines.h.
pub(crate) const CMD_CREATE_INSTANCE: u32 = 0;
pub(crate) const CMD_ENUMERATE_PHYSICAL_DEVICES: u32 = 2;
pub(crate) const CMD_GET_PHYSICAL_DEVICE_MEMORY_PROPERTIES: u32 = 8;
pub(crate) const CMD_CREATE_DEVICE: u32 = 11;
pub(crate) const CMD_FREE_MEMORY: u32 = 22;
pub(crate) const CMD_BIND_IMAGE_MEMORY: u32 = 29;
pub(crate) const CMD_GET_IMAGE_MEMORY_REQUIREMENTS: u32 = 31;
pub(crate) const CMD_DESTROY_IMAGE: u32 = 55;
pub(crate) const CMD_GET_IMAGE_SUBRESOURCE_LAYOUT: u32 = 56;
pub(crate) const CMD_SET_REPLY_COMMAND_STREAM_MESA: u32 = 178;
pub(crate) const CMD_CREATE_RING_MESA: u32 = 188;
pub(crate) const CMD_NOTIFY_RING_MESA: u32 = 190;

/// `VK_COMMAND_GENERATE_REPLY_BIT_EXT` — set in a command's flags word to request
/// a reply written into the previously-set reply command stream.

// ── Vulkan structure-type ids (VkStructureType) ──────────────────────────────
pub(crate) const ST_INSTANCE_CREATE_INFO: i32 = 1;
pub(crate) const ST_DEVICE_QUEUE_CREATE_INFO: i32 = 2;
pub(crate) const ST_DEVICE_CREATE_INFO: i32 = 3;
pub(crate) const ST_RING_CREATE_INFO_MESA: i32 = 1000384000;

/// `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT`.
pub(crate) const EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF: u32 = 0x0000_0200;
pub(crate) const FORMAT_B8G8R8A8_UNORM: u32 = 44;
// IMAGE_TILING_LINEAR / IMAGE_TILING_OPTIMAL moved to `helios_kmd_logic` with
// the encoders that write them (R1002), and the 39th session's evidence moved
// with them. In short: LINEAR was defined as 0 (OPTIMAL), so
// create_linear_scanout_image built a TILED image → device-local-only
// memoryTypeBits (0x3, no host-visible) → choose_host_visible_memory_type failed
// (ScanoutDiag=16 SdgErr=2 / SdgLStg=3). Confirmed against Mesa venus on the same
// NVIDIA host: LINEAR→typebits=0xf (scans out), OPTIMAL→typebits=0x3 (no
// host-visible). There is now a host test asserting the two are 1 and 0.
pub(crate) const IMAGE_USAGE_TRANSFER_SRC: u32 = 0x0000_0001;
pub(crate) const IMAGE_USAGE_TRANSFER_DST: u32 = 0x0000_0002;
pub(crate) const IMAGE_USAGE_SAMPLED: u32 = 0x0000_0004;
pub(crate) const IMAGE_USAGE_STORAGE: u32 = 0x0000_0008;
pub(crate) const IMAGE_USAGE_COLOR_ATTACHMENT: u32 = 0x0000_0010;
pub(crate) const IMAGE_CREATE_MUTABLE_FORMAT: u32 = 0x0000_0008;
pub(crate) const IMAGE_LAYOUT_UNDEFINED: u32 = 0;
pub(crate) const IMAGE_LAYOUT_PREINITIALIZED: u32 = 8;
pub(crate) const IMAGE_ASPECT_COLOR: u32 = 0x0000_0001;

// ── VkMemoryPropertyFlags bits we require ────────────────────────────────────
//
// Defined once in `helios_kmd_logic` alongside the two selectors that read them
// (`choose_host_visible_memory_type` / `choose_device_local_memory_type`), which
// live there because they are pure functions of the host's reported flag array
// and so can carry a host test. VK_MAX_MEMORY_TYPES is the fixed array length
// the host encodes in the memory-properties reply
// (`vn_encode_VkPhysicalDeviceMemoryProperties_partial`), and both the reply
// decoder here and the selectors there must agree on it.
pub(crate) use helios_kmd_logic::{
    MEMORY_PROPERTY_HOST_COHERENT, MEMORY_PROPERTY_HOST_VISIBLE, VK_MAX_MEMORY_TYPES,
};
/// VK_MAX_MEMORY_HEAPS — likewise for the heap array.
pub(crate) const VK_MAX_MEMORY_HEAPS: u32 = 16;

// ── Ring layout (vn_ring `struct layout`, 64-byte aligned header fields) ──────
pub(crate) const RING_HEAD_OFFSET: u64 = 0;
pub(crate) const RING_TAIL_OFFSET: u64 = 64;
pub(crate) const RING_STATUS_OFFSET: u64 = 128;
pub(crate) const RING_BUFFER_OFFSET: u64 = 192;
/// 128 KiB — power of two, matching the ICD's default.
pub(crate) const RING_BUFFER_SIZE: u32 = 131072;
pub(crate) const RING_EXTRA_OFFSET: u64 = RING_BUFFER_OFFSET + RING_BUFFER_SIZE as u64; // 131264
pub(crate) const RING_EXTRA_SIZE: u64 = 4;
/// Total ring shmem = 192 + 131072 + 4 = 131268.
pub(crate) const RING_SHMEM_SIZE: u64 =
    RING_BUFFER_OFFSET + RING_BUFFER_SIZE as u64 + RING_EXTRA_SIZE;
/// Idle timeout reported in the ring-create info (ns); cosmetic for our use.
pub(crate) const RING_IDLE_TIMEOUT_NS: u64 = 1_000_000;

/// Ring status bits (`VkRingStatusFlagsMESA`).
pub(crate) const RING_STATUS_FATAL: u32 = 0x2;

/// Reply shmem size — generous for the small replies we read (largest is the
/// memory-properties reply, ~660 bytes).
pub(crate) const REPLY_SHMEM_SIZE: u64 = 4096;

/// Allocation size of the host-visible page-table memory (16 MiB).
pub(crate) const PAGE_TABLE_ALLOC_SIZE: u64 = 16 * 1024 * 1024;

/// Short spin burst before a ring-head wait falls back to PASSIVE 1 ms sleeps
/// (fast replies stay fast; slow ones cost only sleep latency, never a
/// DISPATCH spin).
pub(crate) const RING_SPIN_BURST: u32 = 50_000;
/// PASSIVE wait budget for the ring head advancing past a published seqno
/// (1 ms sleep-polls). A host that has not consumed the ring in this long is
/// genuinely wedged → the client latches `fatal` ([`FatalReason`]).
///
/// ⚠ OPEN OWNER QUESTION (T4a/R603) — the value is NOT derived from a dxgkrnl
/// deadline, and it is deliberately separate from the 5 s budget every host
/// *fence* wait in this file uses. This wait is reachable from `DxgkDdiPresent`
/// (`ddi/display.rs` → `submit_present_blt` → `ensure_present_image` →
/// `ring_command_reply` → `write_to_ring` → here) while the adapter venus mutex
/// is held, so a caller can in principle block a Present for half a minute —
/// which is several times the Windows default `TdrDdiDelay`, the interval after
/// which dxgkrnl declares a driver hung. If that is right, a real wedge is
/// TDR'd long before this budget expires and the constant bounds post-TDR
/// thread residency rather than preventing a hang.
///
/// It is left at 30 s and NOT shortened here: a bounded wait on a real ring-head
/// watermark is a safety contract, and shortening it without measuring how long
/// a legitimately slow host actually takes would trade a rare stall for a
/// frequent false fatal latch. `VnRingWd` now records the milliseconds actually
/// waited at every expiry, which is the measurement that has to come first.
pub(crate) const RING_WAIT_TIMEOUT_MS: u64 = 30_000;

// The command-stream writer and its capacity live in `helios_kmd_logic`: they
// are pure byte arithmetic with no adapter, handle or wdk-sys edge, and the size
// claim that justifies the buffer is worth a host test. See
// `writer_ext_full_create_device_is_332_bytes` there for the real number — the
// comment that used to sit here said "~120 bytes" and was wrong by 212.
pub(crate) use helios_kmd_logic::{
    encode_image_create, encode_memory_allocate, ImageCreateSpec, MemoryAllocateSpec, MemoryPNext,
    MemoryTypeChoice, Writer, CMD_ALLOCATE_MEMORY, CMD_CREATE_IMAGE, CMD_FLAG_GENERATE_REPLY,
    IMAGE_TILING_LINEAR, IMAGE_TILING_OPTIMAL,
};

/// Why the venus ring was declared unusable. Each arm names a registry counter
/// so a wedge is distinguishable from every other `DeviceError` in a post-mortem
/// `reg query`, at the default `DiagLevel=0`.
pub(crate) enum FatalReason {
    /// The host set `RING_STATUS_FATAL` — it rejected something we encoded.
    /// Records `VnRingFt=1`.
    HostStatusFatal,
    /// The ring head never advanced past our seqno within
    /// [`RING_WAIT_TIMEOUT_MS`]. Records `VnRingWd` = milliseconds waited, so
    /// the dump distinguishes "gave up at the budget" from a short stall.
    HeadWaitTimeout { elapsed_ms: u64 },
}

/// Written into reply word 0 before every reply-generating ring command, so an
/// unanswered reply cannot decode as the previous one. Not a legal
/// `VkCommandTypeEXT`, so it fails every caller's existing command-type check.
pub(crate) const REPLY_POISON: u32 = 0xFFFF_FFFF;

/// Diagnostic breadcrumb base for venus bring-up (0x0D00_00xx).
pub(crate) fn diag(code: u32) {
    crate::diag::record(0x0D00_0000 | (code & 0xFFFF));
}

// ── Guest-assigned Vulkan object handles ──────────────────────────────────────
