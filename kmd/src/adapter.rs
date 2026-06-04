//! Adapter context — one per virtio-gpu device the driver binds to.
//!
//! Under the System-class KMDF model the per-device state hangs off the WDF
//! device object. WDF stores a small POD [`DeviceContext`] *inline* in the
//! device object (the typed-context mechanism); that context holds a raw pointer
//! to a heap-`Box`ed [`AdapterContext`] which carries the real state (the virtio
//! transport behind a spinlock, and the fence table). Using a `Box` keeps Rust
//! construction/`Drop` ordinary — WDF would otherwise hand us a zeroed blob that
//! is unsound to interpret as a `Option<VirtioGpu>` and would never run `Drop`.
//! The `Box` is created in `evt_device_add` and freed in the device's
//! `EvtCleanupCallback` (see pnp.rs).

use core::cell::UnsafeCell;
use core::mem::size_of;

use alloc::boxed::Box;

use wdk_sys::ntddk::{KeAcquireSpinLockRaiseToDpc, KeReleaseSpinLock};
use wdk_sys::{
    call_unsafe_wdf_function_binding, KSPIN_LOCK, LPCSTR, PCWDF_OBJECT_CONTEXT_TYPE_INFO, ULONG,
    WDFOBJECT, WDF_OBJECT_CONTEXT_TYPE_INFO,
};

use crate::error::DriverError;
use crate::fence::FenceTable;
use crate::mapping::MappingTable;
use crate::virtio::VirtioGpu;

/// The WDF typed context stored inline in the device object. POD: a zeroed
/// instance (what WDF hands out before we initialize it) is a valid null
/// pointer, so there is no drop-of-uninitialized hazard. The real state lives
/// behind `adapter`.
#[repr(C)]
pub struct DeviceContext {
    /// Heap-`Box`ed [`AdapterContext`], or null before `evt_device_add` sets it.
    pub adapter: *mut AdapterContext,
}

/// Self-referential `WDF_OBJECT_CONTEXT_TYPE_INFO` for [`DeviceContext`] — the
/// Rust equivalent of `WDF_DECLARE_CONTEXT_TYPE`. `UniqueType` must point at this
/// very record (WDF uses the pointer as the context-type key, consistently at
/// registration and lookup). A `static` may legally reference its own address in
/// its initializer; the newtype wrapper supplies the `Sync` that the raw
/// pointers inside the WDK struct otherwise lack.
#[repr(transparent)]
struct ContextTypeInfo(WDF_OBJECT_CONTEXT_TYPE_INFO);
// SAFETY: the record is immutable after program load and only read by WDF; the
// raw pointers it holds (its own address + a static C string) are valid for the
// program's lifetime.
unsafe impl Sync for ContextTypeInfo {}

static DEVICE_CONTEXT_TYPE_INFO: ContextTypeInfo = ContextTypeInfo(WDF_OBJECT_CONTEXT_TYPE_INFO {
    Size: size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as ULONG,
    ContextName: b"HeliosDeviceContext\0".as_ptr() as LPCSTR,
    ContextSize: size_of::<DeviceContext>(),
    // Self-reference: the type-info record is its own unique key.
    UniqueType: &DEVICE_CONTEXT_TYPE_INFO.0 as PCWDF_OBJECT_CONTEXT_TYPE_INFO,
    EvtDriverGetUniqueContextType: None,
});

/// `PCWDF_OBJECT_CONTEXT_TYPE_INFO` for `WDF_OBJECT_ATTRIBUTES.ContextTypeInfo`
/// and for the typed-context worker lookup.
pub fn device_context_type_info() -> PCWDF_OBJECT_CONTEXT_TYPE_INFO {
    &DEVICE_CONTEXT_TYPE_INFO.0 as PCWDF_OBJECT_CONTEXT_TYPE_INFO
}

/// Raw pointer to the inline [`DeviceContext`] of a WDF object created with
/// `DEVICE_CONTEXT_TYPE_INFO` (i.e. the WDFDEVICE).
///
/// # Safety
/// `handle` must be such an object; otherwise the worker returns null.
unsafe fn device_context_ptr(handle: WDFOBJECT) -> *mut DeviceContext {
    // SAFETY: WdfObjectGetTypedContextWorker returns the inline context pointer
    // for the registered type; non-null for our device. The macro injects the
    // WDF globals + function-table dispatch.
    let p = call_unsafe_wdf_function_binding!(
        WdfObjectGetTypedContextWorker,
        handle,
        device_context_type_info()
    ) as *mut DeviceContext;
    debug_assert!(!p.is_null());
    p
}

/// EXCLUSIVE `&mut` to the inline [`DeviceContext`]. ONLY for the
/// PnP/cleanup-serialized callers — `evt_device_add` (stores the `Box`) and
/// `free_adapter` (nulls it). Must NOT be used on the concurrent (parallel
/// queue) IOCTL path, where overlapping `&mut` to the one inline context would
/// be undefined behavior; that path uses [`adapter_of`] instead.
///
/// # Safety
/// `handle` must be our WDFDEVICE, and the caller must hold the de-facto
/// exclusivity the PnP/cleanup lifecycle provides.
pub unsafe fn device_context_mut<'a>(handle: WDFOBJECT) -> &'a mut DeviceContext {
    &mut *device_context_ptr(handle)
}

