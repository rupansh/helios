//! Adapter context — one per virtio-gpu device the driver binds to.
//!
//! Allocated in `DxgkDdiAddDevice`, populated in `DxgkDdiStartDevice`, freed in
//! `DxgkDdiRemoveDevice`. Dxgkrnl hands this back to us as the opaque
//! `MiniportDeviceContext` in every subsequent DDI call.
//!
//! Phase-7 display pivot (DISPLAY.md §3): recovered from git
//! `658168f:kmd/src/adapter.rs` (the WDDM adapter shape) — it replaces the
//! System-class KMDF device-context-on-the-WDFDEVICE model. The 2D scanout /
//! present state lives inside [`VirtioGpu`] (gpu.rs), reached under the same
//! `virtio_lock`, so this context stays focused on the transport + the saved
//! Dxgkrnl callback interface.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use wdk_sys::ntddk::{KeAcquireSpinLockRaiseToDpc, KeReleaseSpinLock};
use wdk_sys::KSPIN_LOCK;

use crate::dxgk::*;
use crate::error::DriverError;
use crate::virtio::VirtioGpu;

pub struct AdapterContext {
    /// Physical device object for the virtio-gpu device.
    pub pdo: PDEVICE_OBJECT,
    /// Dxgkrnl callback interface, saved in StartDevice. `None` until then.
    /// Written once during the (serialized) StartDevice lifecycle DDI.
    pub dxgkrnl: Option<DXGKRNL_INTERFACE>,
    /// Serializes ALL access to `virtio` (the control virtqueue + the shared
    /// scratch page). Held by present/escape submissions at PASSIVE_LEVEL and,
    /// from 7.2, by the used-ring DPC at DISPATCH_LEVEL — a spinlock (not a mutex)
    /// is mandatory because the DPC path cannot block. `0` is the initialized +
    /// unlocked state of a `KSPIN_LOCK`, so no explicit `KeInitializeSpinLock` is
    /// required (same rationale as the BAR-mapping cache in `virtio::hal`).
    virtio_lock: UnsafeCell<KSPIN_LOCK>,
    /// The virtio-gpu transport, brought up in `DxgkDdiStartDevice`.
    /// Guarded by `virtio_lock`; `None` until StartDevice (and after StopDevice).
    virtio: UnsafeCell<Option<VirtioGpu>>,
    /// Kernel VA of the virtio `VIRTIO_PCI_ISR` status register (0 = none/down).
    /// Read+ack'd LOCK-FREE from `DxgkDdiInterruptRoutine` at DIRQL — it cannot take
    /// `virtio_lock` (a DISPATCH-level spinlock), so the ISR register VA lives here
    /// as a plain atomic, set in StartDevice after the transport is up and cleared in
    /// StopDevice. Reading the register de-asserts the virtio INTx line; without it
    /// an asserted (e.g. config-change) interrupt re-fires forever — an interrupt
    /// storm that hard-hangs the guest (our ISR previously never acked).
    pub isr_status_va: AtomicUsize,
    /// VioGpuDod-style per-source current-mode flags. The scanout can exist from
    /// StartDevice, but the VidPN source is not considered active until
    /// CommitVidPn realizes the committed source mode.
    current_mode_flags: AtomicU32,
    current_rotation: AtomicU32,
}

const CM_FRAMEBUFFER_ACTIVE: u32 = 1 << 0;
const CM_SOURCE_NOT_VISIBLE: u32 = 1 << 1;
const CM_FULLSCREEN_PRESENT: u32 = 1 << 2;

// SAFETY: `dxgkrnl` is written only during the device-lifecycle DDIs, which
// Dxgkrnl serializes. `virtio` is interior-mutable but every access goes through
// `virtio_lock` (a kernel spinlock) via `with_virtio`/`set_virtio`, so concurrent
// present/DPC callers never alias it.
unsafe impl Send for AdapterContext {}
unsafe impl Sync for AdapterContext {}

impl AdapterContext {
    pub fn new(pdo: PDEVICE_OBJECT) -> Result<Self, DriverError> {
        Ok(Self {
            pdo,
            dxgkrnl: None,
            virtio_lock: UnsafeCell::new(0),
            virtio: UnsafeCell::new(None),
            isr_status_va: AtomicUsize::new(0),
            current_mode_flags: AtomicU32::new(CM_SOURCE_NOT_VISIBLE),
            current_rotation: AtomicU32::new(
                _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_IDENTITY as u32,
            ),
        })
    }

