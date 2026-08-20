//! Device-owned HOC1 allocation and bounded immutable HOB1 extent allocator.
//!
//! There is exactly one pool per live D3D12 device.  It is a bare WDDM
//! allocation, locked once for the device lifetime and mapped at one stable
//! GPU virtual address.  The allocator never polls: a completed HQC1 value is
//! sampled once while looking at an extent; pressure is returned to the caller
//! as an exact context/value join which is performed with this lock dropped.

use core::ffi::c_void;
use core::ptr::NonNull;
use std::sync::{Mutex, MutexGuard};

use helios_protocol::{
    validate_pool_extent, HeliosOuterCommandAllocationV1, HELIOS_HOC1_BYTES,
    HELIOS_HOC1_EXTENT_ALIGNMENT, HELIOS_HOC1_MAX_LIVE_EXTENTS, HELIOS_HOC1_POOL_BYTES,
    HELIOS_PACKAGE_GENERATION,
};
use helios_umd_common::hr::{E_FAIL, S_OK};

use crate::{ddi12, log_error};

const E_PENDING: i32 = 0x8000_000au32 as i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExtentState {
    Reserved,
    Sealed,
    Submitted,
    Poisoned,
}

#[derive(Clone, Copy)]
struct LiveExtent {
    lease: u64,
    offset: u64,
    allocation_bytes: u64,
    hob1_bytes: u64,
    queue_context: usize,
    context_generation: u64,
    batch_id: u64,
    state: ExtentState,
    completed_cpu: Option<NonNull<u64>>,
    retire_value: u64,
}

struct PoolState {
    next_lease: u64,
    extents: Vec<LiveExtent>,
    poisoned: bool,
}

/// One reserved HOC1 extent.  Its process-local lease is bookkeeping only and
/// is never placed in HOB1, HOS1, or any host protocol.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoolReservation {
    pub(crate) lease: u64,
    pub(crate) cpu: NonNull<u8>,
    pub(crate) gpuva: u64,
    pub(crate) hob1_bytes: usize,
    context_generation: u64,
    batch_id: u64,
}

/// Pool pressure names the exact HQC1 owner/value which must complete before
/// another allocation attempt.  The queue module performs this join after the
/// allocator mutex has been released.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoolWait {
    pub(crate) queue_context: usize,
    pub(crate) retire_value: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PoolReserve {
    Reserved(PoolReservation),
    Wait(PoolWait),
    Refused,
}

