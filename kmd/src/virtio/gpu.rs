//! The virtio-gpu device object, built on the `virtio-drivers` PCI transport.
//!
//! `VirtioGpu` owns the `PciTransport` (discovers/maps the virtio config
//! regions), the control `VirtQueue`, and a contiguous DMA scratch page, and
//! layers the virtio-gpu command protocol (`helios_protocol`) on top. Built by
//! `init` from `evt_device_prepare_hardware` and stored in
//! `AdapterContext::virtio`.
//!
//! Bring-up (all in `init`, at PASSIVE_LEVEL):
//!   M1 — `DxgkConfigAccess` (over BUS_INTERFACE_STANDARD) → `PciRoot` →
//!        `PciTransport::new::<WdkHal,_>`
//!   M2 — feature negotiation via the `Transport` trait
//!   M3 — control `VirtQueue::<WdkHal>` setup + DRIVER_OK
//!   M4 — `GET_DISPLAY_INFO` polled round-trip (Phase-2 smoke test)
//!
//! PHASE 4 TODO: scan for `VIRTIO_PCI_CAP_SHARED_MEMORY_CFG`
//! (shmid == HOST_VISIBLE) here to record the host-visible BAR base for
//! `MAP_BLOB` (ARCH.md §6), and add `alloc_blob`/`map_blob`/`pop_used` for the
//! blob + async-fence paths. Not needed for the Phase 1–3 control path.

use core::sync::atomic::{AtomicU32, Ordering};

use alloc::vec::Vec;

use bytemuck::Zeroable;
use crate::dxgk::_D3DKMDT_VIDPN_PRESENT_PATH_ROTATION;
use helios_protocol::{
    resp_is_ok, VirtioGpuCmdSubmit, VirtioGpuCtrlHdr, VirtioGpuCtxCreate, VirtioGpuCtxDestroy,
    VirtioGpuCtxResource, VirtioGpuMemEntry, VirtioGpuRect, VirtioGpuResourceAttachBacking,
    VirtioGpuResourceCreate2d, VirtioGpuResourceCreateBlob, VirtioGpuResourceFlush,
    VirtioGpuResourceMapBlob, VirtioGpuResourceUnref, VirtioGpuRespDisplayInfo, VirtioGpuRespMapInfo,
    VirtioGpuSetScanout, VirtioGpuSetScanoutBlob, VirtioGpuTransferToHost2d,
    VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE, VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
    VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB,
    VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB, VIRTIO_GPU_CMD_RESOURCE_UNREF,
    VIRTIO_GPU_CMD_SET_SCANOUT, VIRTIO_GPU_CMD_SET_SCANOUT_BLOB, VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
    VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
    HELIOS_OPTIONAL_FEATURES, HELIOS_REQUIRED_FEATURES,
    VIRTIO_GPU_CMD_CTX_CREATE, VIRTIO_GPU_CMD_CTX_DESTROY, VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
    VIRTIO_GPU_CMD_SUBMIT_3D, VIRTIO_GPU_FLAG_FENCE, VIRTIO_GPU_FLAG_INFO_RING_IDX,
    VIRTIO_GPU_MAP_CACHE_MASK,
    VIRTIO_GPU_SHM_ID_HOST_VISIBLE, VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_SHARED_MEMORY_CFG,
};
use virtio_drivers::Hal;
use virtio_drivers::queue::VirtQueue;
use virtio_drivers::transport::pci::bus::{DeviceFunction, PciRoot};
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::{DeviceStatus, Transport};
use virtio_drivers::Error as VirtError;

use wdk_sys::ntddk::KeStallExecutionProcessor;

use super::config::DxgkConfigAccess;
use super::hal::{DmaBuffer, WdkHal};
use super::VirtioError;

/// Control queue index (virtio-gpu controlq = 0; cursorq = 1 is unused).
const CTRL_QUEUE: u16 = 0;
/// Control-queue ring size — power of two, conservatively ≤ the device's max.
const CTRL_QUEUE_SIZE: usize = 64;
/// One page of contiguous DMA scratch, split into request/response halves.
const SCRATCH_BYTES: usize = 4096;

// ── Bounded synchronous control-queue wait ──────────────────────────────────
// gpu.rs busy-polls the control used ring for synchronous commands. The host
// (QEMU) completes commands on its own thread, independent of the guest vCPU, so
// a command the poll never observes completing means the host *wedged* (e.g. a
// stalled GL present backend). An unbounded poll then hangs the guest forever —
// and these synchronous round-trips run under the virtio spinlock at
// DISPATCH_LEVEL (`AdapterContext::with_virtio`), where an unbounded spin is a
// hard VM hang that eventually trips the DPC watchdog (bugcheck 0x133).
//
// So every poll is bounded: each iteration stalls `CTRL_WAIT_STALL_US` µs
// (`KeStallExecutionProcessor` is wall-clock-calibrated and callable at any IRQL),
// up to `CTRL_WAIT_MAX_STALLS` iterations ≈ 500 ms, then the command fails with
// `DeviceError` and the transport latches [`VirtioGpu::wedged`]. 500 ms is ~15×
// any healthy command yet well under the single-DPC watchdog budget; once
// `wedged` latches, later commands fail fast so total stall stays ≈ one timeout.
const CTRL_WAIT_STALL_US: u32 = 20;
const CTRL_WAIT_MAX_STALLS: u32 = 25_000;

// ── Host-visible shared-memory window (ARCH §6) ─────────────────────────────
// PCI config-space byte offsets used by the capability walk.
const PCI_CFG_STATUS: u16 = 0x04; // command (low 16) | status (high 16)
const PCI_STATUS_CAP_LIST: u32 = 1 << 4; // status bit 4: capabilities list present
const PCI_CFG_CAP_PTR: u16 = 0x34; // first capability offset (in the low byte)
const PCI_CFG_BAR0: u16 = 0x10; // BAR0; BARn at 0x10 + n*4
const PCI_CAP_ID_VNDR: u32 = 0x09; // generic PCI vendor-specific capability id

/// The host-visible memory window: a prefetchable 64-bit PCI BAR (QEMU
/// `hostmem=`) that `RESOURCE_MAP_BLOB` injects resource mappings into. Discovered
/// from the `SHARED_MEMORY_CFG`/`HOST_VISIBLE` capability during `init`.
#[derive(Clone, Copy)]
pub struct HostVisibleWindow {
    /// Guest-physical base of the window (BAR base + the cap's offset).
    pub base: u64,
    /// Window length in bytes (== QEMU `hostmem=`).
    pub len: u64,
}

/// Page size for host-visible window offset allocation + MDL sizing. The window
/// is a PCI BAR; mappings are page-granular.
const BLOB_PAGE: u64 = 4096;

/// Upper bound on a single host-visible blob mapping (256 MiB). Bounds the user-VA
/// pressure of one `MAP_BLOB` so a single map cannot request multi-GB — this
/// shrinks (does not close) the window in which
/// `MmMapLockedPagesSpecifyCache(UserMode)` raises an uncatchable failure exception
/// (ioctl.rs `map_io_pages_to_user`; the load-bearing fix there is a SEH shim,
/// pending). Generous for bring-up; bump if a real venus allocation exceeds it.
const MAX_BLOB_MAP_BYTES: u64 = 256 << 20;

/// Round `n` up to the next [`BLOB_PAGE`] multiple (saturating).
const fn round_up_page(n: u64) -> u64 {
    n.saturating_add(BLOB_PAGE - 1) & !(BLOB_PAGE - 1)
}

/// Result of the under-lock phase of `MAP_BLOB` ([`VirtioGpu::map_blob_prepare`]):
/// the guest-physical range to map and the host's requested caching. The user-space
/// mapping itself (MDL + `MmMapLockedPagesSpecifyCache`) is built by the caller at
/// PASSIVE_LEVEL, OUTSIDE the virtio spinlock (ARCH §6; the IRQL split).
#[derive(Clone, Copy)]
pub struct BlobMapPrep {
    /// Guest-physical base of the resource's mapping inside the host-visible window.
    pub gpa: u64,
    /// Page-rounded length to map, in bytes.
    pub size: u64,
    /// Host caching nibble (`VIRTIO_GPU_MAP_CACHE_*`) from `RESP_OK_MAP_INFO`.
    pub map_cache: u32,
}

