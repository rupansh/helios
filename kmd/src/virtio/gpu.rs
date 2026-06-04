//! The virtio-gpu device object, built on the `virtio-drivers` PCI transport.
//!
//! `VirtioGpu` owns the `PciTransport` (discovers/maps the virtio config
//! regions), the control `VirtQueue`, and a contiguous DMA scratch page, and
//! layers the virtio-gpu command protocol (`helios_protocol`) on top. Built by
//! `init` from `DxgkDdiStartDevice` and stored in `AdapterContext::virtio`.
//!
//! Bring-up (all in `init`, at PASSIVE_LEVEL):
//!   M1 — `DxgkConfigAccess` → `PciRoot` → `PciTransport::new::<WdkHal,_>`
//!   M2 — feature negotiation via the `Transport` trait
//!   M3 — control `VirtQueue::<WdkHal>` setup + DRIVER_OK
//!   M4 — `GET_DISPLAY_INFO` polled round-trip (Phase-2 smoke test)

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use bytemuck::Zeroable;
use helios_protocol::{
    resp_is_ok, VirtioGpuCmdSubmit, VirtioGpuCtrlHdr, VirtioGpuCtxCreate, VirtioGpuCtxDestroy,
    VirtioGpuRespDisplayInfo, HELIOS_OPTIONAL_FEATURES, HELIOS_REQUIRED_FEATURES,
    VIRTIO_GPU_CMD_CTX_CREATE, VIRTIO_GPU_CMD_CTX_DESTROY, VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
    VIRTIO_GPU_CMD_SUBMIT_3D, VIRTIO_GPU_FLAG_FENCE,
};
use virtio_drivers::queue::VirtQueue;
use virtio_drivers::transport::pci::bus::{DeviceFunction, PciRoot};
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::{DeviceStatus, Transport};
use virtio_drivers::{BufferDirection, Hal};

use super::config::DxgkConfigAccess;
use super::hal::WdkHal;
use super::VirtioError;
use crate::dxgk::DXGKRNL_INTERFACE;

/// Control queue index (virtio-gpu controlq = 0; cursorq = 1 is unused).
const CTRL_QUEUE: u16 = 0;
/// Control-queue ring size — power of two, conservatively ≤ the device's max.
const CTRL_QUEUE_SIZE: usize = 64;
/// One page of contiguous DMA scratch, split into request/response halves.
const SCRATCH_BYTES: usize = 4096;

/// An initialized virtio-gpu transport.
pub struct VirtioGpu {
    /// The virtio-modern PCI transport (owns the mapped cfg-region VAs).
    transport: PciTransport,
    /// Control virtqueue (queue 0) — all GPU commands ride this.
    control: VirtQueue<WdkHal, CTRL_QUEUE_SIZE>,
    /// Contiguous DMA scratch page for synchronous command buffers. Freed in
    /// teardown (M6).
    scratch: NonNull<u8>,
    /// Next virtio-gpu 3D context id to hand out (guest-assigned; 0 is the
    /// reserved global context, so we start at 1). Phase 3.
    next_ctx_id: AtomicU32,
    /// Next virtio-gpu resource id to hand out (0 is reserved). Phase 3 (M3.5).
    next_resource_id: AtomicU32,
}

impl VirtioGpu {
    /// Bring the virtio-gpu device online and prove it with `GET_DISPLAY_INFO`.
    pub fn init(dxgkrnl: &DXGKRNL_INTERFACE) -> Result<Self, VirtioError> {
        // ── M1: discover the device + map BARs through Dxgkrnl ──────────────
        // A miniport doesn't own the bus, so config space is reached via the
        // Dxgkrnl callbacks; the DeviceFunction is a formality (DxgkConfigAccess
        // ignores it and addresses our own device via the DeviceHandle).
        let access = DxgkConfigAccess::new(dxgkrnl);
        let mut root = PciRoot::new(access);
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
        // the device, response is written by it.
        let (scratch_pa, scratch) = WdkHal::dma_alloc(1, BufferDirection::Both);
        if scratch_pa == 0 {
            // dma_alloc signals failure with a zero physaddr + dangling ptr;
            // bail rather than write into the dangling page.
            return Err(VirtioError::OutOfMemory);
        }
        // SAFETY: `scratch` is a freshly-allocated, owned, contiguous page.
        let buf = unsafe { core::slice::from_raw_parts_mut(scratch.as_ptr(), SCRATCH_BYTES) };
        let (req_buf, resp_buf) = buf.split_at_mut(SCRATCH_BYTES / 2);

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

        Ok(Self {
            transport,
            control,
            scratch,
            next_ctx_id: AtomicU32::new(1),
            next_resource_id: AtomicU32::new(1),
        })
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
        // Disjoint halves; serialized by the caller's spinlock.
        let buf = unsafe { core::slice::from_raw_parts_mut(self.scratch.as_ptr(), SCRATCH_BYTES) };
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
        // the caller's spinlock.
        let buf = unsafe { core::slice::from_raw_parts_mut(self.scratch.as_ptr(), SCRATCH_BYTES) };
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
        // Free the DMA scratch page. The control `VirtQueue` frees its own ring
        // memory on its own drop (via `Hal::dma_dealloc`).
        //
        // The BAR MMIO mappings made inside `PciTransport` are intentionally NOT
        // freed here: `WdkHal` caches them by physical address and reuses them on
        // the next StartDevice (the BARs are stable across stop/start), so there
        // is no per-cycle leak. The cache is released wholesale in
        // `DxgkDdiUnload` via `WdkHal::unmap_all`.
        //
        // SAFETY: `scratch` was returned by `WdkHal::dma_alloc(1, ..)` in `init`
        // and is freed exactly once (here, when the VirtioGpu is dropped).
        unsafe { WdkHal::dma_dealloc(0, self.scratch, 1) };
    }
}