/// The exact device-owned C65 pool and all handles needed for reverse teardown.
pub(crate) struct OuterCommandPool {
    h_rt_device: ddi12::HANDLE,
    callbacks: *const ddi12::D3DDDI_DEVICECALLBACKS,
    h_paging_queue: u32,
    h_paging_sync: u32,
    paging_fence_cpu: NonNull<u64>,
    h_allocation: u32,
    allocation_generation: u64,
    cpu: NonNull<u8>,
    gpuva: u64,
    locked: bool,
    va_reserved: bool,
    state: Mutex<PoolState>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

unsafe fn wait_paging_fence(
    h_rt_device: ddi12::HANDLE,
    callbacks: &ddi12::D3DDDI_DEVICECALLBACKS,
    h_sync: u32,
    completed_cpu: NonNull<u64>,
    required: u64,
) -> Result<(), i32> {
    if required == 0 || unsafe { completed_cpu.as_ptr().read_volatile() } >= required {
        return Ok(());
    }
    let Some(wait_cb) = callbacks.pfnWaitForSynchronizationObjectFromCpuCb else {
        return Err(E_FAIL);
    };
    let mut wait = ddi12::D3DDDICB_WAITFORSYNCHRONIZATIONOBJECTFROMCPU::default();
    wait.ObjectCount = 1;
    wait.ObjectHandleArray = &h_sync;
    wait.FenceValueArray = &required;
    // A null event is the runtime's synchronous, non-polling paging join.
    let hr = unsafe { wait_cb(h_rt_device, &wait) };
    if hr == S_OK && unsafe { completed_cpu.as_ptr().read_volatile() } >= required {
        Ok(())
    } else {
        Err(if hr < 0 { hr } else { E_FAIL })
    }
}

impl OuterCommandPool {
    /// Create the sole HOC1 allocation.  Every fallible stage unwinds in exact
    /// reverse order through `Drop`; no partially-created pool is published.
    pub(crate) unsafe fn create(
        h_rt_device: ddi12::HANDLE,
        callbacks: *const ddi12::D3DDDI_DEVICECALLBACKS,
    ) -> Result<Self, i32> {
        if callbacks.is_null() {
            return Err(E_FAIL);
        }
        let cb = unsafe { &*callbacks };
        let (
            Some(create_paging),
            Some(destroy_paging),
            Some(allocate),
            Some(deallocate),
            Some(lock2),
            Some(unlock2),
            Some(reserve_va),
            Some(map_va),
            Some(free_va),
            Some(make_resident),
        ) = (
            cb.pfnCreatePagingQueueCb,
            cb.pfnDestroyPagingQueueCb,
            cb.pfnAllocateCb,
            cb.pfnDeallocateCb,
            cb.pfnLock2Cb,
            cb.pfnUnlock2Cb,
            cb.pfnReserveGpuVirtualAddressCb,
            cb.pfnMapGpuVirtualAddressCb,
            cb.pfnFreeGpuVirtualAddressCb,
            cb.pfnMakeResidentCb,
        )
        else {
            return Err(E_FAIL);
        };
        let _ = (destroy_paging, deallocate, unlock2, free_va);

        let mut paging = ddi12::D3DDDICB_CREATEPAGINGQUEUE::default();
        paging.Priority = 0;
        paging.PhysicalAdapterIndex = 0;
        let hr = unsafe { create_paging(h_rt_device, &mut paging) };
        let Some(paging_fence_cpu) = NonNull::new(paging.FenceValueCPUVirtualAddress.cast()) else {
            return Err(if hr < 0 { hr } else { E_FAIL });
        };
        if hr < 0 || paging.hPagingQueue == 0 || paging.hSyncObject == 0 {
            if paging.hPagingQueue != 0 {
                let destroy = ddi12::D3DDDI_DESTROYPAGINGQUEUE {
                    hPagingQueue: paging.hPagingQueue,
                };
                let _ = unsafe { destroy_paging(h_rt_device, &destroy) };
            }
            return Err(if hr < 0 { hr } else { E_FAIL });
        }

        let mut pool = Self {
            h_rt_device,
            callbacks,
            h_paging_queue: paging.hPagingQueue,
            h_paging_sync: paging.hSyncObject,
            paging_fence_cpu,
            h_allocation: 0,
            allocation_generation: 0,
            cpu: NonNull::dangling(),
            gpuva: 0,
            locked: false,
            va_reserved: false,
            state: Mutex::new(PoolState {
                next_lease: 1,
                extents: Vec::new(),
                poisoned: false,
            }),
        };

        let mut hoc1 = HeliosOuterCommandAllocationV1::new(HELIOS_PACKAGE_GENERATION);
        let mut info = ddi12::D3DDDI_ALLOCATIONINFO::default();
        info.pPrivateDriverData = (&mut hoc1 as *mut HeliosOuterCommandAllocationV1).cast();
        info.PrivateDriverDataSize = u32::from(HELIOS_HOC1_BYTES);
        let mut arg = ddi12::D3DDDICB_ALLOCATE::default();
        arg.NumAllocations = 1;
        arg.__bindgen_anon_1.pAllocationInfo = &mut info;
        let hr = unsafe { allocate(h_rt_device, &mut arg) };
        if hr < 0
            || info.hAllocation == 0
            || hoc1
                .validate_create_output(HELIOS_PACKAGE_GENERATION)
                .is_err()
        {
            return Err(if hr < 0 { hr } else { E_FAIL });
        }
        pool.h_allocation = info.hAllocation;
        pool.allocation_generation = hoc1.allocation_generation;

        let mut lock_arg = ddi12::D3DDDICB_LOCK2::default();
        lock_arg.hAllocation = pool.h_allocation;
        let hr = unsafe { lock2(h_rt_device, &mut lock_arg) };
        let Some(cpu) = NonNull::new(lock_arg.pData.cast::<u8>()) else {
            return Err(if hr < 0 { hr } else { E_FAIL });
        };
        if hr < 0 {
            return Err(hr);
        }
        pool.cpu = cpu;
        pool.locked = true;

        let mut reserve = ddi12::D3DDDI_RESERVEGPUVIRTUALADDRESS::default();
        reserve.__bindgen_anon_1.hPagingQueue = pool.h_paging_queue;
        reserve.MinimumAddress = 0;
        reserve.MaximumAddress = u64::MAX;
        reserve.Size = HELIOS_HOC1_POOL_BYTES;
        reserve.__bindgen_anon_2.ReservationType =
            ddi12::_D3DDDIGPUVIRTUALADDRESS_RESERVATION_TYPE_D3DDDIGPUVIRTUALADDRESS_RESERVE_NO_ACCESS;
        let hr = unsafe { reserve_va(h_rt_device, &mut reserve) };
        let reserve_fence = unsafe { reserve.__bindgen_anon_4.PagingFenceValue };
        if hr < 0 || reserve.VirtualAddress == 0 {
            return Err(if hr < 0 { hr } else { E_FAIL });
        }
        pool.gpuva = reserve.VirtualAddress;
        pool.va_reserved = true;
        unsafe {
            wait_paging_fence(
                h_rt_device,
                cb,
                pool.h_paging_sync,
                pool.paging_fence_cpu,
                reserve_fence,
            )?
        };

        let Some(maximum) = pool.gpuva.checked_add(HELIOS_HOC1_POOL_BYTES - 1) else {
            return Err(E_FAIL);
        };
        let mut map = ddi12::D3DDDI_MAPGPUVIRTUALADDRESS::default();
        map.hPagingQueue = pool.h_paging_queue;
        map.BaseAddress = pool.gpuva;
        map.MinimumAddress = pool.gpuva;
        map.MaximumAddress = maximum;
        map.hAllocation = pool.h_allocation;
        map.SizeInPages = HELIOS_HOC1_POOL_BYTES / 4096;
        // Zero protection means GPU read-only.  HOB1 is never device-written.
        let hr = unsafe { map_va(h_rt_device, &mut map) };
        if hr < 0 || map.VirtualAddress != pool.gpuva {
            return Err(if hr < 0 { hr } else { E_FAIL });
        }
        unsafe {
            wait_paging_fence(
                h_rt_device,
                cb,
                pool.h_paging_sync,
                pool.paging_fence_cpu,
                map.PagingFenceValue,
            )?
        };

        let allocation = pool.h_allocation;
        let mut resident = ddi12::D3DDDI_MAKERESIDENT::default();
        resident.hPagingQueue = pool.h_paging_queue;
        resident.NumAllocations = 1;
        resident.AllocationList = &allocation;
        let hr = unsafe { make_resident(h_rt_device, &mut resident) };
        if hr != S_OK && hr != E_PENDING {
            return Err(hr);
        }
        unsafe {
            wait_paging_fence(
                h_rt_device,
                cb,
                pool.h_paging_sync,
                pool.paging_fence_cpu,
                resident.PagingFenceValue,
            )?
        };

        log_error!(
            "A7 D3D12 HOC1 created alloc=0x{:x} generation={} cpu={:p} gpuva=0x{:x}",
            pool.h_allocation,
            pool.allocation_generation,
            pool.cpu.as_ptr(),
            pool.gpuva,
        );
        Ok(pool)
    }

