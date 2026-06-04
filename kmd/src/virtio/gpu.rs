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
    VirtioGpuResourceCreateBlob, VirtioGpuRespDisplayInfo, VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB,
    HELIOS_OPTIONAL_FEATURES, HELIOS_REQUIRED_FEATURES,
    VIRTIO_GPU_CMD_CTX_CREATE, VIRTIO_GPU_CMD_CTX_DESTROY, VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
    VIRTIO_GPU_CMD_SUBMIT_3D, VIRTIO_GPU_FLAG_FENCE, VIRTIO_GPU_SHM_ID_HOST_VISIBLE,
    VIRTIO_PCI_CAP_SHARED_MEMORY_CFG,
};
use virtio_drivers::queue::VirtQueue;
use virtio_drivers::transport::pci::bus::{DeviceFunction, PciRoot};
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::{DeviceStatus, Transport};

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
    pub fn ctx_create(&mut self, capset_id: u32) -> Result<u32, VirtioError> {
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
    pub fn ctx_destroy(&mut self, ctx_id: u32) -> Result<(), VirtioError> {
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
    /// NOTE (Phase 5 protocol gap): real venus device-memory blobs need
    /// `blob_id` = the venus memory id; `HeliosEscapeAllocBlob` has no such field
    /// yet, so `blob_id` is 0 here. Fine for a standalone mappable scratch blob.
    pub fn alloc_blob(
        &mut self,
        ctx_id: u32,
        blob_mem: u32,
        blob_flags: u32,
        size: u64,
    ) -> Result<u32, VirtioError> {
        if size == 0 {
            return Err(VirtioError::DeviceError);
        }
        let resource_id = self.next_resource_id.fetch_add(1, Ordering::Relaxed);
        let mut cmd = VirtioGpuResourceCreateBlob::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB;
        cmd.hdr.ctx_id = ctx_id;
        cmd.resource_id = resource_id;
        cmd.blob_mem = blob_mem;
        cmd.blob_flags = blob_flags;
        cmd.nr_entries = 0;
        cmd.blob_id = 0;
        cmd.size = size;
        self.ctrl_roundtrip(bytemuck::bytes_of(&cmd))?;
        // Record only after the host accepted the create (so a failed create
        // doesn't occupy a slot). Done under the caller's spinlock — alloc-free.
        self.blobs.insert(resource_id, size)?;
        Ok(resource_id)
    }

    /// Submit an opaque Venus command stream to `ctx_id`, fenced with `fence_id`.
    ///
    /// `venus` MUST be physically contiguous (carve it from a [`DmaBuffer`]) — it
    /// rides a single device-readable descriptor. The command is fenced and this
    /// blocks (polled) until the device acknowledges it on the used ring, so by
    /// the time it returns the work is host-visible-complete (interim sync fence
    /// model; the async/KEVENT model lands in M3.4).
    pub fn submit_venus(
        &mut self,
        ctx_id: u32,
        fence_id: u64,
        venus: &[u8],
    ) -> Result<(), VirtioError> {
        if venus.is_empty() {
            return Err(VirtioError::DeviceError);
        }
        let mut cmd = VirtioGpuCmdSubmit::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_SUBMIT_3D;
        cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE;
        cmd.hdr.fence_id = fence_id;
        cmd.hdr.ctx_id = ctx_id;
        cmd.size = venus.len() as u32;

        let hdr_len = core::mem::size_of::<VirtioGpuCmdSubmit>();
        let resp_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
        // SAFETY: `scratch` is our owned contiguous page; the low half holds the
        // submit header (device-read), the high half the response (device-write).
        // Disjoint halves; serialized by the caller's spinlock. We take a raw
        // pointer (not a &mut borrow of self.scratch) so self.control/transport
        // can be borrowed for the round-trip below.
        let buf = unsafe {
            core::slice::from_raw_parts_mut(self.scratch.as_slice().as_ptr() as *mut u8, SCRATCH_BYTES)
        };
        let (hdr_buf, resp_buf) = buf.split_at_mut(SCRATCH_BYTES / 2);
        hdr_buf[..hdr_len].copy_from_slice(bytemuck::bytes_of(&cmd));

        // Two device-readable descriptors (submit header + Venus stream) and one
        // device-writable response descriptor (TRANSPORT §7 two-descriptor + resp).
        self.control
            .add_notify_wait_pop(
                &[&hdr_buf[..hdr_len], venus],
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

    /// Send a single-buffer control command (already serialized to `req` bytes)
    /// and wait for the device's ctrl-header response. Reuses the scratch page
    /// (request in the low half, response in the high half).
    fn ctrl_roundtrip(&mut self, req: &[u8]) -> Result<(), VirtioError> {
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
        // The `scratch` DmaBuffer frees its contiguous page on its own drop, and
        // the control `VirtQueue` frees its ring memory on its drop (via
        // `Hal::dma_dealloc`).
        //
        // The BAR MMIO mappings made inside `PciTransport` are intentionally NOT
        // freed here: `WdkHal` caches them by physical address and reuses them on
        // the next PrepareHardware (the BARs are stable across stop/start), so
        // there is no per-cycle leak. The cache is released wholesale in
        // `EvtDriverUnload` via `WdkHal::unmap_all`.
    }
}