/// Walk the PCI capability list for the virtio `SHARED_MEMORY_CFG` capability
/// whose shmid is `HOST_VISIBLE`, returning its guest-physical (base, length).
/// virtio-drivers' `PciTransport` ignores cap type 8, so we scan it ourselves
/// over the bus interface. Returns `None` if absent (a device built without
/// blob/hostmem), which makes `MAP_BLOB` unavailable rather than crashing.
fn scan_host_visible_window(access: &DxgkConfigAccess) -> Option<HostVisibleWindow> {
    if (access.read32(PCI_CFG_STATUS) >> 16) & PCI_STATUS_CAP_LIST == 0 {
        return None;
    }
    // Capability pointers are dword-aligned; mask the reserved low 2 bits.
    let mut cap = (access.read32(PCI_CFG_CAP_PTR) & 0xFF) as u16 & 0xFC;
    // Bounded walk — a corrupt cap_next cannot escape the 256-byte config space.
    for _ in 0..48 {
        if cap == 0 {
            break;
        }
        let d0 = access.read32(cap);
        let cap_id = d0 & 0xFF;
        let cap_next = ((d0 >> 8) & 0xFF) as u16 & 0xFC;
        let cfg_type = (d0 >> 24) & 0xFF;
        if cap_id == PCI_CAP_ID_VNDR && cfg_type == VIRTIO_PCI_CAP_SHARED_MEMORY_CFG as u32 {
            // `virtio_pci_cap`: bar at +4 byte0, id (shmid) at +4 byte1.
            let d1 = access.read32(cap + 4);
            let bar = (d1 & 0xFF) as u16;
            let shmid = (d1 >> 8) & 0xFF;
            if shmid == VIRTIO_GPU_SHM_ID_HOST_VISIBLE as u32 {
                // `virtio_pci_cap64`: offset lo/hi at +8/+16, length lo/hi at +12/+20.
                let off =
                    access.read32(cap + 8) as u64 | ((access.read32(cap + 16) as u64) << 32);
                let len =
                    access.read32(cap + 12) as u64 | ((access.read32(cap + 20) as u64) << 32);
                let base = bar_base(access, bar)?;
                return Some(HostVisibleWindow { base: base + off, len });
            }
        }
        cap = cap_next;
    }
    None
}

/// Read the guest-physical base a memory BAR was assigned, handling the 64-bit
/// (type 0b10) layout the prefetchable host-visible window uses.
fn bar_base(access: &DxgkConfigAccess, bar: u16) -> Option<u64> {
    if bar > 5 {
        return None;
    }
    let reg = PCI_CFG_BAR0 + bar * 4;
    let lo = access.read32(reg);
    if lo & 0x1 != 0 {
        return None; // I/O-space BAR — not the memory window
    }
    let base = (lo & 0xFFFF_FFF0) as u64;
    // Memory BAR type in bits [2:1]: 0b10 == 64-bit (high half in BARn+1).
    if (lo >> 1) & 0x3 == 0x2 {
        Some(base | ((access.read32(reg + 4) as u64) << 32))
    } else {
        Some(base)
    }
}

/// Walk the PCI capability list for the virtio `ISR_CFG` capability and return the
/// guest-physical address of the 1-byte ISR status register (`bar_base + offset`),
/// or `None` if absent. Reading that register acks + de-asserts the device's INTx
/// line; the DOD's `DxgkDdiInterruptRoutine` needs its kernel VA (mapped in `init`)
/// to ack interrupts lock-free at DIRQL. Same cap-walk as [`scan_host_visible_window`],
/// but the ISR cap is a plain 32-bit `virtio_pci_cap` (offset at +8).
fn scan_isr_status(access: &DxgkConfigAccess) -> Option<u64> {
    if (access.read32(PCI_CFG_STATUS) >> 16) & PCI_STATUS_CAP_LIST == 0 {
        return None;
    }
    let mut cap = (access.read32(PCI_CFG_CAP_PTR) & 0xFF) as u16 & 0xFC;
    for _ in 0..48 {
        if cap == 0 {
            break;
        }
        let d0 = access.read32(cap);
        let cap_id = d0 & 0xFF;
        let cap_next = ((d0 >> 8) & 0xFF) as u16 & 0xFC;
        let cfg_type = (d0 >> 24) & 0xFF;
        if cap_id == PCI_CAP_ID_VNDR && cfg_type == VIRTIO_PCI_CAP_ISR_CFG as u32 {
            // `virtio_pci_cap`: bar at +4 byte0, offset (u32) at +8.
            let bar = (access.read32(cap + 4) & 0xFF) as u16;
            let off = access.read32(cap + 8) as u64;
            let base = bar_base(access, bar)?;
            return Some(base + off);
        }
        cap = cap_next;
    }
    None
}

/// Base for guest-assigned blob resource ids. Started well above the low ids a
/// prior display driver (inbox VioGpuDod) may have used for scanout/framebuffer
/// resources that can survive the driver swap — the host rejects a colliding id
/// with `VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID` (0x1203). Phase 4b.
const RESOURCE_ID_BASE: u32 = 0x1000;

/// Max concurrently-live blob resources tracked per device. The registry's
/// backing buffer is reserved to this capacity once at init; the cap bounds
/// growth so inserts never reallocate. Generous for bring-up; the ICD's working
/// set is far smaller. Table-full → OutOfMemory.
const MAX_BLOBS: usize = 256;

/// One tracked blob resource. Phase 4c will add `window_offset`/`user_va`/`mdl`
/// when the blob is mapped.
#[derive(Clone, Copy)]
struct BlobSlot {
    resource_id: u32,
    /// Blob size in bytes (from ALLOC_BLOB; MAP_BLOB needs it to size the MDL).
    size: u64,
}

/// Blob registry (resource_id → metadata). The backing `Vec` is **heap**-allocated
/// (NOT an inline array) — an inline `[BlobSlot; 256]` lived in `VirtioGpu`, which
/// is built by value on the kernel stack in `init`, and the ~4 KB array overflowed
/// the small kernel stack → `0x7F` double fault on every driver load. The buffer
/// is reserved to `MAX_BLOBS` at init (PASSIVE_LEVEL); `insert` stays within that
/// reserved capacity so it never reallocates and is therefore safe to call under
/// the virtio spinlock at DISPATCH_LEVEL.
struct BlobTable {
    slots: Vec<BlobSlot>,
}

impl BlobTable {
    /// Reserve the backing buffer up front (PASSIVE_LEVEL). After this, `insert`
    /// up to `MAX_BLOBS` entries performs no further allocation.
    fn with_reserved_capacity() -> Self {
        Self {
            slots: Vec::with_capacity(MAX_BLOBS),
        }
    }

    /// Record a freshly created blob. Errors (without allocating) if the table is
    /// at capacity — `push` below stays within the reserved buffer, so it makes no
    /// allocator call and is safe under the spinlock.
    fn insert(&mut self, resource_id: u32, size: u64) -> Result<(), VirtioError> {
        if self.slots.len() >= MAX_BLOBS {
            return Err(VirtioError::OutOfMemory);
        }
        self.slots.push(BlobSlot { resource_id, size });
        Ok(())
    }

    /// Look up a tracked blob's size (None if `resource_id` is unknown).
    fn size_of(&self, resource_id: u32) -> Option<u64> {
        self.slots
            .iter()
            .find(|s| s.resource_id == resource_id)
            .map(|s| s.size)
    }
}

// ── Async submission (Phase 4e) ─────────────────────────────────────────────
//
// `submit_venus` is non-blocking: it adds the SUBMIT_3D descriptors to the
// control queue, notifies, and RETURNS without polling the used ring. Each such
// in-flight submission keeps its device-visible buffers alive in
// [`VirtioGpu::inflight`] until the host completes it; completions are reaped
// (popped + freed) lazily from `submit_venus`/`fence_complete`/`quiesce_into`.
//
// IRQL: these run under the AdapterContext virtio spinlock at DISPATCH_LEVEL, so
// they MUST NOT allocate or free `DmaBuffer`s (Mm{Allocate,Free}ContiguousMemory
// are PASSIVE-only). Buffers are allocated by the caller at PASSIVE before the
// lock; completed `InFlight` entries are *moved* (never dropped) into a
// caller-supplied `retired` Vec under the lock and dropped by the caller at
// PASSIVE after the lock releases.

/// Bytes reserved at the front of a submit's metadata `DmaBuffer`: the
/// device-read SUBMIT_3D header followed by the device-written ctrl response.
/// `[0, hdr) = VirtioGpuCmdSubmit`, `[hdr, hdr+resp) = VirtioGpuCtrlHdr`.
pub const SUBMIT_META_BYTES: usize =
    core::mem::size_of::<VirtioGpuCmdSubmit>() + core::mem::size_of::<VirtioGpuCtrlHdr>();

/// Max simultaneously in-flight async submissions tracked + max entries reaped
/// into one `retired` Vec. The control queue (`CTRL_QUEUE_SIZE` descriptors, 3
/// per submit) caps real concurrency well below this; the generous bound makes
/// the in-flight `Vec` never reallocate (so `push` is alloc-free under the lock).
pub const MAX_INFLIGHT: usize = CTRL_QUEUE_SIZE;

/// One outstanding async SUBMIT_3D. Owns its device-visible buffers for as long
/// as the device may DMA them (until `pop_used`).
pub struct InFlight {
    /// Descriptor-chain head returned by `VirtQueue::add` (the pop_used token).
    token: u16,
    /// The submit's fence id (monotonic; the ICD assigns it). `fence_complete`
    /// reports a fence done once no in-flight entry carries its id.
    fence_id: u64,
    /// `[SUBMIT_3D header | ctrl response]` (see [`SUBMIT_META_BYTES`]).
    meta: DmaBuffer,
    /// The opaque Venus command stream (device-read).
    venus: DmaBuffer,
}

impl InFlight {
    /// A carrier that owns ONLY buffers awaiting free — no live token/fence. Used
    /// by `submit_venus`'s error paths to hand `meta`/`venus` back to the caller's
    /// `retired` list so they are freed at PASSIVE_LEVEL, never dropped inside the
    /// DISPATCH-level virtio spinlock (`Mm*ContiguousMemory` is PASSIVE-only).
    /// `pop_used` is never called on a carrier (it is only dropped).
    fn to_free(meta: DmaBuffer, venus: DmaBuffer) -> Self {
        Self {
            token: 0,
            fence_id: 0,
            meta,
            venus,
        }
    }
}