    /// Make every exact allocation named by the immutable HOB1 resident before
    /// `pfnSubmitCommandCb`.  The device paging queue supplies the sole
    /// completion domain; no private timeline or polling loop is introduced.
    pub(crate) unsafe fn make_resident(&self, allocations: &[u32]) -> Result<(), i32> {
        if allocations.is_empty() {
            return Ok(());
        }
        if allocations.iter().any(|handle| *handle == 0) || self.callbacks.is_null() {
            return Err(E_FAIL);
        }
        let cb = unsafe { &*self.callbacks };
        let Some(make_resident) = cb.pfnMakeResidentCb else {
            return Err(E_FAIL);
        };
        let Ok(count) = u32::try_from(allocations.len()) else {
            return Err(E_FAIL);
        };
        let mut arg = ddi12::D3DDDI_MAKERESIDENT::default();
        arg.hPagingQueue = self.h_paging_queue;
        arg.NumAllocations = count;
        arg.AllocationList = allocations.as_ptr();
        let hr = unsafe { make_resident(self.h_rt_device, &mut arg) };
        if hr != S_OK && hr != E_PENDING {
            return Err(hr);
        }
        unsafe {
            wait_paging_fence(
                self.h_rt_device,
                cb,
                self.h_paging_sync,
                self.paging_fence_cpu,
                arg.PagingFenceValue,
            )
        }
    }