    /// Borrow the Dxgkrnl interface, or fail if StartDevice has not run yet.
    pub fn dxgkrnl(&self) -> Result<&DXGKRNL_INTERFACE, DriverError> {
        self.dxgkrnl.as_ref().ok_or(DriverError::DeviceNotFound)
    }

    /// Install (or clear) the virtio transport under the lock.
    ///
    /// The previous transport, if any, is dropped *after* the lock is released:
    /// `VirtioGpu::drop` resets the device and frees contiguous memory, both of
    /// which are PASSIVE_LEVEL-only — they must not run at the DISPATCH_LEVEL the
    /// spinlock raises to. MUST be called at PASSIVE_LEVEL (StartDevice /
    /// StopDevice, which Dxgkrnl serializes).
    pub fn set_virtio(&self, new: Option<VirtioGpu>) {
        // SAFETY: `virtio_lock` is a valid KSPIN_LOCK; the critical section only
        // swaps the Option in/out of the cell (no allocation, no device I/O).
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.virtio_lock.get()) };
        let old = core::mem::replace(unsafe { &mut *self.virtio.get() }, new);
        unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };
        // Dropped here, at PASSIVE_LEVEL, outside the lock.
        drop(old);
    }

    /// Run `f` against the live virtio transport while holding `virtio_lock`.
    ///
    /// Returns `DeviceNotFound` if the transport is not currently up. `f` runs at
    /// DISPATCH_LEVEL (spinlock held): it must not allocate or call pageable code.
    /// Stage any payload (e.g. a Venus stream) into a `DmaBuffer` *before* calling
    /// this, then pass a slice of it into `f`.
    pub fn with_virtio<R>(&self, f: impl FnOnce(&mut VirtioGpu) -> R) -> Result<R, DriverError> {
        // SAFETY: spinlock-guarded exclusive access to the cell's contents for the
        // duration of the critical section.
        let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.virtio_lock.get()) };
        let result = match unsafe { &mut *self.virtio.get() } {
            Some(v) => Ok(f(v)),
            None => Err(DriverError::DeviceNotFound),
        };
        unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };
        result
    }

    pub fn reset_current_mode(&self) {
        self.current_mode_flags
            .store(CM_SOURCE_NOT_VISIBLE, Ordering::Release);
        self.current_rotation.store(
            _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_IDENTITY as u32,
            Ordering::Release,
        );
    }

    pub fn mark_framebuffer_active(&self, active: bool) {
        if active {
            self.current_mode_flags.fetch_or(
                CM_FRAMEBUFFER_ACTIVE | CM_FULLSCREEN_PRESENT,
                Ordering::AcqRel,
            );
        } else {
            self.current_mode_flags
                .fetch_and(!CM_FRAMEBUFFER_ACTIVE, Ordering::AcqRel);
        }
    }

    pub fn mark_fullscreen_present(&self) {
        self.current_mode_flags
            .fetch_or(CM_FULLSCREEN_PRESENT, Ordering::AcqRel);
    }

    pub fn set_source_visible(&self, visible: bool) {
        if visible {
            self.current_mode_flags.fetch_and(!CM_SOURCE_NOT_VISIBLE, Ordering::AcqRel);
            self.current_mode_flags
                .fetch_or(CM_FULLSCREEN_PRESENT, Ordering::AcqRel);
        } else {
            self.current_mode_flags
                .fetch_or(CM_SOURCE_NOT_VISIBLE, Ordering::AcqRel);
        }
    }

    pub fn source_not_visible(&self) -> bool {
        self.current_mode_flags.load(Ordering::Acquire) & CM_SOURCE_NOT_VISIBLE != 0
    }

    pub fn framebuffer_active(&self) -> bool {
        self.current_mode_flags.load(Ordering::Acquire) & CM_FRAMEBUFFER_ACTIVE != 0
    }

    pub fn set_rotation(&self, rotation: _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::Type) {
        self.current_rotation
            .store(rotation as u32, Ordering::Release);
    }

    pub fn rotation(&self) -> _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::Type {
        match self.current_rotation.load(Ordering::Acquire) {
            x if x == _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_ROTATE90 as u32 => {
                _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_ROTATE90
            }
            _ => _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_IDENTITY,
        }
    }
}
