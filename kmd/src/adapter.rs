//! Adapter context — one per virtio-gpu device the driver binds to.
//!
//! Allocated in `DxgkDdiAddDevice`, populated in `DxgkDdiStartDevice`, freed in
//! `DxgkDdiRemoveDevice`. Dxgkrnl hands this back to us as the opaque
//! `MiniportDeviceContext` in every subsequent DDI call.

use crate::dxgk::*;
use crate::error::DriverError;

pub struct AdapterContext {
    /// Physical device object for the virtio-gpu device.
    pub pdo: PDEVICE_OBJECT,
    /// Dxgkrnl callback interface, saved in StartDevice. `None` until then.
    pub dxgkrnl: Option<DXGKRNL_INTERFACE>,
    /// Monotonic fence sequence counter (used once we submit work).
    pub fence_seq: u64,
    // Phase 2 (task #4) adds: `virtio: Option<crate::virtio::VirtioGpu>`.
}

// SAFETY: Dxgkrnl serializes device-lifecycle DDIs, so the context is not
// accessed concurrently during init/teardown. Once we add the virtqueue hot
// path (Phase 2+) we will guard shared state with a spinlock; the ISR path will
// only touch ISR-safe fields.
unsafe impl Send for AdapterContext {}
unsafe impl Sync for AdapterContext {}

impl AdapterContext {
    pub fn new(pdo: PDEVICE_OBJECT) -> Result<Self, DriverError> {
        Ok(Self {
            pdo,
            dxgkrnl: None,
            fence_seq: 0,
        })
    }

    /// Borrow the Dxgkrnl interface, or fail if StartDevice has not run yet.
    pub fn dxgkrnl(&self) -> Result<&DXGKRNL_INTERFACE, DriverError> {
        self.dxgkrnl.as_ref().ok_or(DriverError::DeviceNotFound)
    }
}