/// Shared borrow of the live [`AdapterContext`] for a WDFDEVICE (or `None` if not
/// yet set). This is the concurrent IOCTL read path: it reads the `adapter`
/// pointer via a RAW read and never materializes a `&mut DeviceContext`, so
/// parallel callers do not form overlapping `&mut` aliases. The `adapter` field
/// is written only under PnP serialization (`evt_device_add` before the
/// interface is openable; `free_adapter` in cleanup after all IRPs drain), never
/// concurrently with live IOCTL dispatch.
///
/// # Safety
/// `handle` must be our WDFDEVICE. The returned reference is valid until the
/// device's `EvtCleanupCallback` frees the `Box`.
pub unsafe fn adapter_of<'a>(handle: WDFOBJECT) -> Option<&'a AdapterContext> {
    let p = device_context_ptr(handle);
    // Raw read of the field — no `&`/`&mut` to `*p` is materialized.
    let adapter = core::ptr::addr_of!((*p).adapter).read();
    if adapter.is_null() {
        None
    } else {
        Some(&*adapter)
    }
}

pub struct AdapterContext {
    /// Serializes ALL access to `virtio` (the control virtqueue + the shared
    /// scratch page). Held by IOCTL submissions at PASSIVE_LEVEL and, from
    /// Phase 4, by the used-ring DPC at DISPATCH_LEVEL — a spinlock (not a mutex)
    /// is mandatory because the DPC path cannot block. `0` is the initialized +
    /// unlocked state of a `KSPIN_LOCK`, so no explicit `KeInitializeSpinLock` is
    /// required (same rationale as the BAR-mapping cache in `virtio::hal`).
    virtio_lock: UnsafeCell<KSPIN_LOCK>,
    /// The virtio-gpu transport, brought up in `evt_device_prepare_hardware`.
    /// Guarded by `virtio_lock`; `None` until PrepareHardware (and after
    /// ReleaseHardware).
    virtio: UnsafeCell<Option<VirtioGpu>>,
    /// fence_id → KEVENT table for the async `IOCTL_HELIOS_WAIT_FENCE` path.
    /// Present and functional; wired to the used-ring DPC in Phase 4 (today's
    /// submit path is synchronous, so WAIT_FENCE completes trivially).
    #[allow(dead_code)]
    pub fences: FenceTable,
    /// Live host-visible blob mappings (resource_id → user VA + MDL), recorded by
    /// `IOCTL_HELIOS_MAP_BLOB` and drained by `EvtFileCleanup`. Lives here (not in
    /// `virtio`) so teardown survives transport release — see [`MappingTable`].
    pub mappings: MappingTable,
}

// SAFETY: `virtio` is interior-mutable but every access goes through
// `virtio_lock` (a kernel spinlock) via `with_virtio`/`set_virtio`, so concurrent
// IOCTL/DPC callers never alias it. `fences` is internally synchronized by its
// own spinlock. The `Box<AdapterContext>` is shared across the device's IOCTL,
// PnP, and interrupt callbacks via the WDF context pointer.
unsafe impl Send for AdapterContext {}
unsafe impl Sync for AdapterContext {}

impl AdapterContext {
    pub fn new() -> Self {
        Self {
            virtio_lock: UnsafeCell::new(0),
            virtio: UnsafeCell::new(None),
            fences: FenceTable::new(),
            mappings: MappingTable::new(),
        }
    }

    /// Install (or clear) the virtio transport under the lock.
    ///
    /// The previous transport, if any, is dropped *after* the lock is released:
    /// `VirtioGpu::drop` resets the device and frees contiguous memory, both of
    /// which are PASSIVE_LEVEL-only — they must not run at the DISPATCH_LEVEL the
    /// spinlock raises to. MUST be called at PASSIVE_LEVEL (PrepareHardware /
    /// ReleaseHardware, which the PnP manager serializes).
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
}

/// Free the heap `AdapterContext` a device owns. Called from the device's
/// `EvtCleanupCallback`.
///
/// # Safety
/// `device` must be our WDFDEVICE. Runs at PASSIVE_LEVEL (device cleanup). The
/// virtio transport must already have been torn down in ReleaseHardware so the
/// `Box` here frees only the (transport-empty) shell.
pub unsafe fn free_adapter(device: WDFOBJECT) {
    // Cleanup is serialized w.r.t. IOCTL dispatch (runs after all IRPs drain), so
    // the exclusive &mut is sound here.
    let ctx = device_context_mut(device);
    if !ctx.adapter.is_null() {
        // SAFETY: `adapter` was produced by Box::into_raw in evt_device_add and
        // is freed exactly once (here).
        drop(unsafe { Box::from_raw(ctx.adapter) });
        ctx.adapter = core::ptr::null_mut();
    }
}