/// Reconstruct the exact `(hdr, venus, resp)` slices a submit was `add`ed with,
/// from the raw buffer pointers, so `pop_used` can be handed the matching set
/// without forming a `&`/`&mut` borrow of `self.inflight` across the
/// `self.control` call (mirrors the scratch raw-pointer pattern).
///
/// # Safety
/// `meta`/`venus` must point at the live `InFlight`'s buffers; `meta` must hold
/// at least [`SUBMIT_META_BYTES`] and `venus` at least `venus_len` bytes. The
/// returned slices alias those buffers and must not outlive them.
unsafe fn submit_slices<'a>(
    meta: *mut u8,
    venus: *mut u8,
    venus_len: usize,
) -> (&'a [u8], &'a [u8], &'a mut [u8]) {
    let hdr_len = core::mem::size_of::<VirtioGpuCmdSubmit>();
    let resp_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
    // SAFETY: the three sub-spans are disjoint and within the buffers' lengths
    // (hdr at [0,hdr_len), resp at [hdr_len,hdr_len+resp_len) in `meta`; venus is
    // a separate buffer). The caller upholds the pointer/length contract.
    let hdr = core::slice::from_raw_parts(meta, hdr_len);
    let resp = core::slice::from_raw_parts_mut(meta.add(hdr_len), resp_len);
    let venus = core::slice::from_raw_parts(venus, venus_len);
    (hdr, venus, resp)
}

/// An initialized virtio-gpu transport.
pub struct VirtioGpu {
    /// The virtio-modern PCI transport (owns the mapped cfg-region VAs).
    transport: PciTransport,
    /// Control virtqueue (queue 0) — all GPU commands ride this.
    control: VirtQueue<WdkHal, CTRL_QUEUE_SIZE>,
    /// Contiguous DMA scratch page for synchronous command buffers. RAII —
    /// `DmaBuffer::drop` frees the page (including on `init`'s early-error paths).
    scratch: DmaBuffer,
    /// Next virtio-gpu 3D context id to hand out (guest-assigned; 0 is the
    /// reserved global context, so we start at 1). Phase 3.
    next_ctx_id: AtomicU32,
    /// Next virtio-gpu resource id to hand out (0 is reserved). Phase 3 (M3.5).
    next_resource_id: AtomicU32,
    /// The host-visible blob window (SHARED_MEMORY_CFG / HOST_VISIBLE), or `None`
    /// if the device exposes none. `MAP_BLOB` maps resources from here (ARCH §6).
    host_visible: Option<HostVisibleWindow>,
    /// Tracks live blob resources (id → size); see [`BlobTable`].
    blobs: BlobTable,
    /// Bump allocator for host-visible window offsets. Each `MAP_BLOB` claims
    /// `[next_window_offset, next_window_offset + round_up_page(size))` and advances
    /// the pointer. Never reclaimed (bring-up): the window is multi-GB and the ICD's
    /// mapped working set is small. Guarded by the AdapterContext virtio lock.
    next_window_offset: u64,
    /// In-flight async SUBMIT_3D submissions (Phase 4e). Completions are matched
    /// by descriptor token, NOT by position: with per-queue `ring_idx` fence
    /// routing the host can retire fenced commands out of submission order, so the
    /// used-ring head is not necessarily this list's front. Capacity is reserved
    /// to [`MAX_INFLIGHT`] at init so `push` never reallocates (alloc-free under
    /// the spinlock).
    inflight: Vec<InFlight>,
    /// Highest fence id ever submitted. Fence ids are globally monotonic (the ICD
    /// assigns them under its device mutex), so a fence `f` has been submitted iff
    /// `f <= max_submitted_fence_id`; combined with "`f` is no longer in-flight"
    /// that means it has completed. Drives `IOCTL_HELIOS_WAIT_FENCE`
    /// (out-of-order-safe — see [`VirtioGpu::fence_complete`]).
    max_submitted_fence_id: u64,
    /// Persistent, physically-contiguous guest framebuffer backing the 2D desktop
    /// scanout resource (`DESKTOP_RES_ID`). Allocated once per mode in
    /// [`set_desktop_mode`]; `DxgkDdiPresentDisplayOnly` blts the desktop primary
    /// into it, then `TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH` push it to the host
    /// scanout (Phase 7.1 DOD desktop path). `None` until the first mode set.
    desktop_fb: Option<DmaBuffer>,
    /// Current desktop mode width/height (pixels); valid iff `desktop_fb.is_some()`.
    desktop_w: u32,
    desktop_h: u32,
    /// True iff the last [`Self::set_desktop_mode`] actually programmed the scanout
    /// (CREATE_2D → ATTACH → SET_SCANOUT all succeeded), as opposed to merely
    /// installing the framebuffer. Gates the idempotent CommitVidPn fast-path so a
    /// transient host command-error on the StartDevice install does not suppress the
    /// retry CommitVidPn would otherwise perform. Distinct from `desktop_fb.is_some()`,
    /// which is set before the commands run (so the fb is freed on teardown even on
    /// failure). Cleared by a fresh transport (StopDevice → StartDevice).
    desktop_programmed: bool,
    /// Latched once a synchronous control round-trip times out waiting for the
    /// host to complete it (the control queue wedged — see [`CTRL_WAIT_MAX_STALLS`]).
    /// Every subsequent command fails fast (`DeviceError`) instead of re-stalling,
    /// bounding total DISPATCH-level spin to one timeout. Cleared only by a fresh
    /// transport (StopDevice → StartDevice rebuilds `VirtioGpu`).
    wedged: bool,
    /// virtio-gpu `type_` of the most recent synchronous control command, recorded
    /// before the round-trip so a wedged/failed present can be attributed to the
    /// exact command at PASSIVE_LEVEL (after the spinlock drops) — see
    /// [`VirtioGpu::diag_last_cmd`]. Diagnostic only; benign cross-IRQL read.
    diag_last_cmd: u32,
    /// Kernel VA of the `VIRTIO_PCI_ISR` status register (0 if the cap is absent),
    /// mapped in `init`. Copied to `AdapterContext::isr_status_va` so the DOD's ISR
    /// can ack the virtio interrupt lock-free at DIRQL (see that field).
    isr_status_va: usize,
}

/// Fixed virtio-gpu resource id for the 2D desktop scanout. Held well clear of
/// the blob path's allocator (`RESOURCE_ID_BASE = 0x1000`+) so it never collides.
const DESKTOP_RES_ID: u32 = 0x100;

impl VirtioGpu {
    /// Bring the virtio-gpu device online and prove it with `GET_DISPLAY_INFO`.
    pub fn init(access: &DxgkConfigAccess) -> Result<Self, VirtioError> {
        // ── M1: discover the device + map BARs through the bus interface ────
        // A function driver doesn't own the bus, so config space is reached via
        // the PCI bus's BUS_INTERFACE_STANDARD (GetBusData/SetBusData); the
        // DeviceFunction is a formality (DxgkConfigAccess ignores it and
        // addresses our own device via the bus-interface context). BAR MMIO is
        // mapped on demand by WdkHal inside PciTransport::new.
        let mut root = PciRoot::new(*access);
        let device_function = DeviceFunction {
            bus: 0,
            device: 0,
            function: 0,
        };
        let mut transport = PciTransport::new::<WdkHal, _>(&mut root, device_function)
            .map_err(|_| VirtioError::DeviceError)?;
        // TEMP init-progress breadcrumb (→ HeliosStep, survives a hung re-init since
        // no DDI runs after to overwrite it). 0x0200_001N = init milestone N.
        crate::diag::record(0x0200_0011); // M1: PCI transport mapped

        // ── M2: feature negotiation (VirtIO 1.2 spec §3.1.1) ────────────────
        transport.set_status(DeviceStatus::empty()); // reset
        let mut spins = 0u32;
        while !transport.get_status().is_empty() && spins < 100_000 {
            spins += 1;
        }
        crate::diag::record(0x0200_0021); // M2a: reset-wait done
        transport.set_status(DeviceStatus::ACKNOWLEDGE);
        transport.set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER);

        let offered = transport.read_device_features();
        crate::diag::record(0x0200_0022); // M2b: device features read
        let accepted = offered & (HELIOS_REQUIRED_FEATURES | HELIOS_OPTIONAL_FEATURES);
        transport.write_driver_features(accepted);
        transport.set_status(
            DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::FEATURES_OK,
        );
        if !transport.get_status().contains(DeviceStatus::FEATURES_OK)
            || accepted & HELIOS_REQUIRED_FEATURES != HELIOS_REQUIRED_FEATURES
        {
            crate::diag::record(0x0200_002F); // M2x: FEATURES_OK rejected
            transport.set_status(DeviceStatus::FAILED);
            return Err(VirtioError::FeatureRejected);
        }
        crate::diag::record(0x0200_0023); // M2c: features OK, entering VirtQueue::new

