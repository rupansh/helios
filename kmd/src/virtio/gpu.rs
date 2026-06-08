//! The virtio-gpu device object, built on the `virtio-drivers` PCI transport.
//!
//! `VirtioGpu` owns the `PciTransport` (discovers/maps the virtio config
//! regions), the control `VirtQueue`, and a contiguous DMA scratch page, and
//! layers the virtio-gpu command protocol (`helios_protocol`) on top. Built by
//! `init` from `evt_device_prepare_hardware` and stored in
//! `AdapterContext::virtio`.
//!
//! Bring-up (all in `init`, at PASSIVE_LEVEL):
//!   M1 — `KmdfConfigAccess` (over BUS_INTERFACE_STANDARD) → `PciRoot` →
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
use helios_protocol::{
    resp_is_ok, VirtioGpuCmdSubmit, VirtioGpuCtrlHdr, VirtioGpuCtxCreate, VirtioGpuCtxDestroy,
    VirtioGpuCtxResource, VirtioGpuRect, VirtioGpuResourceCreateBlob, VirtioGpuResourceFlush,
    VirtioGpuResourceMapBlob, VirtioGpuResourceUnmapBlob, VirtioGpuResourceUnref,
    VirtioGpuRespDisplayInfo, VirtioGpuRespMapInfo, VirtioGpuSetScanoutBlob,
    HELIOS_OPTIONAL_FEATURES, HELIOS_REQUIRED_FEATURES, VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE,
    VIRTIO_GPU_CMD_CTX_CREATE, VIRTIO_GPU_CMD_CTX_DESTROY, VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE,
    VIRTIO_GPU_CMD_GET_DISPLAY_INFO, VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB,
    VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB,
    VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB, VIRTIO_GPU_CMD_RESOURCE_UNREF,
    VIRTIO_GPU_CMD_SET_SCANOUT_BLOB, VIRTIO_GPU_CMD_SUBMIT_3D, VIRTIO_GPU_FLAG_FENCE,
    VIRTIO_GPU_FLAG_INFO_RING_IDX, VIRTIO_GPU_MAP_CACHE_MASK, VIRTIO_GPU_SHM_ID_HOST_VISIBLE,
    VIRTIO_PCI_CAP_SHARED_MEMORY_CFG,
};
use virtio_drivers::queue::VirtQueue;
use virtio_drivers::transport::pci::bus::{DeviceFunction, PciRoot};
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::{DeviceStatus, Transport};
use virtio_drivers::Error as VirtError;

use super::config::KmdfConfigAccess;
use super::hal::{DmaBuffer, WdkHal};
use super::VirtioError;

/// Control queue index (virtio-gpu controlq = 0; cursorq = 1 is unused).
const CTRL_QUEUE: u16 = 0;
/// Control-queue ring size — power of two, conservatively ≤ the device's max.
const CTRL_QUEUE_SIZE: usize = 64;
/// One page of contiguous DMA scratch, split into request/response halves.
const SCRATCH_BYTES: usize = 4096;

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
fn scan_host_visible_window(access: &KmdfConfigAccess) -> Option<HostVisibleWindow> {
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
                let off = access.read32(cap + 8) as u64 | ((access.read32(cap + 16) as u64) << 32);
                let len = access.read32(cap + 12) as u64 | ((access.read32(cap + 20) as u64) << 32);
                let base = bar_base(access, bar)?;
                return Some(HostVisibleWindow {
                    base: base + off,
                    len,
                });
            }
        }
        cap = cap_next;
    }
    None
}