    /// Give one UMD-owned standalone WDDM allocation an exact, bounded GPUVA.
    /// The returned extent is page-rounded only for VidMm bookkeeping; HRA1
    /// and allocation-use bounds continue to use the caller's exact byte size.
    pub(crate) unsafe fn map_internal_allocation(
        &self,
        h_allocation: u32,
        bytes: u64,
    ) -> Result<(u64, u64), i32> {
        const PAGE_BYTES: u64 = 4096;
        if h_allocation == 0 || bytes == 0 || self.callbacks.is_null() {
            return Err(E_FAIL);
        }
        let Some(reserved_bytes) = bytes
            .checked_add(PAGE_BYTES - 1)
            .map(|value| value & !(PAGE_BYTES - 1))
        else {
            return Err(E_FAIL);
        };
        let cb = unsafe { &*self.callbacks };
        let (Some(reserve_va), Some(map_va), Some(free_va)) = (
            cb.pfnReserveGpuVirtualAddressCb,
            cb.pfnMapGpuVirtualAddressCb,
            cb.pfnFreeGpuVirtualAddressCb,
        ) else {
            return Err(E_FAIL);
        };

        let mut reserve = ddi12::D3DDDI_RESERVEGPUVIRTUALADDRESS::default();
        reserve.__bindgen_anon_1.hPagingQueue = self.h_paging_queue;
        reserve.MaximumAddress = u64::MAX;
        reserve.Size = reserved_bytes;
        reserve.__bindgen_anon_2.ReservationType =
            ddi12::_D3DDDIGPUVIRTUALADDRESS_RESERVATION_TYPE_D3DDDIGPUVIRTUALADDRESS_RESERVE_NO_ACCESS;
        let hr = unsafe { reserve_va(self.h_rt_device, &mut reserve) };
        if hr < 0 || reserve.VirtualAddress == 0 {
            return Err(if hr < 0 { hr } else { E_FAIL });
        }
        let gpuva = reserve.VirtualAddress;
        if let Err(hr) = unsafe {
            wait_paging_fence(
                self.h_rt_device,
                cb,
                self.h_paging_sync,
                self.paging_fence_cpu,
                reserve.__bindgen_anon_4.PagingFenceValue,
            )
        } {
            let free = ddi12::D3DDDICB_FREEGPUVIRTUALADDRESS {
                BaseAddress: gpuva,
                Size: reserved_bytes,
            };
            let _ = unsafe { free_va(self.h_rt_device, &free) };
            return Err(hr);
        }

        let Some(maximum) = gpuva.checked_add(reserved_bytes - 1) else {
            let free = ddi12::D3DDDICB_FREEGPUVIRTUALADDRESS {
                BaseAddress: gpuva,
                Size: reserved_bytes,
            };
            let _ = unsafe { free_va(self.h_rt_device, &free) };
            return Err(E_FAIL);
        };
        let mut map = ddi12::D3DDDI_MAPGPUVIRTUALADDRESS::default();
        map.hPagingQueue = self.h_paging_queue;
        map.BaseAddress = gpuva;
        map.MinimumAddress = gpuva;
        map.MaximumAddress = maximum;
        map.hAllocation = h_allocation;
        map.SizeInPages = reserved_bytes / PAGE_BYTES;
        let hr = unsafe { map_va(self.h_rt_device, &mut map) };
        if hr < 0 || map.VirtualAddress != gpuva {
            let free = ddi12::D3DDDICB_FREEGPUVIRTUALADDRESS {
                BaseAddress: gpuva,
                Size: reserved_bytes,
            };
            let _ = unsafe { free_va(self.h_rt_device, &free) };
            return Err(if hr < 0 { hr } else { E_FAIL });
        }
        if let Err(hr) = unsafe {
            wait_paging_fence(
                self.h_rt_device,
                cb,
                self.h_paging_sync,
                self.paging_fence_cpu,
                map.PagingFenceValue,
            )
        } {
            let free = ddi12::D3DDDICB_FREEGPUVIRTUALADDRESS {
                BaseAddress: gpuva,
                Size: reserved_bytes,
            };
            let _ = unsafe { free_va(self.h_rt_device, &free) };
            return Err(hr);
        }
        if let Err(hr) = unsafe { self.make_resident(&[h_allocation]) } {
            let free = ddi12::D3DDDICB_FREEGPUVIRTUALADDRESS {
                BaseAddress: gpuva,
                Size: reserved_bytes,
            };
            let _ = unsafe { free_va(self.h_rt_device, &free) };
            return Err(hr);
        }
        Ok((gpuva, reserved_bytes))
    }