        // ── M3: control virtqueue (queue 0), then DRIVER_OK ─────────────────
        let mut control = VirtQueue::<WdkHal, CTRL_QUEUE_SIZE>::new(
            &mut transport,
            CTRL_QUEUE,
            /* indirect */ false,
            /* event_idx */ false,
        )
        .map_err(|_| VirtioError::DeviceError)?;

        // Suppress device used-ring interrupts (VIRTQ_AVAIL_F_NO_INTERRUPT). The
        // Phase 1–3 control path is purely synchronous — every command rides
        // `add_notify_wait_pop`, which POLLS the used ring; nothing reads the
        // virtio ISR-status register. Leaving interrupts enabled would assert a
        // level-triggered INTx line (virtio-drivers does not program MSI-X) that
        // our ISR never acks → an interrupt storm. Phase 4 re-enables this
        // (set_dev_notify(true)) when the DPC becomes the used-ring consumer.
        control.set_dev_notify(false);

        transport.set_status(
            DeviceStatus::ACKNOWLEDGE
                | DeviceStatus::DRIVER
                | DeviceStatus::FEATURES_OK
                | DeviceStatus::DRIVER_OK,
        );
        crate::diag::record(0x0200_0013); // M3: control virtqueue up, DRIVER_OK

        // ── M4: GET_DISPLAY_INFO polled round-trip (smoke test) ─────────────
        // Request + response live in one contiguous page so each buffer is
        // physically contiguous for the device (our Hal::share is identity — no
        // bounce buffer). Halves are disjoint (split_at_mut): request is read by
        // the device, response is written by it. `scratch` is RAII: any `?`
        // early-return below frees the page via DmaBuffer::drop.
        let mut scratch = DmaBuffer::new(SCRATCH_BYTES).ok_or(VirtioError::OutOfMemory)?;
        let (req_buf, resp_buf) = scratch.as_mut_slice().split_at_mut(SCRATCH_BYTES / 2);

        let hdr_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
        let resp_len = core::mem::size_of::<VirtioGpuRespDisplayInfo>();
        let mut req = VirtioGpuCtrlHdr::zeroed();
        req.type_ = VIRTIO_GPU_CMD_GET_DISPLAY_INFO;
        req_buf[..hdr_len].copy_from_slice(bytemuck::bytes_of(&req));

        crate::diag::record(0x0200_0014); // M4a: about to poll GET_DISPLAY_INFO (UNBOUNDED)
        control
            .add_notify_wait_pop(
                &[&req_buf[..hdr_len]],
                &mut [&mut resp_buf[..resp_len]],
                &mut transport,
            )
            .map_err(|_| VirtioError::DeviceError)?;
        crate::diag::record(0x0200_0015); // M4b: GET_DISPLAY_INFO completed

        let resp: &VirtioGpuRespDisplayInfo = bytemuck::from_bytes(&resp_buf[..resp_len]);
        if !resp_is_ok(resp.hdr.type_) {
            return Err(VirtioError::DeviceError);
        }
        crate::kmsg(c"Helios: virtio-gpu GET_DISPLAY_INFO OK\n");

        // ── M5: locate the host-visible blob window (ARCH §6) ───────────────
        // Best-effort: absence only disables MAP_BLOB, it does not fail init.
        let host_visible = scan_host_visible_window(access);
        crate::kmsg(if host_visible.is_some() {
            c"Helios: host-visible window found\n"
        } else {
            c"Helios: no host-visible window (MAP_BLOB unavailable)\n"
        });

        // ── M6: map the ISR status register so the DOD ISR can ack the virtio
        // interrupt (read de-asserts INTx). Best-effort: 0 ⇒ the ISR declines every
        // interrupt as before (only safe if the device never asserts).
        let isr_status_va = match scan_isr_status(access) {
            // SAFETY: maps the device's 1-byte ISR status BAR register at PASSIVE
            // (WdkHal caches it; the VA stays valid until DxgkDdiUnload). A single
            // volatile read of it at DIRQL acks the interrupt — that is its only use.
            Some(gpa) => unsafe { WdkHal::mmio_phys_to_virt(gpa, 4) }.as_ptr() as usize,
            None => 0,
        };
        crate::kmsg(if isr_status_va != 0 {
            c"Helios: ISR status register mapped\n"
        } else {
            c"Helios: no ISR cap (interrupts unacked!)\n"
        });
        crate::diag::record(0x0200_0016); // M6: scans done, init succeeding