/// Read the guest-physical base a memory BAR was assigned, handling the 64-bit
/// (type 0b10) layout the prefetchable host-visible window uses.
fn bar_base(access: &KmdfConfigAccess, bar: u16) -> Option<u64> {
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
const MAX_CONTEXTS: usize = 64;
const MAX_WINDOW_RANGES: usize = MAX_BLOBS;

#[derive(Clone, Copy)]
struct ContextSlot {
    ctx_id: u32,
    owner: usize,
}

struct ContextTable {
    slots: Vec<ContextSlot>,
}

impl ContextTable {
    fn with_reserved_capacity() -> Self {
        Self {
            slots: Vec::with_capacity(MAX_CONTEXTS),
        }
    }

    fn insert(&mut self, ctx_id: u32, owner: usize) -> Result<(), VirtioError> {
        if self.slots.len() >= MAX_CONTEXTS {
            return Err(VirtioError::OutOfMemory);
        }
        self.slots.push(ContextSlot { ctx_id, owner });
        Ok(())
    }

    fn can_insert(&self) -> bool {
        self.slots.len() < MAX_CONTEXTS
    }

    fn remove(&mut self, ctx_id: u32) {
        if let Some(idx) = self.slots.iter().position(|s| s.ctx_id == ctx_id) {
            self.slots.swap_remove(idx);
        }
    }

    fn take_one_for_owner(&mut self, owner: usize) -> Option<u32> {
        let idx = self.slots.iter().position(|s| s.owner == owner)?;
        Some(self.slots.swap_remove(idx).ctx_id)
    }
}

/// One tracked blob resource. Phase 4c will add `window_offset`/`user_va`/`mdl`
/// when the blob is mapped.
#[derive(Clone, Copy)]
struct BlobSlot {
    ctx_id: u32,
    resource_id: u32,
    /// Blob size in bytes (from ALLOC_BLOB; MAP_BLOB needs it to size the MDL).
    size: u64,
    /// RESOURCE_MAP_BLOB succeeded and should be paired with RESOURCE_UNMAP_BLOB.
    mapped: bool,
    /// Host-visible window offset used for RESOURCE_MAP_BLOB.
    map_offset: u64,
    /// Rounded mapped length in the host-visible window.
    map_len: u64,
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
    fn insert(&mut self, ctx_id: u32, resource_id: u32, size: u64) -> Result<(), VirtioError> {
        if self.slots.len() >= MAX_BLOBS {
            return Err(VirtioError::OutOfMemory);
        }
        self.slots.push(BlobSlot {
            ctx_id,
            resource_id,
            size,
            mapped: false,
            map_offset: 0,
            map_len: 0,
        });
        Ok(())
    }

    /// Remove and return one blob owned by `ctx_id`.
    fn take_one_for_ctx(&mut self, ctx_id: u32) -> Option<BlobSlot> {
        let idx = self.slots.iter().position(|s| s.ctx_id == ctx_id)?;
        Some(self.slots.swap_remove(idx))
    }

    /// Remove and return `resource_id`, if it belongs to `ctx_id`.
    fn take(&mut self, ctx_id: u32, resource_id: u32) -> Option<BlobSlot> {
        let idx = self
            .slots
            .iter()
            .position(|s| s.ctx_id == ctx_id && s.resource_id == resource_id)?;
        Some(self.slots.swap_remove(idx))
    }

    /// Look up one blob owned by `ctx_id`.
    fn get(&self, ctx_id: u32, resource_id: u32) -> Option<BlobSlot> {
        self.slots
            .iter()
            .find(|s| s.ctx_id == ctx_id && s.resource_id == resource_id)
            .copied()
    }

    /// Look up a tracked blob's size (None if `resource_id` is unknown).
    fn size_of(&self, resource_id: u32) -> Option<u64> {
        self.slots
            .iter()
            .find(|s| s.resource_id == resource_id)
            .map(|s| s.size)
    }

    /// Mark a tracked blob as mapped into the host-visible window.
    fn mark_mapped(
        &mut self,
        resource_id: u32,
        map_offset: u64,
        map_len: u64,
    ) -> Result<(), VirtioError> {
        let slot = self
            .slots
            .iter_mut()
            .find(|s| s.resource_id == resource_id)
            .ok_or(VirtioError::DeviceError)?;
        slot.mapped = true;
        slot.map_offset = map_offset;
        slot.map_len = map_len;
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct WindowRange {
    offset: u64,
    len: u64,
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
    /// Tracks live Venus contexts by owning file object. `EvtFileCleanup` uses
    /// this to destroy contexts if the client exits before issuing CTX_DESTROY.
    contexts: ContextTable,
    /// High-water mark for host-visible window offsets. Freed ranges are reused
    /// through `free_window_ranges` before this grows.
    next_window_offset: u64,
    /// Free ranges in the host-visible window. Capacity is reserved at init so
    /// release/map bookkeeping does not allocate under the AdapterContext lock.
    free_window_ranges: Vec<WindowRange>,
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
}

impl VirtioGpu {
    /// Bring the virtio-gpu device online and prove it with `GET_DISPLAY_INFO`.
    pub fn init(access: &KmdfConfigAccess) -> Result<Self, VirtioError> {
        // ── M1: discover the device + map BARs through the bus interface ────
        // A function driver doesn't own the bus, so config space is reached via
        // the PCI bus's BUS_INTERFACE_STANDARD (GetBusData/SetBusData); the
        // DeviceFunction is a formality (KmdfConfigAccess ignores it and
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

        // ── M2: feature negotiation (VirtIO 1.2 spec §3.1.1) ────────────────
        transport.set_status(DeviceStatus::empty()); // reset
        let mut spins = 0u32;
        while !transport.get_status().is_empty() && spins < 100_000 {
            spins += 1;
        }
        transport.set_status(DeviceStatus::ACKNOWLEDGE);
        transport.set_status(DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER);

        let offered = transport.read_device_features();
        let accepted = offered & (HELIOS_REQUIRED_FEATURES | HELIOS_OPTIONAL_FEATURES);
        transport.write_driver_features(accepted);
        transport.set_status(
            DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER | DeviceStatus::FEATURES_OK,
        );
        if !transport.get_status().contains(DeviceStatus::FEATURES_OK)
            || accepted & HELIOS_REQUIRED_FEATURES != HELIOS_REQUIRED_FEATURES
        {
            transport.set_status(DeviceStatus::FAILED);
            return Err(VirtioError::FeatureRejected);
        }

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

        control
            .add_notify_wait_pop(
                &[&req_buf[..hdr_len]],
                &mut [&mut resp_buf[..resp_len]],
                &mut transport,
            )
            .map_err(|_| VirtioError::DeviceError)?;

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

        Ok(Self {
            transport,
            control,
            scratch,
            next_ctx_id: AtomicU32::new(1),
            next_resource_id: AtomicU32::new(RESOURCE_ID_BASE),
            host_visible,
            blobs: BlobTable::with_reserved_capacity(),
            contexts: ContextTable::with_reserved_capacity(),
            next_window_offset: 0,
            free_window_ranges: Vec::with_capacity(MAX_WINDOW_RANGES),
            inflight: Vec::with_capacity(MAX_INFLIGHT),
            max_submitted_fence_id: 0,
        })
    }

    /// The host-visible blob window discovered at init, if any. `MAP_BLOB` uses
    /// it to translate a resource's window offset into a guest-physical range.
    pub fn host_visible(&self) -> Option<HostVisibleWindow> {
        self.host_visible
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
        owner: usize,
        retired: &mut Vec<InFlight>,
    ) -> Result<u32, VirtioError> {
        self.quiesce_into(retired)?;
        if !self.contexts.can_insert() {
            return Err(VirtioError::OutOfMemory);
        }

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
        self.contexts.insert(ctx_id, owner)?;
        Ok(ctx_id)
    }

    /// Destroy a previously created 3D context.
    pub fn ctx_destroy(
        &mut self,
        ctx_id: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        self.contexts.remove(ctx_id);
        self.destroy_ctx_resources(ctx_id)?;

        let mut cmd = VirtioGpuCtxDestroy::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DESTROY;
        cmd.hdr.ctx_id = ctx_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
    }

    pub fn ctx_destroy_for_owner(
        &mut self,
        owner: usize,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        while let Some(ctx_id) = self.contexts.take_one_for_owner(owner) {
            self.destroy_ctx_resources(ctx_id)?;

            let mut cmd = VirtioGpuCtxDestroy::zeroed();
            cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DESTROY;
            cmd.hdr.ctx_id = ctx_id;
            self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))?;
        }

        Ok(())
    }

    fn destroy_ctx_resources(&mut self, ctx_id: u32) -> Result<(), VirtioError> {
        while let Some(slot) = self.blobs.take_one_for_ctx(ctx_id) {
            self.release_blob_slot(slot)?;
        }

        Ok(())
    }

    /// Explicitly release one blob while the process is alive. This closes the
    /// HOST3D resource leak that otherwise persisted until CTX_DESTROY.
    pub fn release_blob(
        &mut self,
        ctx_id: u32,
        resource_id: u32,
        retired: &mut Vec<InFlight>,
    ) -> Result<(), VirtioError> {
        self.quiesce_into(retired)?;
        let Some(slot) = self.blobs.get(ctx_id, resource_id) else {
            return Ok(());
        };
        self.release_blob_slot(slot)?;
        self.blobs.take(ctx_id, resource_id);
        Ok(())
    }

    fn release_blob_slot(&mut self, slot: BlobSlot) -> Result<(), VirtioError> {
        if slot.mapped {
            let mut unmap = VirtioGpuResourceUnmapBlob::zeroed();
            unmap.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB;
            unmap.resource_id = slot.resource_id;
            self.ctrl_roundtrip(bytemuck::bytes_of(&unmap))?;
            self.free_window_range(slot.map_offset, slot.map_len);
        }

        let mut detach = VirtioGpuCtxResource::zeroed();
        detach.hdr.type_ = VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE;
        detach.hdr.ctx_id = slot.ctx_id;
        detach.resource_id = slot.resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&detach))?;

        let mut unref = VirtioGpuResourceUnref::zeroed();
        unref.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_UNREF;
        unref.resource_id = slot.resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&unref))
    }

    fn alloc_window_range(&mut self, len: u64, window_len: u64) -> Result<u64, VirtioError> {
        if let Some(idx) = self.free_window_ranges.iter().position(|r| r.len >= len) {
            let offset = self.free_window_ranges[idx].offset;
            if self.free_window_ranges[idx].len == len {
                self.free_window_ranges.swap_remove(idx);
            } else {
                self.free_window_ranges[idx].offset += len;
                self.free_window_ranges[idx].len -= len;
            }
            return Ok(offset);
        }

        let offset = self.next_window_offset;
        let end = offset.checked_add(len).ok_or(VirtioError::OutOfMemory)?;
        if end > window_len {
            return Err(VirtioError::OutOfMemory);
        }

        self.next_window_offset = end;
        Ok(offset)
    }

    fn free_window_range(&mut self, offset: u64, len: u64) {
        if len == 0 {
            return;
        }

        if offset.checked_add(len) == Some(self.next_window_offset) {
            self.next_window_offset = offset;
            while let Some(idx) = self
                .free_window_ranges
                .iter()
                .position(|r| r.offset.checked_add(r.len) == Some(self.next_window_offset))
            {
                let r = self.free_window_ranges.swap_remove(idx);
                self.next_window_offset = r.offset;
            }
            return;
        }

        for range in &mut self.free_window_ranges {
            if range.offset.checked_add(range.len) == Some(offset) {
                range.len += len;
                return;
            }
            if offset.checked_add(len) == Some(range.offset) {
                range.offset = offset;
                range.len += len;
                return;
            }
        }

        if self.free_window_ranges.len() < MAX_WINDOW_RANGES {
            self.free_window_ranges.push(WindowRange { offset, len });
        }
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
        self.blobs.insert(ctx_id, resource_id, size)?;
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
        cmd.r = VirtioGpuRect {
            x: 0,
            y: 0,
            width,
            height,
        };
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
        cmd.r = VirtioGpuRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        cmd.resource_id = resource_id;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))
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
                    while !self.control.can_pop() {
                        core::hint::spin_loop();
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
        while !self.inflight.is_empty() {
            while !self.control.can_pop() {
                core::hint::spin_loop();
            }
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
        let size = self
            .blobs
            .size_of(resource_id)
            .ok_or(VirtioError::DeviceError)?;
        let map_len = round_up_page(size);
        if map_len == 0 || map_len > MAX_BLOB_MAP_BYTES {
            return Err(VirtioError::DeviceError);
        }
        let offset = self.alloc_window_range(map_len, window.len)?;

        // Host round-trip before recording the blob as mapped. If the host rejects
        // the map, return the reserved aperture range for later reuse.
        let mut cmd = VirtioGpuResourceMapBlob::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB;
        cmd.resource_id = resource_id;
        cmd.offset = offset;
        let map_info = match self.map_blob_roundtrip(&cmd) {
            Ok(map_info) => map_info,
            Err(e) => {
                self.free_window_range(offset, map_len);
                return Err(e);
            }
        };

        self.blobs.mark_mapped(resource_id, offset, map_len)?;
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
        req_buf[..req.len()].copy_from_slice(req);
        self.control
            .add_notify_wait_pop(
                &[&req_buf[..req.len()]],
                &mut [&mut resp_buf[..resp_len]],
                &mut self.transport,
            )
            .map_err(|_| VirtioError::DeviceError)?;
        let resp: &VirtioGpuRespMapInfo = bytemuck::from_bytes(&resp_buf[..resp_len]);
        if resp_is_ok(resp.hdr.type_) {
            Ok(resp.map_info)
        } else {
            Err(VirtioError::DeviceError)
        }
    }

    /// Send a single-buffer control command (already serialized to `req` bytes)
    /// and wait for the device's ctrl-header response. Reuses the scratch page
    /// (request in the low half, response in the high half).
    fn ctrl_roundtrip(&mut self, req: &[u8]) -> Result<(), VirtioError> {
        let resp_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
        // SAFETY: owned contiguous page; disjoint req/resp halves, serialized by
        // the caller's spinlock. Raw pointer (not a &mut borrow of self.scratch)
        // so self.control/transport can be borrowed for the round-trip.
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
        req_buf[..req.len()].copy_from_slice(req);
        self.control
            .add_notify_wait_pop(
                &[&req_buf[..req.len()]],
                &mut [&mut resp_buf[..resp_len]],
                &mut self.transport,
            )
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