    /// Remove the exact VidMm mapping before its standalone WDDM allocation is
    /// deallocated. No lookup key or independent completion value is created.
    pub(crate) unsafe fn free_internal_gpuva(&self, gpuva: u64, bytes: u64) -> bool {
        if gpuva == 0 || bytes == 0 || self.callbacks.is_null() {
            return false;
        }
        let Some(free) = (unsafe { &*self.callbacks }).pfnFreeGpuVirtualAddressCb else {
            return false;
        };
        let arg = ddi12::D3DDDICB_FREEGPUVIRTUALADDRESS {
            BaseAddress: gpuva,
            Size: bytes,
        };
        (unsafe { free(self.h_rt_device, &arg) }) >= 0
    }

    /// Reserve a non-overlapping aligned extent or name one exact oldest HQC1
    /// completion to join.  No runtime or translator call occurs under `state`.
    pub(crate) fn reserve(
        &self,
        hob1_bytes: usize,
        queue_context: usize,
        context_generation: u64,
        batch_id: u64,
    ) -> PoolReserve {
        let Ok(hob1_bytes_u64) = u64::try_from(hob1_bytes) else {
            return PoolReserve::Refused;
        };
        if queue_context == 0 || context_generation == 0 || batch_id == 0 {
            return PoolReserve::Refused;
        }
        let Some(allocation_bytes) = hob1_bytes_u64
            .checked_add(u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT) - 1)
            .map(|v| v & !(u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT) - 1))
        else {
            return PoolReserve::Refused;
        };

        let mut state = lock(&self.state);
        if state.poisoned {
            return PoolReserve::Refused;
        }

        // One volatile sample per candidate, never a spin loop.
        state.extents.retain(|extent| {
            if extent.state != ExtentState::Submitted {
                return true;
            }
            let Some(completed) = extent.completed_cpu else {
                return true;
            };
            (unsafe { completed.as_ptr().read_volatile() }) < extent.retire_value
        });

        let live = match u32::try_from(state.extents.len()) {
            Ok(live) => live,
            Err(_) => return PoolReserve::Refused,
        };
        if live >= HELIOS_HOC1_MAX_LIVE_EXTENTS {
            return oldest_wait(&state).map_or(PoolReserve::Refused, PoolReserve::Wait);
        }

