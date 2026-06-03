//! virtio-gpu command/response structures (KMD.md Phase 2, TRANSPORT.md §1).
//!
//! Layouts mirror `virtio_gpu.h` from the Linux kernel / virglrenderer and MUST
//! match byte-for-byte, since the host decodes these directly. Every struct is
//! `repr(C)` and laid out to contain no implicit padding so it can derive
//! `Pod`/`Zeroable` for safe zero-copy (de)serialization.

use bytemuck::{Pod, Zeroable};

// ── Control command types (request) ─────────────────────────────────────────
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
pub const VIRTIO_GPU_CMD_GET_CAPSET_INFO: u32 = 0x0108;
pub const VIRTIO_GPU_CMD_GET_CAPSET: u32 = 0x0109;
pub const VIRTIO_GPU_CMD_CTX_CREATE: u32 = 0x0200;
pub const VIRTIO_GPU_CMD_CTX_DESTROY: u32 = 0x0201;
pub const VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const VIRTIO_GPU_CMD_SUBMIT_3D: u32 = 0x0204;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB: u32 = 0x0208;
pub const VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB: u32 = 0x0209;
pub const VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB: u32 = 0x020a;

// ── Response types ────────────────────────────────────────────────────────
pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_OK_CAPSET_INFO: u32 = 0x1102;
pub const VIRTIO_GPU_RESP_OK_CAPSET: u32 = 0x1103;
pub const VIRTIO_GPU_RESP_OK_MAP_INFO: u32 = 0x1105;
pub const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
pub const VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
pub const VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
pub const VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID: u32 = 0x1204;
pub const VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

/// `VIRTIO_GPU_RESP_OK_*` occupy [0x1100, 0x1200); errors are >= 0x1200.
#[inline]
pub fn resp_is_ok(resp_type: u32) -> bool {
    (0x1100..0x1200).contains(&resp_type)
}

// ── Command-header flags ────────────────────────────────────────────────────
/// Request a fence: the device writes a response carrying the same `fence_id`
/// and signals the used ring when the command completes.
pub const VIRTIO_GPU_FLAG_FENCE: u32 = 1 << 0;
/// `ring_idx` field in the header is valid (context init / multi-ring).
pub const VIRTIO_GPU_FLAG_INFO_RING_IDX: u32 = 1 << 1;

// ── Capset IDs ──────────────────────────────────────────────────────────────
pub const VIRTIO_GPU_CAPSET_VIRGL: u32 = 1;
pub const VIRTIO_GPU_CAPSET_VIRGL2: u32 = 2;
pub const VIRTIO_GPU_CAPSET_GFXSTREAM: u32 = 3;
/// Vulkan via Venus. This is the capset Helios drives.
pub const VIRTIO_GPU_CAPSET_VENUS: u32 = 4;

// ── Blob memory / flags ─────────────────────────────────────────────────────
pub const VIRTIO_GPU_BLOB_MEM_GUEST: u32 = 1;
pub const VIRTIO_GPU_BLOB_MEM_HOST3D: u32 = 2;
pub const VIRTIO_GPU_BLOB_MEM_HOST3D_GUEST: u32 = 3;
pub const VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE: u32 = 1;
pub const VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE: u32 = 2;
pub const VIRTIO_GPU_BLOB_FLAG_USE_CROSS_DEVICE: u32 = 4;

pub const VIRTIO_GPU_MAX_SCANOUTS: usize = 16;

/// Control command header — prepended to every virtio-gpu command and response.
/// 24 bytes, 8-byte aligned, no implicit padding.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCtrlHdr {
    pub type_: u32,    // VIRTIO_GPU_CMD_* (request) or VIRTIO_GPU_RESP_* (reply)
    pub flags: u32,    // VIRTIO_GPU_FLAG_*
    pub fence_id: u64, // echoed back in the response when FLAG_FENCE is set
    pub ctx_id: u32,   // 3D context id (0 for global commands)
    pub ring_idx: u8,  // valid only with FLAG_INFO_RING_IDX
    pub padding: [u8; 3],
}

/// `VIRTIO_GPU_CMD_CTX_CREATE`. For Venus, `context_init` carries the capset id.
/// 96 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCtxCreate {
    pub hdr: VirtioGpuCtrlHdr,
    pub nlen: u32,
    pub context_init: u32, // capset_id for Venus (== VIRTIO_GPU_CAPSET_VENUS)
    pub debug_name: [u8; 64],
}

/// `VIRTIO_GPU_CMD_CTX_DESTROY` — header only.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCtxDestroy {
    pub hdr: VirtioGpuCtrlHdr,
}

/// `VIRTIO_GPU_CMD_SUBMIT_3D` — header followed by `size` bytes of Venus stream.
/// 32 bytes (the command data follows in a separate descriptor).
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCmdSubmit {
    pub hdr: VirtioGpuCtrlHdr,
    pub size: u32,
    pub padding: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB`. 56 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuResourceCreateBlob {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub nr_entries: u32,
    pub blob_id: u64,
    pub size: u64,
}

/// `VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB`. 40 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuResourceMapBlob {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub padding: u32,
    pub offset: u64,
}

/// `VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB`. 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuResourceUnmapBlob {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub padding: u32,
}

/// `VIRTIO_GPU_RESP_OK_MAP_INFO` — reply to MAP_BLOB. 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuRespMapInfo {
    pub hdr: VirtioGpuCtrlHdr,
    pub map_info: u32, // host caching flags
    pub padding: u32,
}

/// `VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE` / `..._DETACH_RESOURCE`. 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCtxResource {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub padding: u32,
}

/// `VIRTIO_GPU_CMD_RESOURCE_UNREF`. 32 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuResourceUnref {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub padding: u32,
}

// ── GET_DISPLAY_INFO (Phase 2 smoke test) ───────────────────────────────────

/// A rectangle in the display-info response.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuDisplayOne {
    pub r: VirtioGpuRect,
    pub enabled: u32,
    pub flags: u32,
}

/// `VIRTIO_GPU_RESP_OK_DISPLAY_INFO`. Header + fixed array of scanout descriptors.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuRespDisplayInfo {
    pub hdr: VirtioGpuCtrlHdr,
    pub pmodes: [VirtioGpuDisplayOne; VIRTIO_GPU_MAX_SCANOUTS],
}

// Compile-time guarantees that the on-wire sizes are what the host expects.
const _: () = {
    assert!(core::mem::size_of::<VirtioGpuCtrlHdr>() == 24);
    assert!(core::mem::size_of::<VirtioGpuCtxCreate>() == 96);
    assert!(core::mem::size_of::<VirtioGpuCmdSubmit>() == 32);
    assert!(core::mem::size_of::<VirtioGpuResourceCreateBlob>() == 56);
    assert!(core::mem::size_of::<VirtioGpuResourceMapBlob>() == 40);
    assert!(core::mem::size_of::<VirtioGpuRespMapInfo>() == 32);
    assert!(core::mem::size_of::<VirtioGpuRespDisplayInfo>() == 24 + 16 * 24);
};