        Ok(Self {
            transport,
            control,
            scratch,
            next_ctx_id: AtomicU32::new(1),
            next_resource_id: AtomicU32::new(RESOURCE_ID_BASE),
            host_visible,
            blobs: BlobTable::with_reserved_capacity(),
            next_window_offset: 0,
            inflight: Vec::with_capacity(MAX_INFLIGHT),
            max_submitted_fence_id: 0,
            desktop_fb: None,
            desktop_w: 0,
            desktop_h: 0,
            desktop_programmed: false,
            wedged: false,
            diag_last_cmd: 0,
            isr_status_va,
        })
    }

    /// The host-visible blob window discovered at init, if any. `MAP_BLOB` uses
    /// it to translate a resource's window offset into a guest-physical range.
    pub fn host_visible(&self) -> Option<HostVisibleWindow> {
        self.host_visible
    }

    /// Kernel VA of the `VIRTIO_PCI_ISR` status register (0 if absent). Copied into
    /// `AdapterContext` so the DOD's ISR can ack the virtio interrupt lock-free.
    pub fn isr_status_va(&self) -> usize {
        self.isr_status_va
    }

    // ── Venus control path (Phase 3, M3.2) ──────────────────────────────────
    //
    // All three methods drive the control virtqueue *synchronously* via
    // `add_notify_wait_pop` (polled used-ring round-trip), like `init`. They take
    // `&mut self` and assume the caller holds the AdapterContext spinlock so the
    // shared `scratch` page and control queue are not touched concurrently
    // (escape submits at PASSIVE today; the DPC drain arrives in M3.4). They run
    // under that spinlock at DISPATCH_LEVEL, so they perform NO allocation — any
    // payload buffer (the Venus stream) is allocated by the caller at PASSIVE and
    // passed in already contiguous.

    /// Create a virtio-gpu 3D context bound to `capset_id` (Venus = 4) and return
    /// the guest-assigned context id.
    ///
    /// Like every synchronous roundtrip it first [`quiesce_into`]s any in-flight
    /// async submits: those occupy the shared control queue, and the synchronous
    /// `add_notify_wait_pop` below pops by token assuming ITS chain is next on the
    /// used ring — which only holds once the queue has no async work ahead of it.
    pub fn ctx_create(
        &mut self,
        capset_id: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<u32, VirtioError> {
        self.quiesce_into(retired)?;
        let ctx_id = self.next_ctx_id.fetch_add(1, Ordering::Relaxed);
        let mut cmd = VirtioGpuCtxCreate::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_CREATE;
        cmd.hdr.ctx_id = ctx_id;
        // With VIRTIO_GPU_F_CONTEXT_INIT, context_init carries the capset id.
        cmd.context_init = capset_id;
        // A debug name helps host-side (virglrenderer) logs; purely cosmetic.
        const NAME: &[u8] = b"helios";
        cmd.nlen = NAME.len() as u32;
        cmd.debug_name[..NAME.len()].copy_from_slice(NAME);
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))?;
        Ok(ctx_id)
    }

    /// Destroy a previously created 3D context.
    pub fn ctx_destroy(
        &mut self,
        ctx_id: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let mut cmd = VirtioGpuCtxDestroy::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DESTROY;
        cmd.hdr.ctx_id = ctx_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    /// Allocate a virtio-gpu blob resource in `ctx_id` and return its guest-
    /// assigned resource id (Phase 4b). `blob_mem`/`blob_flags` are the caller's
    /// `VIRTIO_GPU_BLOB_MEM_*` / `VIRTIO_GPU_BLOB_FLAG_*` (HOST3D + USE_MAPPABLE
    /// for a host-visible mappable blob). HOST3D blobs are host-backed, so
    /// `nr_entries = 0` (no guest page list follows the command). The size is
    /// recorded so a later `MAP_BLOB` can size the MDL.
    ///
    /// `blob_id` is the venus device-memory id backing a HOST3D mappable blob
    /// (the ICD's `bo_ops.create_from_device_memory(mem_id)` → ALLOC_BLOB; ARCH §5).
    /// A standalone scratch blob with no venus backing passes `blob_id = 0` (the
    /// host then rejects a HOST3D mappable blob — see phase4-blob-plan).
    pub fn alloc_blob(
        &mut self,
        ctx_id: u32,
        blob_mem: u32,
        blob_flags: u32,
        blob_id: u64,
        size: u64,
        retired: &mut Vec<InFlight>,
    ) -> Result<u32, VirtioError> {
        if size == 0 {
            return Err(VirtioError::DeviceError);
        }
        // Drain in-flight async submits before the synchronous create+attach
        // roundtrips (shared control queue; see `ctx_create`).
        self.quiesce_into(retired)?;
        let resource_id = self.next_resource_id.fetch_add(1, Ordering::Relaxed);
        let mut cmd = VirtioGpuResourceCreateBlob::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB;
        cmd.hdr.ctx_id = ctx_id;
        cmd.resource_id = resource_id;
        cmd.blob_mem = blob_mem;
        cmd.blob_flags = blob_flags;
        cmd.nr_entries = 0;
        cmd.blob_id = blob_id;
        cmd.size = size;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))?;

        // Attach the blob resource to its 3D context. The Linux virtio-gpu kernel
        // driver issues VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE for every context
        // resource right after creating it (confirmed by ftrace of a working venus
        // guest: create_blob -> map -> ctx_attach_resource). Without it the host's
        // venus context never binds the resource, so a subsequent RESOURCE_MAP_BLOB
        // (and venus ring use) fails with "resource does not exist". The resource id
        // namespace is per-context for venus, so the attach is required before map.
        let mut attach = VirtioGpuCtxResource::zeroed();
        attach.hdr.type_ = VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE;
        attach.hdr.ctx_id = ctx_id;
        attach.resource_id = resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&attach))?;

        // Record only after the host accepted the create + attach (so a failed
        // create doesn't occupy a slot). Done under the caller's spinlock — alloc-free.
        self.blobs.insert(resource_id, size)?;
        Ok(resource_id)
    }

    /// Bind a venus blob `resource_id` to scanout 0 for zero-copy display
    /// (`VIRTIO_GPU_CMD_SET_SCANOUT_BLOB`) — the Phase-7 go/no-go gate path
    /// (DISPLAY.md §8). The blob must already be an **exportable** HOST3D resource
    /// (created via `alloc_blob(blob_id = venus mem id)` and rendered into); QEMU's
    /// `virgl_cmd_set_scanout_blob` materializes its `dmabuf_fd` (set at blob-create
    /// by `virgl_renderer_resource_get_info`) and the host GL backend presents it
    /// under `-spice gl=on`. A non-OK roundtrip here (e.g. `RESP_ERR_UNSPEC`,
    /// "resource not backed by a dmabuf") is the gate's *failure* signal — the venus
    /// image wasn't created exportable / ANV didn't export it.
    ///
    /// `stride`/`offset` are the plane-0 geometry of a LINEAR image (the host needs
    /// them to interpret the imported dmabuf). This is a device-global scanout
    /// command, not a 3D-context one, so `hdr.ctx_id = 0`.
    pub fn set_scanout_blob(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
        format: u32,
        stride: u32,
        offset: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        // Drain in-flight async submits before the synchronous control roundtrip
        // (shared control queue; see `ctx_create`/`alloc_blob`).
        self.quiesce_into(retired)?;
        let mut cmd = VirtioGpuSetScanoutBlob::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_SET_SCANOUT_BLOB;
        cmd.r = VirtioGpuRect { x: 0, y: 0, width, height };
        cmd.scanout_id = 0;
        cmd.resource_id = resource_id;
        cmd.width = width;
        cmd.height = height;
        cmd.format = format;
        cmd.strides[0] = stride;
        cmd.offsets[0] = offset;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    /// Flush a scanout resource's full rect to the display
    /// (`VIRTIO_GPU_CMD_RESOURCE_FLUSH`). For a GL/blob scanout under `-spice
    /// gl=on` this is what drives the actual host present. Device-global
    /// (`hdr.ctx_id = 0`).
    pub fn resource_flush(
        &mut self,
        resource_id: u32,
        width: u32,
        height: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let mut cmd = VirtioGpuResourceFlush::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_FLUSH;
        cmd.r = VirtioGpuRect { x: 0, y: 0, width, height };
        cmd.resource_id = resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    // ── 2D desktop scanout (Phase 7.1 DOD desktop path) ─────────────────────
    // The non-blob 2D scanout: a guest-page-backed BGRX resource the DOD paints
    // the Windows desktop primary into, displayed by the host under `-spice
    // gl=on`. All device-global (`hdr.ctx_id = 0`); each low-level helper drains
    // the async venus pool first (`quiesce_into`) exactly like the blob path.

    /// `VIRTIO_GPU_CMD_RESOURCE_CREATE_2D` — create a host-side 2D resource.
    fn resource_create_2d(
        &mut self,
        resource_id: u32,
        format: u32,
        width: u32,
        height: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let mut cmd = VirtioGpuResourceCreate2d::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_CREATE_2D;
        cmd.resource_id = resource_id;
        cmd.format = format;
        cmd.width = width;
        cmd.height = height;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    /// `VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING` with a single contiguous run.
    /// The header + one `VirtioGpuMemEntry` (48 bytes total) are sent as one
    /// device-read buffer; a contiguous framebuffer needs exactly one entry.
    fn attach_backing_contiguous(
        &mut self,
        resource_id: u32,
        addr: u64,
        length: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let mut hdr = VirtioGpuResourceAttachBacking::zeroed();
        hdr.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING;
        hdr.resource_id = resource_id;
        hdr.nr_entries = 1;
        let entry = VirtioGpuMemEntry { addr, length, padding: 0 };
        // header (32) || entry (16) contiguous — the device reads one buffer.
        let mut buf = [0u8; 48];
        buf[..32].copy_from_slice(bytemuck::bytes_of(&hdr));
        buf[32..].copy_from_slice(bytemuck::bytes_of(&entry));
        self.ctrl_roundtrip(&buf)
    }

    /// `VIRTIO_GPU_CMD_SET_SCANOUT` — bind a 2D resource to a scanout (or detach
    /// with `resource_id = 0`).
    fn set_scanout(
        &mut self,
        scanout_id: u32,
        resource_id: u32,
        width: u32,
        height: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let mut cmd = VirtioGpuSetScanout::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_SET_SCANOUT;
        cmd.r = VirtioGpuRect { x: 0, y: 0, width, height };
        cmd.scanout_id = scanout_id;
        cmd.resource_id = resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    /// `VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D` — push a rect of the resource's guest
    /// backing into the host copy. `offset` is the byte offset of (x,y).
    fn transfer_to_host_2d(
        &mut self,
        resource_id: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        offset: u64,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let mut cmd = VirtioGpuTransferToHost2d::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D;
        cmd.r = VirtioGpuRect { x, y, width, height };
        cmd.offset = offset;
        cmd.resource_id = resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    /// `VIRTIO_GPU_CMD_RESOURCE_UNREF` — free a host resource.
    fn resource_unref(
        &mut self,
        resource_id: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let mut cmd = VirtioGpuResourceUnref::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_UNREF;
        cmd.resource_id = resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    /// Install a desktop scanout mode (W×H, BGRX) on scanout 0. `new_fb` is a
    /// caller-allocated (PASSIVE) physically-contiguous buffer ≥ `W*H*4` that
    /// backs the 2D resource. Tears down any prior scanout, then
    /// CREATE_2D → ATTACH_BACKING → SET_SCANOUT(0).
    ///
    /// Returns the **old** framebuffer (if any) for the caller to drop at
    /// PASSIVE_LEVEL — a `DmaBuffer` must never be freed here under the virtio
    /// spinlock (DISPATCH). The new fb is installed regardless of command
    /// outcome (so it is owned + freed at transport teardown), so the command
    /// Result is returned alongside the old fb rather than via `?`.
    pub fn set_desktop_mode(
        &mut self,
        new_fb: DmaBuffer,
        width: u32,
        height: u32,
        retired: &mut Vec<InFlight>,
    ) -> (Option<DmaBuffer>, Result<(), VirtioError>) {
        let addr = new_fb.phys();
        let length = (width as u64).saturating_mul(height as u64).saturating_mul(4) as u32;
        let had_prior = self.desktop_fb.is_some();
        // Install the new fb first (so it is owned and freed at PASSIVE on
        // teardown even if a command below fails); extract the old fb to return.
        let old = self.desktop_fb.replace(new_fb);
        self.desktop_w = width;
        self.desktop_h = height;
        // Best-effort detach + unref of the prior host resource so it releases
        // the old fb's pages before the caller drops `old`.
        if had_prior {
            let _ = self.set_scanout(0, 0, 0, 0, retired);
            let _ = self.resource_unref(DESKTOP_RES_ID, retired);
        }
        let result = (|| {
            self.resource_create_2d(
                DESKTOP_RES_ID,
                VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
                width,
                height,
                retired,
            )?;
            self.attach_backing_contiguous(DESKTOP_RES_ID, addr, length, retired)?;
            self.set_scanout(0, DESKTOP_RES_ID, width, height, retired)
        })();
        // Record whether the scanout is actually live, so the idempotent CommitVidPn
        // fast-path skips re-programming only a *successfully* programmed mode (a
        // failed/partial program here leaves this false → CommitVidPn retries).
        self.desktop_programmed = result.is_ok();
        (old, result)
    }

    /// True once a desktop scanout mode has been installed.
    pub fn desktop_ready(&self) -> bool {
        self.desktop_fb.is_some()
    }

    /// True iff the current scanout was *successfully* programmed (not merely the
    /// framebuffer installed). Gates the idempotent CommitVidPn fast-path — see
    /// [`Self::desktop_programmed`] (the field).
    pub fn desktop_programmed(&self) -> bool {
        self.desktop_programmed
    }

    /// Current desktop scanout geometry `(width, height)`, or `(0, 0)` if no mode
    /// has been installed yet.
    pub fn desktop_dims(&self) -> (u32, u32) {
        if self.desktop_fb.is_some() {
            (self.desktop_w, self.desktop_h)
        } else {
            (0, 0)
        }
    }

    /// TEMP diagnostic: fill the desktop scanout with a solid BGRX color and
    /// flush it to the host. Used to distinguish "source hidden/black" from a
    /// broken virtio transfer/scanout path.
    pub fn fill_desktop_color(
        &mut self,
        b: u8,
        g: u8,
        r: u8,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        let (w, h) = (self.desktop_w, self.desktop_h);
        {
            let fb = self.desktop_fb.as_mut().ok_or(VirtioError::DeviceError)?;
            let dst = fb.as_mut_slice();
            for px in dst.chunks_exact_mut(4) {
                px[0] = b;
                px[1] = g;
                px[2] = r;
                px[3] = 0xFF;
            }
        }
        self.transfer_to_host_2d(DESKTOP_RES_ID, 0, 0, w, h, 0, retired)?;
        self.resource_flush(DESKTOP_RES_ID, w, h, retired)
    }

    /// Blt the present source surface (full frame) into the desktop framebuffer,
    /// then push it to the host scanout (`TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH`
    /// of the whole frame). `src` is the system-memory desktop primary (locked
    /// non-paged for the present); `src_pitch` is signed and can be negative for a
    /// bottom-up surface. Per-row copy handles src/dst pitch mismatch. (Dirty-rect
    /// optimization deferred — first light pushes the full frame.)
    pub unsafe fn present_desktop(
        &mut self,
        src: *const u8,
        src_pitch: isize,
        rotation: _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::Type,
        retired: &mut Vec<InFlight>,
    ) -> Result<u32, VirtioError> {
        if src.is_null() || src_pitch == 0 {
            return Err(VirtioError::DeviceError);
        }
        let (w, h) = (self.desktop_w, self.desktop_h);
        let dst_pitch = (w as usize).saturating_mul(4);
        let src_row_bytes = if src_pitch < 0 {
            src_pitch.saturating_neg() as usize
        } else {
            src_pitch as usize
        };
        let copy_bytes = dst_pitch.min(src_row_bytes);
        let mut src_or: u8 = 0;
        let mut dst_or: u8 = 0;
        {
            let fb = self.desktop_fb.as_mut().ok_or(VirtioError::DeviceError)?;
            let dst = fb.as_mut_slice();
            if rotation == _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_ROTATE90 {
                let src_width = h as usize;
                let src_height = w as usize;
                let src_needed = src_width.saturating_mul(4);
                if src_row_bytes < src_needed {
                    return Err(VirtioError::DeviceError);
                }
                for dy in 0..h as usize {
                    let dst_row = dy.saturating_mul(dst_pitch);
                    for dx in 0..w as usize {
                        let sx = dy;
                        let sy = src_height.saturating_sub(1).saturating_sub(dx);
                        let row = unsafe { src.offset((sy as isize).saturating_mul(src_pitch)) };
                        let so = sx.saturating_mul(4);
                        let doo = dst_row.saturating_add(dx.saturating_mul(4));
                        if doo + 4 <= dst.len() && so + 4 <= src_row_bytes {
                            unsafe {
                                core::ptr::copy_nonoverlapping(row.add(so), dst[doo..doo + 4].as_mut_ptr(), 4);
                            }
                            if dy < 16 && dx < 64 {
                                for i in 0..4 {
                                    let b = unsafe { core::ptr::read(row.add(so + i)) };
                                    src_or |= b;
                                    dst_or |= dst[doo + i];
                                }
                            }
                        }
                    }
                }
            } else {
                for y in 0..h as usize {
                    let doo = y.saturating_mul(dst_pitch);
                    if doo + copy_bytes <= dst.len() {
                        let row = unsafe { src.offset((y as isize).saturating_mul(src_pitch)) };
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                row,
                                dst[doo..doo + copy_bytes].as_mut_ptr(),
                                copy_bytes,
                            );
                        }
                        if y < 16 {
                            let sample = copy_bytes.min(256);
                            for i in 0..sample {
                                let b = unsafe { core::ptr::read(row.add(i)) };
                                src_or |= b;
                                dst_or |= dst[doo + i];
                            }
                        }
                    }
                }
            }
        }
        self.transfer_to_host_2d(DESKTOP_RES_ID, 0, 0, w, h, 0, retired)?;
        self.resource_flush(DESKTOP_RES_ID, w, h, retired)?;
        Ok(0x0D00_0000 | ((src_or as u32) << 8) | dst_or as u32)
    }

    /// Submit an opaque Venus command stream to `ctx_id`, fenced with `fence_id`
    /// — **non-blocking** (Phase 4e). Adds the SUBMIT_3D descriptors, notifies the
    /// device, records the submission as in-flight, and RETURNS WITHOUT WAITING.
    /// Completion is observed later (here on the next call, in `fence_complete`,
    /// or in `quiesce_into`). This breaks the synchronous-submit deadlock class: a
    /// submit whose host fence can only retire after a *later* submit no longer
    /// stalls the single control channel.
    ///
    /// Buffer ownership: the device DMAs `meta`/`venus`'s physical pages until the
    /// matching `pop_used`, so this TAKES OWNERSHIP of both and parks them in the
    /// in-flight pool. `meta` must be ≥ [`SUBMIT_META_BYTES`] (this writes the
    /// SUBMIT_3D header into it and reserves the response tail); `venus` is the
    /// command stream (`venus_len` device-read bytes). Both must be allocated by
    /// the caller at PASSIVE_LEVEL.
    ///
    /// `retired` collects in-flight entries reaped here (drain-before-submit, plus
    /// backpressure draining when the queue is full); the caller drops them at
    /// PASSIVE to free their `DmaBuffer`s — never under this spinlock.
    pub fn submit_venus(
        &mut self,
        ctx_id: u32,
        fence_id: u64,
        ring_idx: u32,
        mut meta: DmaBuffer,
        venus: DmaBuffer,
        venus_len: usize,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        // Every error path below routes `meta`/`venus` into `retired` so the
        // caller frees them at PASSIVE — they must never be dropped here at the
        // DISPATCH-level spinlock. Success parks them in the in-flight pool.
        if venus_len == 0
            || venus_len > venus.as_slice().len()
            || meta.as_slice().len() < SUBMIT_META_BYTES
        {
            retired.push(InFlight::to_free(meta, venus));
            return Err(VirtioError::DeviceError);
        }
        // Honor the wedged latch (like ctrl_roundtrip / map_blob_roundtrip): once a
        // synchronous command timed out, a stale chain sits permanently at the used-
        // ring head, so adding here would push onto a desynced queue whose
        // completion can never be reaped. Fail fast, routing the buffers to `retired`
        // for the PASSIVE-level free (never dropped under this DISPATCH spinlock).
        if self.wedged {
            retired.push(InFlight::to_free(meta, venus));
            return Err(VirtioError::DeviceError);
        }
        // Reap anything the host has already completed, freeing queue slots.
        if let Err(e) = self.drain_completed(retired) {
            retired.push(InFlight::to_free(meta, venus));
            return Err(e);
        }

        let mut cmd = VirtioGpuCmdSubmit::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_SUBMIT_3D;
        cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE;
        cmd.hdr.fence_id = fence_id;
        cmd.hdr.ctx_id = ctx_id;
        // Route the fence to this queue's venus host timeline. ring_idx 0 is the
        // CPU/primary ring (global fence is fine); a queue is bound to ring_idx>0
        // and the host must create the fence on that context+ring timeline
        // (QEMU virgl_renderer_context_create_fence) for venus's per-queue waits
        // (vkQueueWaitIdle) to complete. Mirrors the virtgpu EXECBUF ring_idx.
        if ring_idx > 0 {
            cmd.hdr.flags |= VIRTIO_GPU_FLAG_INFO_RING_IDX;
            cmd.hdr.ring_idx = ring_idx as u8;
        }
        cmd.size = venus_len as u32;

        let hdr_len = core::mem::size_of::<VirtioGpuCmdSubmit>();
        meta.as_mut_slice()[..hdr_len].copy_from_slice(bytemuck::bytes_of(&cmd));

        // Raw pointers to the owned buffers so the `(hdr, venus, resp)` slices do
        // not borrow `self.inflight`/`meta`/`venus` across the `self.control` call
        // (and so the same slices can be rebuilt verbatim for `pop_used`).
        let meta_ptr = meta.as_slice().as_ptr() as *mut u8;
        let venus_ptr = venus.as_slice().as_ptr() as *mut u8;

        // Two device-readable descriptors (submit header + Venus stream) and one
        // device-writable response descriptor (TRANSPORT §7 two-descriptor + resp).
        // On QueueFull, block-drain completions (the device is making progress on
        // earlier submits) until a slot frees, then retry. The loop yields a
        // Result (no conditional move of meta/venus inside it); the single move
        // onto the error path happens once, after the loop.
        // ONE ~500 ms budget shared across ALL QueueFull retries (not reset per
        // retry) — same DPC-watchdog reasoning as `quiesce_into`.
        let mut qf_budget = CTRL_WAIT_MAX_STALLS;
        let token_result: Result<u16, VirtioError> = loop {
            // SAFETY: `meta`/`venus` are owned and outlive the in-flight entry
            // (parked below until `pop_used`); the slices are within their lengths.
            let (hdr, venus_in, resp) = unsafe { submit_slices(meta_ptr, venus_ptr, venus_len) };
            // SAFETY: the buffers remain valid until the matching `pop_used`
            // (drain_completed / quiesce_into / fence_complete), per `add`'s contract.
            match unsafe { self.control.add(&[hdr, venus_in], &mut [resp]) } {
                Ok(t) => break Ok(t),
                Err(VirtError::QueueFull) => {
                    if self.inflight.is_empty() {
                        // Empty pool but the queue rejects a 3-descriptor chain —
                        // nothing will free up. Surface rather than spin forever.
                        break Err(VirtioError::DeviceError);
                    }
                    // Bounded wait for a slot to free (was an unbounded spin); a
                    // wedged host surfaces as DeviceError instead of hanging.
                    if let Err(e) = self.wait_can_pop_budget(&mut qf_budget) {
                        break Err(e);
                    }
                    if let Err(e) = self.drain_completed(retired) {
                        break Err(e);
                    }
                }
                Err(_) => break Err(VirtioError::DeviceError),
            }
        };
        let token = match token_result {
            Ok(t) => t,
            Err(e) => {
                retired.push(InFlight::to_free(meta, venus));
                return Err(e);
            }
        };

        if self.control.should_notify() {
            self.transport.notify(CTRL_QUEUE);
        }

        // Park the submission. `push` stays within the reserved capacity
        // (drain above keeps `inflight.len() < MAX_INFLIGHT`), so it is alloc-free.
        self.inflight.push(InFlight {
            token,
            fence_id,
            meta,
            venus,
        });
        if fence_id > self.max_submitted_fence_id {
            self.max_submitted_fence_id = fence_id;
        }
        Ok(())
    }

    /// Pop every control-queue completion the host has posted, moving each retired
    /// [`InFlight`] into `retired` (for the caller to free at PASSIVE).
    /// Non-blocking: stops as soon as the used ring is empty.
    ///
    /// Completions are matched by descriptor token (`peek_used` → find the
    /// in-flight entry with that token), NOT by list position: per-`ring_idx` fence
    /// routing lets the host retire fenced commands out of submission order, so the
    /// used-ring head may be any in-flight entry. `pop_used` requires the exact
    /// `(inputs, outputs)` the chain was `add`ed with, reconstructed here from the
    /// matched entry's buffers.
    fn drain_completed(&mut self, retired: &mut Vec<InFlight>) -> Result<(), VirtioError> {
        while self.control.can_pop() {
            // The token (descriptor-chain head) of the next used element.
            let next_token = match self.control.peek_used() {
                Some(t) => t,
                None => break,
            };
            // Find which in-flight submission that completion belongs to.
            let i = match self.inflight.iter().position(|e| e.token == next_token) {
                Some(i) => i,
                None => {
                    // A completion whose token we don't track. We cannot `pop_used`
                    // it without its matching buffers, and skipping it would desync
                    // the used ring. Post-quiesce every queued chain is a submit we
                    // track, so this is not expected; stop draining defensively.
                    break;
                }
            };
            // Copy out Copy fields + raw buffer pointers so no borrow of
            // `self.inflight` is held across the `self.control.pop_used` call.
            let (token, meta_ptr, venus_ptr, venus_len) = {
                let e = &self.inflight[i];
                (
                    e.token,
                    e.meta.as_slice().as_ptr() as *mut u8,
                    e.venus.as_slice().as_ptr() as *mut u8,
                    e.venus.as_slice().len(),
                )
            };
            // SAFETY: same `(hdr, venus, resp)` set originally `add`ed (the buffers
            // are still owned by `inflight[i]`); `pop_used` consumes this token,
            // which equals the next used element (peeked above).
            let (hdr, venus_in, resp) = unsafe { submit_slices(meta_ptr, venus_ptr, venus_len) };
            let popped = unsafe { self.control.pop_used(token, &[hdr, venus_in], &mut [resp]) };
            match popped {
                Ok(_) => {
                    // The slot is done regardless of the host's response status.
                    // Move (do not drop) into the caller's PASSIVE-level free list.
                    let done = self.inflight.remove(i);
                    retired.push(done);
                }
                // NotReady/WrongToken: nothing more to reap right now.
                Err(_) => break,
            }
        }
        Ok(())
    }

    /// Block-drain ALL in-flight async submits to completion. Used before any
    /// synchronous control roundtrip (which assumes an otherwise-idle queue) and
    /// from teardown. Retired entries go to `retired` (freed by the caller at
    /// PASSIVE).
    fn quiesce_into(&mut self, retired: &mut Vec<InFlight>) -> Result<(), VirtioError> {
        // ONE ~500 ms budget for the WHOLE drain (not per completion) — a slow host
        // completing each parked entry just under a per-call cap must not let the
        // loop spin for N × 500 ms at DISPATCH and trip the DPC watchdog.
        let mut budget = CTRL_WAIT_MAX_STALLS;
        while !self.inflight.is_empty() {
            // Bounded (was an unbounded spin) — a wedged host must not hang the
            // DISPATCH-level caller. On budget exhaustion `wedged` latches and we bail.
            self.wait_can_pop_budget(&mut budget)?;
            let before = self.inflight.len();
            self.drain_completed(retired)?;
            if self.inflight.len() == before {
                // A completion was signaled but did not retire the front entry
                // (out-of-order/WrongToken) — should not happen on the FIFO control
                // queue. Bail rather than spin forever.
                return Err(VirtioError::DeviceError);
            }
        }
        Ok(())
    }

    /// Reap completions, then report whether `fence_id` has completed. A fence is
    /// complete iff it was submitted (`fence_id <= max_submitted_fence_id`) and is
    /// no longer in-flight — which is correct even when fences retire out of
    /// submission order (per-`ring_idx` routing). `fence_id == 0` (the ICD's
    /// "no specific fence") is always complete. Drives `IOCTL_HELIOS_WAIT_FENCE`
    /// (the IOCTL handler does the PASSIVE-level wait/poll loop around this).
    ///
    /// The ICD only waits on a fence it has already submitted, so a `fence_id`
    /// that is `<= max_submitted_fence_id` and absent from `inflight` has truly
    /// completed (it is not a not-yet-submitted future id).
    pub fn fence_complete(
        &mut self,
        fence_id: u64,
        retired: &mut Vec<InFlight>,
    ) -> Result<bool, VirtioError> {
        self.drain_completed(retired)?;
        if fence_id == 0 {
            return Ok(true);
        }
        // Submitted (id assigned monotonically, so `<= max`) AND no longer parked
        // in the in-flight pool ⇒ the host retired it. Robust to out-of-order
        // completion across rings.
        let still_pending = self.inflight.iter().any(|e| e.fence_id == fence_id);
        Ok(fence_id <= self.max_submitted_fence_id && !still_pending)
    }

    /// Phase-1 (under-lock) half of `MAP_BLOB` (ARCH §6): reserve a host-visible
    /// window offset for `resource_id`, tell the host to inject the resource's
    /// mapping there (`RESOURCE_MAP_BLOB`), and return the guest-physical range +
    /// host caching for the caller to map into user space at PASSIVE_LEVEL.
    ///
    /// Runs under the AdapterContext virtio spinlock (DISPATCH_LEVEL) — alloc-free.
    /// The window offset is consumed (bumped) only after the host accepts the
    /// command, so a rejected map does not waste window space. The caller
    /// ([`crate::ioctl`]) must ensure the resource is not already mapped (it tracks
    /// live mappings in the AdapterContext mapping table) before calling this.
    pub fn map_blob_prepare(
        &mut self,
        resource_id: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<BlobMapPrep, VirtioError> {
        // Drain in-flight async submits before the synchronous RESOURCE_MAP_BLOB
        // roundtrip (shared control queue; see `ctx_create`).
        self.quiesce_into(retired)?;
        let window = self.host_visible.ok_or(VirtioError::DeviceError)?;
        // The resource must have been created (ALLOC_BLOB) so we know its size.
        let size = self.blobs.size_of(resource_id).ok_or(VirtioError::DeviceError)?;
        let map_len = round_up_page(size);
        if map_len == 0 || map_len > MAX_BLOB_MAP_BYTES {
            return Err(VirtioError::DeviceError);
        }
        // Bump-allocate a page-aligned window offset; refuse if the window is full.
        let offset = self.next_window_offset;
        let end = offset.checked_add(map_len).ok_or(VirtioError::OutOfMemory)?;
        if end > window.len {
            return Err(VirtioError::OutOfMemory);
        }

        // Host round-trip FIRST — only mutate `next_window_offset` on success so a
        // rejected map leaves the allocator state unchanged.
        let mut cmd = VirtioGpuResourceMapBlob::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB;
        cmd.resource_id = resource_id;
        cmd.offset = offset;
        let map_info = self.map_blob_roundtrip(&cmd)?;

        self.next_window_offset = end;
        Ok(BlobMapPrep {
            gpa: window.base + offset,
            size: map_len,
            map_cache: map_info & VIRTIO_GPU_MAP_CACHE_MASK,
        })
    }

    /// Send `RESOURCE_MAP_BLOB` and return the host's `map_info` caching word from
    /// the `RESP_OK_MAP_INFO` reply. Reuses the scratch page (req low / resp high).
    fn map_blob_roundtrip(&mut self, cmd: &VirtioGpuResourceMapBlob) -> Result<u32, VirtioError> {
        let req = bytemuck::bytes_of(cmd);
        let resp_len = core::mem::size_of::<VirtioGpuRespMapInfo>();
        // SAFETY: owned contiguous page; disjoint req/resp halves, serialized by
        // the caller's spinlock. Raw pointer (not a &mut borrow of self.scratch) so
        // self.control/transport can be borrowed for the round-trip.
        let buf = unsafe {
            core::slice::from_raw_parts_mut(
                self.scratch.as_slice().as_ptr() as *mut u8,
                SCRATCH_BYTES,
            )
        };
        let (req_buf, resp_buf) = buf.split_at_mut(SCRATCH_BYTES / 2);
        if req.len() > req_buf.len() || resp_len > resp_buf.len() {
            return Err(VirtioError::DeviceError);
        }
        if self.wedged {
            return Err(VirtioError::DeviceError);
        }
        self.diag_last_cmd = VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB;
        req_buf[..req.len()].copy_from_slice(req);
        // Bounded add → notify → poll → pop (see `ctrl_roundtrip` for the rationale
        // and the safety argument for leaving a chain queued on a wedged timeout).
        // SAFETY: `req_buf`/`resp_buf` alias `scratch` via a raw pointer (alive for
        // the transport's lifetime), not a borrow of `self`; valid through pop_used.
        let token = match unsafe {
            self.control
                .add(&[&req_buf[..req.len()]], &mut [&mut resp_buf[..resp_len]])
        } {
            Ok(t) => t,
            Err(_) => {
                self.wedged = true;
                return Err(VirtioError::DeviceError);
            }
        };
        if self.control.should_notify() {
            self.transport.notify(CTRL_QUEUE);
        }
        self.wait_can_pop_bounded()?;
        // SAFETY: same buffers passed to `add`; still valid (see above).
        unsafe {
            self.control
                .pop_used(token, &[&req_buf[..req.len()]], &mut [&mut resp_buf[..resp_len]])
        }
        .map_err(|_| VirtioError::DeviceError)?;
        let resp: &VirtioGpuRespMapInfo = bytemuck::from_bytes(&resp_buf[..resp_len]);
        if resp_is_ok(resp.hdr.type_) {
            Ok(resp.map_info)
        } else {
            Err(VirtioError::DeviceError)
        }
    }

    /// Poll the control used ring for one completion, charging the busy-wait
    /// against a caller-owned stall budget (each `KeStallExecutionProcessor` tick
    /// decrements it). When the budget hits zero it latches [`Self::wedged`] and
    /// returns `DeviceError`. A *shared* budget is what bounds a MULTI-completion
    /// drain (`quiesce_into`, the `submit_venus` QueueFull loop): the budget must
    /// span the whole drain, not reset per completion — otherwise a slow-but-alive
    /// host that completes each entry just under the per-call cap never latches
    /// `wedged`, and N entries × ~500 ms cumulatively spin the DISPATCH-level caller
    /// long enough to trip the DPC watchdog (0x133).
    fn wait_can_pop_budget(&mut self, budget: &mut u32) -> Result<(), VirtioError> {
        if self.wedged {
            return Err(VirtioError::DeviceError);
        }
        while !self.control.can_pop() {
            if *budget == 0 {
                self.wedged = true;
                return Err(VirtioError::DeviceError);
            }
            // SAFETY: KeStallExecutionProcessor is callable at any IRQL (we may be
            // at DISPATCH under the virtio spinlock); it only busy-waits ~µs.
            unsafe { KeStallExecutionProcessor(CTRL_WAIT_STALL_US) };
            *budget -= 1;
        }
        Ok(())
    }

    /// Wait for ONE completion with a fresh ~500 ms budget ([`CTRL_WAIT_MAX_STALLS`]).
    /// Correct for the single-command synchronous round-trips (`ctrl_roundtrip`,
    /// `map_blob_roundtrip`), which wait on exactly one completion before the
    /// spinlock is released. Multi-completion drains must use
    /// [`Self::wait_can_pop_budget`] with a carried budget instead.
    fn wait_can_pop_bounded(&mut self) -> Result<(), VirtioError> {
        let mut budget = CTRL_WAIT_MAX_STALLS;
        self.wait_can_pop_budget(&mut budget)
    }

    /// virtio-gpu `type_` of the most recent synchronous control command. Read at
    /// PASSIVE (after the spinlock drops) to attribute a wedged/failed present to
    /// the exact command — e.g. `0x0104` RESOURCE_FLUSH, `0x0105` TRANSFER_TO_HOST_2D,
    /// `0x0103` SET_SCANOUT, `0x0102` RESOURCE_UNREF, `0x0101` RESOURCE_CREATE_2D.
    pub fn diag_last_cmd(&self) -> u32 {
        self.diag_last_cmd
    }

    /// True once a synchronous control command timed out (the host stopped
    /// completing the control queue). Latches until a fresh transport is built.
    pub fn is_wedged(&self) -> bool {
        self.wedged
    }

    /// Send a single-buffer control command (already serialized to `req` bytes)
    /// and wait for the device's ctrl-header response. Reuses the scratch page
    /// (request in the low half, response in the high half). The used-ring wait is
    /// bounded ([`Self::wait_can_pop_bounded`]) so a wedged host fails this command
    /// rather than spinning the (DISPATCH-level) caller forever.
    fn ctrl_roundtrip(&mut self, req: &[u8]) -> Result<(), VirtioError> {
        if self.wedged {
            return Err(VirtioError::DeviceError);
        }
        // Record the command type for the PASSIVE-level breadcrumb (written after
        // the spinlock drops): the ctrl header's `type_` is its first LE u32.
        self.diag_last_cmd = if req.len() >= 4 {
            u32::from_le_bytes([req[0], req[1], req[2], req[3]])
        } else {
            0
        };
        let resp_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
        // SAFETY: owned contiguous page; disjoint req/resp halves, serialized by
        // the caller's spinlock. Raw pointer (not a &mut borrow of self.scratch)
        // so self.control/transport can be borrowed for the round-trip.
        let buf = unsafe {
            core::slice::from_raw_parts_mut(self.scratch.as_slice().as_ptr() as *mut u8, SCRATCH_BYTES)
        };
        let (req_buf, resp_buf) = buf.split_at_mut(SCRATCH_BYTES / 2);
        if req.len() > req_buf.len() || resp_len > resp_buf.len() {
            return Err(VirtioError::DeviceError);
        }
        req_buf[..req.len()].copy_from_slice(req);
        // Manual add → notify → bounded poll → pop (the crate's
        // `add_notify_wait_pop` is an UNBOUNDED spin). If the wait times out we
        // leave the chain queued and latch `wedged`: the device was reset on Drop
        // before `scratch` is freed, so it never DMAs freed memory, and `wedged`
        // stops any further command from reusing the now-desynced queue.
        // SAFETY: `req_buf`/`resp_buf` alias `scratch` (alive for the transport's
        // lifetime) via the raw pointer above, not a borrow of `self`; they remain
        // valid through the matching `pop_used` and are not otherwise accessed.
        let token = match unsafe {
            self.control
                .add(&[&req_buf[..req.len()]], &mut [&mut resp_buf[..resp_len]])
        } {
            Ok(t) => t,
            Err(_) => {
                self.wedged = true;
                return Err(VirtioError::DeviceError);
            }
        };
        if self.control.should_notify() {
            self.transport.notify(CTRL_QUEUE);
        }
        self.wait_can_pop_bounded()?;
        // SAFETY: same buffers passed to `add`; still valid (see above).
        unsafe {
            self.control
                .pop_used(token, &[&req_buf[..req.len()]], &mut [&mut resp_buf[..resp_len]])
        }
        .map_err(|_| VirtioError::DeviceError)?;
        let resp: &VirtioGpuCtrlHdr = bytemuck::from_bytes(&resp_buf[..resp_len]);
        if resp_is_ok(resp.type_) {
            Ok(())
        } else {
            Err(VirtioError::DeviceError)
        }
    }
}

impl Drop for VirtioGpu {
    fn drop(&mut self) {
        // Quiesce the device (resets queues) so it stops touching the rings we
        // are about to free.
        self.transport.set_status(DeviceStatus::empty());
        // The `scratch` DmaBuffer frees its contiguous page on its own drop, the
        // control `VirtQueue` frees its ring memory on its drop (via
        // `Hal::dma_dealloc`), and the `inflight` Vec drops every parked
        // `InFlight`'s buffers. The device was reset above (set_status empty), so
        // it no longer DMAs those pages. `VirtioGpu::drop` runs at PASSIVE_LEVEL
        // (set_virtio drops the old transport outside the spinlock), as the
        // contiguous-memory free requires.
        //
        // The BAR MMIO mappings made inside `PciTransport` are intentionally NOT
        // freed here: `WdkHal` caches them by physical address and reuses them on
        // the next PrepareHardware (the BARs are stable across stop/start), so
        // there is no per-cycle leak. The cache is released wholesale in
        // `EvtDriverUnload` via `WdkHal::unmap_all`.
    }
}