        state.extents.sort_unstable_by_key(|extent| extent.offset);
        let mut offset = 0u64;
        for extent in &state.extents {
            let Some(end) = offset.checked_add(allocation_bytes) else {
                return PoolReserve::Refused;
            };
            if end <= extent.offset {
                break;
            }
            let Some(next) = extent.offset.checked_add(extent.allocation_bytes) else {
                return PoolReserve::Refused;
            };
            let Some(aligned) = next
                .checked_add(u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT) - 1)
                .map(|v| v & !(u64::from(HELIOS_HOC1_EXTENT_ALIGNMENT) - 1))
            else {
                return PoolReserve::Refused;
            };
            offset = aligned;
        }
        if validate_pool_extent(offset, hob1_bytes_u64, live).is_err()
            || offset
                .checked_add(allocation_bytes)
                .is_none_or(|end| end > HELIOS_HOC1_POOL_BYTES)
        {
            return oldest_wait(&state).map_or(PoolReserve::Refused, PoolReserve::Wait);
        }

        let lease = state.next_lease;
        if lease == 0 || lease == u64::MAX {
            state.poisoned = true;
            return PoolReserve::Refused;
        }
        state.next_lease = lease + 1;
        state.extents.push(LiveExtent {
            lease,
            offset,
            allocation_bytes,
            hob1_bytes: hob1_bytes_u64,
            queue_context,
            context_generation,
            batch_id,
            state: ExtentState::Reserved,
            completed_cpu: None,
            retire_value: 0,
        });
        let Some(gpuva) = self.gpuva.checked_add(offset) else {
            state.extents.retain(|extent| extent.lease != lease);
            return PoolReserve::Refused;
        };
        let cpu = unsafe { NonNull::new_unchecked(self.cpu.as_ptr().add(offset as usize)) };
        PoolReserve::Reserved(PoolReservation {
            lease,
            cpu,
            gpuva,
            hob1_bytes,
            context_generation,
            batch_id,
        })
    }

    pub(crate) fn seal(&self, reservation: PoolReservation) -> bool {
        let mut state = lock(&self.state);
        let Some(extent) = state
            .extents
            .iter_mut()
            .find(|extent| extent.lease == reservation.lease)
        else {
            return false;
        };
        if extent.state != ExtentState::Reserved
            || extent.hob1_bytes != reservation.hob1_bytes as u64
            || extent.context_generation != reservation.context_generation
            || extent.batch_id != reservation.batch_id
        {
            return false;
        }
        extent.state = ExtentState::Sealed;
        true
    }

    pub(crate) fn submitted(
        &self,
        reservation: PoolReservation,
        completed_cpu: NonNull<u64>,
        retire_value: u64,
    ) -> bool {
        if retire_value == 0 {
            return false;
        }
        let mut state = lock(&self.state);
        let Some(extent) = state
            .extents
            .iter_mut()
            .find(|extent| extent.lease == reservation.lease)
        else {
            return false;
        };
        if extent.state != ExtentState::Sealed {
            return false;
        }
        extent.state = ExtentState::Submitted;
        extent.completed_cpu = Some(completed_cpu);
        extent.retire_value = retire_value;
        true
    }

    pub(crate) fn abandon(&self, lease: u64) {
        let mut state = lock(&self.state);
        state
            .extents
            .retain(|extent| extent.lease != lease || extent.state == ExtentState::Submitted);
    }

    pub(crate) fn poison(&self, lease: u64) {
        let mut state = lock(&self.state);
        state.poisoned = true;
        if let Some(extent) = state
            .extents
            .iter_mut()
            .find(|extent| extent.lease == lease)
        {
            extent.state = ExtentState::Poisoned;
        }
    }

    /// Called only after the queue's last HQC1 value has completed.  Removing
    /// all tuples precedes freeing the stable association object they name.
    pub(crate) fn purge_queue(&self, queue_context: usize) {
        lock(&self.state)
            .extents
            .retain(|extent| extent.queue_context != queue_context);
    }
}

fn oldest_wait(state: &PoolState) -> Option<PoolWait> {
    state
        .extents
        .iter()
        .filter(|extent| extent.state == ExtentState::Submitted && extent.retire_value != 0)
        .min_by_key(|extent| extent.lease)
        .map(|extent| PoolWait {
            queue_context: extent.queue_context,
            retire_value: extent.retire_value,
        })
}

impl Drop for OuterCommandPool {
    fn drop(&mut self) {
        if self.callbacks.is_null() {
            return;
        }
        let cb = unsafe { &*self.callbacks };
        if self.va_reserved && self.gpuva != 0 {
            if let Some(free) = cb.pfnFreeGpuVirtualAddressCb {
                let arg = ddi12::D3DDDICB_FREEGPUVIRTUALADDRESS {
                    BaseAddress: self.gpuva,
                    Size: HELIOS_HOC1_POOL_BYTES,
                };
                let _ = unsafe { free(self.h_rt_device, &arg) };
            }
        }
        if self.locked && self.h_allocation != 0 {
            if let Some(unlock) = cb.pfnUnlock2Cb {
                let arg = ddi12::D3DDDICB_UNLOCK2 {
                    hAllocation: self.h_allocation,
                };
                let _ = unsafe { unlock(self.h_rt_device, &arg) };
            }
        }
        if self.h_allocation != 0 {
            if let Some(deallocate) = cb.pfnDeallocateCb {
                let allocation = self.h_allocation;
                let arg = ddi12::D3DDDICB_DEALLOCATE {
                    hResource: core::ptr::null_mut::<c_void>(),
                    NumAllocations: 1,
                    HandleList: &allocation,
                };
                let _ = unsafe { deallocate(self.h_rt_device, &arg) };
            }
        }
        if self.h_paging_queue != 0 {
            if let Some(destroy) = cb.pfnDestroyPagingQueueCb {
                let arg = ddi12::D3DDDI_DESTROYPAGINGQUEUE {
                    hPagingQueue: self.h_paging_queue,
                };
                let _ = unsafe { destroy(self.h_rt_device, &arg) };
            }
        }
    }
}
